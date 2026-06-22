//! Polls sysconfig (ring params, MTU, sysctls) on a fixed interval.
//!
//! Every poll:
//!   - Inserts all numeric values to `sysconfig_values`  (for plotting)
//!   - On first change: inserts a row to `sysconfig_changes` (Grafana reads
//!     this table as an annotation source — vertical lines on every panel)
//!
//! Grafana annotation query:
//!   SELECT ts as time, note as text, key as tags
//!   FROM nyquist.sysconfig_changes
//!   WHERE $__timeFilter(ts)
//!   ORDER BY ts

use std::collections::HashMap;
use std::time::{Duration, UNIX_EPOCH};
use clickhouse::Client;

pub struct ConfigWatcher {
    client:   Client,
    database: String,
    interval: Duration,
}

impl ConfigWatcher {
    pub fn new(
        url:      &str,
        database: &str,
        username: &str,
        password: &str,
        interval: Duration,
    ) -> Self {
        let client = Client::default()
            .with_url(url)
            .with_database(database)
            .with_user(username)
            .with_password(password);
        ConfigWatcher { client, database: database.to_string(), interval }
    }

    pub async fn run(self) {
        let mut prev: HashMap<String, String> = HashMap::new();
        let mut ready = false;

        loop {
            if !ready {
                match self.ensure_tables().await {
                    Ok(()) => ready = true,
                    Err(e) => {
                        tracing::warn!(error = %e, "config_watcher: table init failed, retrying");
                        tokio::time::sleep(self.interval).await;
                        continue;
                    }
                }
            }

            let current = match tokio::task::spawn_blocking(collect_config).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "config_watcher: collect failed");
                    tokio::time::sleep(self.interval).await;
                    continue;
                }
            };

            let ts_ms = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;

            let host = hostname();

            // Detect changes (skip on the very first poll — no baseline yet).
            if !prev.is_empty() {
                let changes: Vec<_> = current.iter()
                    .filter(|(k, v)| prev.get(k.as_str()).map(|old| old.as_str() != v.as_str()).unwrap_or(false))
                    .map(|(k, v)| (k.clone(), prev[k.as_str()].clone(), v.clone()))
                    .collect();

                if !changes.is_empty() {
                    tracing::info!(
                        count = changes.len(),
                        keys  = %changes.iter().map(|(k,_,_)| k.as_str()).collect::<Vec<_>>().join(", "),
                        "config_watcher: change detected"
                    );
                    if let Err(e) = self.insert_changes(ts_ms, &host, &changes).await {
                        tracing::warn!(error = %e, "config_watcher: changes insert failed");
                    }
                }
            }

            if let Err(e) = self.insert_values(ts_ms, &host, &current).await {
                tracing::warn!(error = %e, "config_watcher: values insert failed");
            }

            prev = current.into_iter().collect();
            tokio::time::sleep(self.interval).await;
        }
    }

    // ── DDL ──────────────────────────────────────────────────────────────────

    async fn ensure_tables(&self) -> Result<(), clickhouse::error::Error> {
        self.client.query(&format!(
            "CREATE TABLE IF NOT EXISTS {db}.sysconfig_values (\
                ts       DateTime64(3, 'UTC'),\
                hostname LowCardinality(String),\
                key      LowCardinality(String),\
                value    Float64\
            ) ENGINE = MergeTree()\
            PARTITION BY toDate(ts)\
            ORDER BY (key, ts)\
            TTL toDate(ts) + INTERVAL 30 DAY",
            db = self.database,
        )).execute().await?;

        self.client.query(&format!(
            "CREATE TABLE IF NOT EXISTS {db}.sysconfig_changes (\
                ts        DateTime64(3, 'UTC'),\
                hostname  LowCardinality(String),\
                key       LowCardinality(String),\
                old_value String,\
                new_value String,\
                note      String\
            ) ENGINE = MergeTree()\
            PARTITION BY toDate(ts)\
            ORDER BY (ts, key)",
            db = self.database,
        )).execute().await?;

        tracing::info!(database = %self.database, "config_watcher: tables ready");
        Ok(())
    }

    // ── inserts ───────────────────────────────────────────────────────────────

    async fn insert_values(
        &self,
        ts_ms: i64,
        host:  &str,
        config: &[(String, String)],
    ) -> Result<(), clickhouse::error::Error> {
        let rows: Vec<_> = config.iter()
            .filter_map(|(k, v)| v.parse::<f64>().ok().map(|n| (k.as_str(), n)))
            .collect();
        if rows.is_empty() { return Ok(()); }

        let mut sql = format!(
            "INSERT INTO {}.sysconfig_values (ts, hostname, key, value) VALUES ",
            self.database
        );
        for (i, (key, val)) in rows.iter().enumerate() {
            if i > 0 { sql.push(','); }
            sql.push_str(&format!(
                "(fromUnixTimestamp64Milli({ts_ms}, 'UTC'), '{}', '{}', {val})",
                esc(host), esc(key),
            ));
        }
        self.client.query(&sql).execute().await
    }

    async fn insert_changes(
        &self,
        ts_ms:   i64,
        host:    &str,
        changes: &[(String, String, String)],
    ) -> Result<(), clickhouse::error::Error> {
        let mut sql = format!(
            "INSERT INTO {}.sysconfig_changes \
             (ts, hostname, key, old_value, new_value, note) VALUES ",
            self.database
        );
        for (i, (key, old, new)) in changes.iter().enumerate() {
            if i > 0 { sql.push(','); }
            let note = fmt_note(key, old, new);
            sql.push_str(&format!(
                "(fromUnixTimestamp64Milli({ts_ms}, 'UTC'), '{}', '{}', '{}', '{}', '{}')",
                esc(host), esc(key), esc(old), esc(new), esc(&note),
            ));
        }
        self.client.query(&sql).execute().await
    }
}

