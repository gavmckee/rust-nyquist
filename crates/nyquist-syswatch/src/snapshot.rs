use std::collections::HashMap;
use nyquist_sysconfig::SysConfig;

pub struct Change {
    pub key:       String,
    pub old_value: String,
    pub new_value: String,
    pub pid:       u32,
    pub comm:      String,
}

/// Current known configuration state. Updated on every BPF/inotify event.
pub struct Snapshot {
    state: HashMap<String, String>,
}

impl Snapshot {
    /// Populate from a freshly collected SysConfig baseline.
    pub fn from_sysconfig(cfg: &SysConfig) -> Self {
        let mut state = HashMap::new();
        flatten_into(cfg, &mut state);
        Snapshot { state }
    }

    /// Merge a new SysConfig reading, returning only keys whose values changed.
    /// The pid/comm fields are attached from the triggering BPF event.
    pub fn diff_and_update(
        &mut self,
        cfg:  &SysConfig,
        pid:  u32,
        comm: &str,
    ) -> Vec<Change> {
        let mut fresh: HashMap<String, String> = HashMap::new();
        flatten_into(cfg, &mut fresh);

        let mut changes = Vec::new();
        for (k, new_v) in &fresh {
            match self.state.get(k) {
                Some(old_v) if old_v == new_v => {}
                Some(old_v) => changes.push(Change {
                    key:       k.clone(),
                    old_value: old_v.clone(),
                    new_value: new_v.clone(),
                    pid,
                    comm:      comm.to_string(),
                }),
                // First time seeing this key (new interface appeared) — no change record.
                None => {}
            }
        }
        // Absorb the new state.
        self.state = fresh;
        changes
    }
}

/// Flatten a SysConfig into (key, string_value) pairs using the same
/// key namespace as nyquist_clickhouse::config_watcher for compatibility.
fn flatten_into(cfg: &SysConfig, out: &mut HashMap<String, String>) {
    let s = &cfg.sysctl;
    out.insert("sysctl.tcp_rmem_max".into(),              s.tcp_rmem[2].to_string());
    out.insert("sysctl.tcp_wmem_max".into(),              s.tcp_wmem[2].to_string());
    out.insert("sysctl.rmem_max".into(),                  s.rmem_max.to_string());
    out.insert("sysctl.wmem_max".into(),                  s.wmem_max.to_string());
    out.insert("sysctl.netdev_max_backlog".into(),        s.netdev_max_backlog.to_string());
    out.insert("sysctl.tcp_slow_start_after_idle".into(), s.tcp_slow_start_after_idle.to_string());
    out.insert("sysctl.tcp_congestion_control".into(),    s.tcp_congestion_control.clone());

    for (iface, info) in &cfg.interfaces {
        out.insert(format!("ring.{iface}.rx"),           info.ring_rx.to_string());
        out.insert(format!("ring.{iface}.tx"),           info.ring_tx.to_string());
        out.insert(format!("ring.{iface}.rx_max"),       info.ring_rx_max.to_string());
        out.insert(format!("mtu.{iface}"),               info.mtu.to_string());
        out.insert(format!("channels.{iface}.rx"),       info.rx_queues.to_string());
        out.insert(format!("channels.{iface}.tx"),       info.tx_queues.to_string());
        out.insert(format!("channels.{iface}.combined"), info.combined_queues.to_string());
        out.insert(format!("rss.{iface}.table_size"),    info.rss_table_size.to_string());
        out.insert(format!("msix.{iface}.vectors"),      info.msix_vectors.to_string());
        out.insert(format!("coalesce.{iface}.rx_usecs"), info.coalesce_rx_usecs.to_string());
        out.insert(format!("coalesce.{iface}.tx_usecs"), info.coalesce_tx_usecs.to_string());
    }
}