// ── config collection ─────────────────────────────────────────────────────────

/// Flatten SysConfig into `(key, string_value)` pairs.
/// Called via `spawn_blocking` since the underlying ioctls are synchronous.
fn collect_config() -> Vec<(String, String)> {
    let cfg = nyquist_sysconfig::collect();
    let mut out: Vec<(String, String)> = Vec::new();

    let s = &cfg.sysctl;
    out.push(("sysctl.tcp_rmem_max".into(),              s.tcp_rmem[2].to_string()));
    out.push(("sysctl.tcp_wmem_max".into(),              s.tcp_wmem[2].to_string()));
    out.push(("sysctl.rmem_max".into(),                  s.rmem_max.to_string()));
    out.push(("sysctl.wmem_max".into(),                  s.wmem_max.to_string()));
    out.push(("sysctl.netdev_max_backlog".into(),        s.netdev_max_backlog.to_string()));
    out.push(("sysctl.tcp_slow_start_after_idle".into(), s.tcp_slow_start_after_idle.to_string()));
    out.push(("sysctl.tcp_congestion_control".into(),    s.tcp_congestion_control.clone()));

    for (iface, info) in &cfg.interfaces {
        out.push((format!("ring.{iface}.rx"),             info.ring_rx.to_string()));
        out.push((format!("ring.{iface}.tx"),             info.ring_tx.to_string()));
        out.push((format!("ring.{iface}.rx_max"),         info.ring_rx_max.to_string()));
        out.push((format!("mtu.{iface}"),                 info.mtu.to_string()));
        out.push((format!("channels.{iface}.combined"),   info.combined_queues.to_string()));
        out.push((format!("coalesce.{iface}.rx_usecs"),   info.coalesce_rx_usecs.to_string()));
        out.push((format!("coalesce.{iface}.tx_usecs"),   info.coalesce_tx_usecs.to_string()));
    }

    out
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Human-readable annotation text for a config change.
fn fmt_note(key: &str, old: &str, new: &str) -> String {
    let parts: Vec<&str> = key.splitn(3, '.').collect();
    match parts.as_slice() {
        // Memory-related sysctls: show bytes and MB
        ["sysctl", param, ..] if param.contains("mem") => {
            let fmt_b = |s: &str| -> String {
                s.parse::<u64>()
                    .map(|n| format!("{s} ({:.0}MB)", n as f64 / 1_000_000.0))
                    .unwrap_or_else(|_| s.to_string())
            };
            format!("{key}: {} → {}", fmt_b(old), fmt_b(new))
        }
        // Ring descriptor counts
        ["ring", iface, param] => {
            format!("ring.{param}: {old} → {new}  ({iface})")
        }
        // MTU
        ["mtu", iface] => format!("MTU: {old} → {new}  ({iface})"),
        // Coalesce
        ["coalesce", iface, param] => {
            format!("coalesce.{param}: {old} → {new}  ({iface})")
        }
        // Channel count
        ["channels", iface, "combined"] => {
            format!("channels.combined: {old} → {new}  ({iface})")
        }
        _ => format!("{key}: {old} → {new}"),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_note_ring() {
        assert_eq!(
            fmt_note("ring.ens1f1np1.rx", "1024", "8192"),
            "ring.rx: 1024 → 8192  (ens1f1np1)"
        );
    }

    #[test]
    fn fmt_note_mtu() {
        assert_eq!(
            fmt_note("mtu.ens1f1np1", "1500", "9000"),
            "MTU: 1500 → 9000  (ens1f1np1)"
        );
    }

    #[test]
    fn fmt_note_mem_sysctl() {
        let note = fmt_note("sysctl.tcp_rmem_max", "6291456", "16777216");
        assert!(note.contains("6MB"), "expected MB annotation: {note}");
        assert!(note.contains("17MB"), "expected MB annotation: {note}");
    }

    #[test]
    fn fmt_note_fallback() {
        assert_eq!(
            fmt_note("sysctl.tcp_slow_start_after_idle", "1", "0"),
            "sysctl.tcp_slow_start_after_idle: 1 → 0"
        );
    }
}
