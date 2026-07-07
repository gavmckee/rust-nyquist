use std::collections::HashMap;
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use nyquist_sysconfig::StatsReader;

// Aggregate stats whose names contain these substrings are tracked as counters.
const KEEP_PATTERNS: &[&str] = &[
    "out_of_buffer",
    "discards_phy",
    "steer_missed",
    "rx_missed",
    "rx_fifo",
    "crc_error",
    "pci_signal",
];

// Per-queue stat suffixes to expose as individual metrics.
// Each generates nic/queue/{rx,tx}_{suffix}{iface, queue=N}.
const QUEUE_STAT_SUFFIXES: &[(&str, &str, Unit)] = &[
    ("packets",       "nic/queue/rx_packets",       Unit::Count),
    ("bytes",         "nic/queue/rx_bytes",          Unit::Bytes),
    ("dropped",       "nic/queue/rx_dropped",        Unit::Count),
    ("buff_alloc_err","nic/queue/rx_buff_alloc_err", Unit::Count),
];
const TX_QUEUE_STAT_SUFFIXES: &[(&str, &str, Unit)] = &[
    ("packets", "nic/queue/tx_packets", Unit::Count),
    ("bytes",   "nic/queue/tx_bytes",   Unit::Bytes),
    ("dropped", "nic/queue/tx_dropped", Unit::Count),
];

// Sysfs statistics files mirrored as counters.
const SYSFS_STATS: &[(&str, &str)] = &[
    ("rx_missed_errors", "nic/rx_missed"),
    ("rx_fifo_errors",   "nic/rx_fifo_errors"),
    ("rx_frame_errors",  "nic/rx_frame_errors"),
];

/// Everything registered up front so the per-tick path records by MetricId
/// with zero MetricDef/Labels/String allocations.
struct IfaceState {
    n_stats:        usize,
    drop_stats:     Vec<(MetricId, usize)>,      // (id, stat_idx)
    queue_metrics:  Vec<(MetricId, usize)>,      // (id, stat_idx)
    rx_packet_idxs: Vec<(u32, usize)>,           // (queue, stat_idx) for RSS CV
    queue_prev:     HashMap<u32, u64>,
    rss_cv_id:      MetricId,
    sysfs:          Vec<(String, MetricId)>,     // (path, id)
}

pub struct NicStatsSampler {
    interval: Duration,
    // One socket + scratch buffer for the process lifetime. None if the
    // socket can't be opened (non-Linux, exotic sandbox) — sampler idles.
    reader: Option<StatsReader>,
    // Reused values buffer: ~7k u64 per mlx5 port, refilled in place.
    values: Vec<u64>,
    ifaces: Vec<String>,
    ifaces_refreshed: Option<Instant>,
    state: HashMap<String, IfaceState>,
}

const IFACE_LIST_REFRESH: Duration = Duration::from_secs(60);

impl NicStatsSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        NicStatsSampler {
            interval,
            reader: StatsReader::new().ok(),
            values: Vec::new(),
            ifaces: Vec::new(),
            ifaces_refreshed: None,
            state: HashMap::new(),
        }
    }

    fn sample_iface(&mut self, reg: &Registry, now: Instant, iface: &str) {
        let Some(reader) = self.reader.as_mut() else { return };
        let got = reader.stat_values(iface, &mut self.values);
        if got == 0 { return; }

        if self.state.get(iface).map(|s| s.n_stats) != Some(got) {
            // Count changed (queue reconfig) or first sight: rebuild the
            // name→metric mapping. Skip recording this tick so values are
            // never paired against names from a different stat layout.
            let names = reader.stat_names(iface);
            if names.is_empty() { return; }
            self.state.insert(iface.to_string(), build_state(reg, iface, &names));
            return;
        }
        let st = self.state.get_mut(iface).unwrap();

        // ── aggregate drop/miss/error counters ────────────────────────────────
        for &(id, idx) in &st.drop_stats {
            if let Some(&val) = self.values.get(idx) {
                reg.record_counter(id, now, val);
            }
        }

        // ── per-queue metrics ─────────────────────────────────────────────────
        for &(id, idx) in &st.queue_metrics {
            if let Some(&val) = self.values.get(idx) {
                reg.record_counter(id, now, val);
            }
        }

        // ── RSS coefficient of variation (from rx_packets deltas) ─────────────
        let deltas = queue_deltas(&st.rx_packet_idxs, &self.values, &mut st.queue_prev);
        if let Some(cv) = coefficient_of_variation(&deltas) {
            reg.record_gauge(st.rss_cv_id, now, (cv * 100.0) as u64);
        }

        // ── sysfs counters ────────────────────────────────────────────────────
        for (path, id) in &st.sysfs {
            let Ok(s) = std::fs::read_to_string(path) else { continue };
            let Ok(v) = s.trim().parse::<u64>() else { continue };
            reg.record_counter(*id, now, v);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for NicStatsSampler {
    fn name(&self) -> &str { "nic_stats" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        if self.reader.is_none() { return Ok(()); }
        if self.ifaces_refreshed
            .is_none_or(|t| now.saturating_duration_since(t) >= IFACE_LIST_REFRESH)
        {
            self.ifaces = list_physical_ifaces();
            self.ifaces_refreshed = Some(now);
        }
        let ifaces = std::mem::take(&mut self.ifaces);
        for iface in &ifaces {
            self.sample_iface(reg, now, iface);
        }
        self.ifaces = ifaces;
        Ok(())
    }
}

fn build_state(reg: &Registry, iface: &str, names: &[String]) -> IfaceState {
    let mut drop_stats = Vec::new();
    let mut queue_metrics = Vec::new();
    let mut rx_packet_idxs = Vec::new();

    for (idx, name) in names.iter().enumerate() {
        // Per-queue RX stats
        let mut matched_queue = false;
        for &(suffix, metric, unit) in QUEUE_STAT_SUFFIXES {
            if let Some(q) = parse_queue_stat(name, "rx", suffix) {
                if suffix == "packets" {
                    rx_packet_idxs.push((q, idx));
                }
                let id = reg.register(
                    MetricDef::new(metric, Kind::Counter)
                        .unit(unit)
                        .labels(Labels::new()
                            .insert("iface", iface)
                            .insert("queue", q.to_string())),
                );
                queue_metrics.push((id, idx));
                matched_queue = true;
                break;
            }
        }
        if matched_queue { continue; }

        // Per-queue TX stats
        let mut matched_tx = false;
        for &(suffix, metric, unit) in TX_QUEUE_STAT_SUFFIXES {
            if let Some(q) = parse_queue_stat(name, "tx", suffix) {
                let id = reg.register(
                    MetricDef::new(metric, Kind::Counter)
                        .unit(unit)
                        .labels(Labels::new()
                            .insert("iface", iface)
                            .insert("queue", q.to_string())),
                );
                queue_metrics.push((id, idx));
                matched_tx = true;
                break;
            }
        }
        if matched_tx { continue; }

        // Aggregate drop/miss stats
        if KEEP_PATTERNS.iter().any(|p| name.contains(p)) {
            let id = reg.register(
                MetricDef::new(format!("nic/driver/{}", sanitize(name)), Kind::Counter)
                    .unit(Unit::Count)
                    .labels(Labels::new().insert("iface", iface)),
            );
            drop_stats.push((id, idx));
        }
    }

    let rss_cv_id = reg.register(
        MetricDef::new("nic/rss_cv", Kind::Gauge)
            .unit(Unit::Percent)
            .labels(Labels::new().insert("iface", iface)),
    );

    let sysfs = SYSFS_STATS.iter().map(|(file, metric)| {
        let id = reg.register(
            MetricDef::new(*metric, Kind::Counter)
                .unit(Unit::Count)
                .labels(Labels::new().insert("iface", iface)),
        );
        (format!("/sys/class/net/{iface}/statistics/{file}"), id)
    }).collect();

    IfaceState {
        n_stats: names.len(),
        drop_stats,
        queue_metrics,
        rx_packet_idxs,
        queue_prev: HashMap::new(),
        rss_cv_id,
        sysfs,
    }
}

// Matches rx{N}_{suffix} and rx_queue_{N}_{suffix}
fn parse_queue_stat(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let full_suffix = format!("_{suffix}");
    // rx{N}_suffix  (mlx5 style)
    if let Some(mid) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(&full_suffix)) {
        if !mid.is_empty() && mid.chars().all(|c| c.is_ascii_digit()) {
            return mid.parse().ok();
        }
    }
    // rx_queue_{N}_suffix  (ixgbe/i40e style)
    let queue_prefix = format!("{prefix}_queue_");
    if let Some(mid) = name.strip_prefix(&queue_prefix).and_then(|s| s.strip_suffix(&full_suffix)) {
        if !mid.is_empty() && mid.chars().all(|c| c.is_ascii_digit()) {
            return mid.parse().ok();
        }
    }
    None
}

/// Per-queue rx_packets deltas for this tick. A delta of ZERO from a known
/// baseline is real data — a starved queue — and must be included: excluding
/// idle queues made rss_cv read LOW precisely when one queue received
/// nothing (the pathology the metric exists to catch). Only queues seen for
/// the first time (no baseline yet) are skipped.
fn queue_deltas(
    rx_packet_idxs: &[(u32, usize)],
    values: &[u64],
    queue_prev: &mut HashMap<u32, u64>,
) -> Vec<f64> {
    let mut deltas = Vec::with_capacity(rx_packet_idxs.len());
    for &(q, idx) in rx_packet_idxs {
        if let Some(&cur) = values.get(idx) {
            if let Some(prev) = queue_prev.insert(q, cur) {
                deltas.push(cur.saturating_sub(prev) as f64);
            }
        }
    }
    deltas
}

/// None when a CV would be meaningless: fewer than two queues, or below
/// ~1 packet/queue/tick where idle queues are coupon-collector noise, not
/// imbalance (the old code returned a misleading 0.0 there).
fn coefficient_of_variation(vals: &[f64]) -> Option<f64> {
    if vals.len() < 2 { return None; }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    if mean < 1.0 { return None; }
    let var = vals.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / vals.len() as f64;
    Some(var.sqrt() / mean)
}

fn sanitize(name: &str) -> String {
    name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
}

fn list_physical_ifaces() -> Vec<String> {
    std::fs::read_dir("/sys/class/net")
        .map(|d| d.filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            if matches!(name.as_str(), "lo") { return None; }
            if name.starts_with("veth") || name.starts_with("docker")
               || name.starts_with("virbr") { return None; }
            Some(name)
        }).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_stat_name_parsing() {
        assert_eq!(parse_queue_stat("rx3_packets", "rx", "packets"), Some(3));
        assert_eq!(parse_queue_stat("rx_queue_12_bytes", "rx", "bytes"), Some(12));
        assert_eq!(parse_queue_stat("tx0_dropped", "tx", "dropped"), Some(0));
        assert_eq!(parse_queue_stat("rx_packets", "rx", "packets"), None);
        assert_eq!(parse_queue_stat("rx3_bytes", "rx", "packets"), None);
    }

    #[test]
    fn starved_queue_raises_cv_instead_of_vanishing() {
        let idxs = vec![(0u32, 0usize), (1, 1), (2, 2), (3, 3)];
        let mut prev = HashMap::new();
        // First tick: baselines only, no deltas.
        assert!(queue_deltas(&idxs, &[100, 100, 100, 100], &mut prev).is_empty());
        // Second tick: queues 0-2 receive 100 packets each; queue 3 is STARVED.
        let deltas = queue_deltas(&idxs, &[200, 200, 200, 100], &mut prev);
        assert_eq!(deltas, vec![100.0, 100.0, 100.0, 0.0]);
        let cv = coefficient_of_variation(&deltas).unwrap();
        // Old behavior dropped the zero and reported CV = 0 (perfect balance).
        assert!(cv > 0.5, "starved queue must raise CV, got {cv}");
        // Balanced traffic still reads ~0.
        let balanced = coefficient_of_variation(&[100.0, 100.0, 100.0, 100.0]).unwrap();
        assert!(balanced < 0.01);
    }

    #[test]
    fn cv_is_withheld_when_meaningless() {
        // Below ~1 pkt/queue/tick: coupon-collector noise, not imbalance.
        assert!(coefficient_of_variation(&[0.0, 0.0, 1.0, 0.0]).is_none());
        // Single queue: no distribution to speak of.
        assert!(coefficient_of_variation(&[500.0]).is_none());
        assert!(coefficient_of_variation(&[]).is_none());
    }

    #[test]
    fn build_state_registers_ids_once() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let names: Vec<String> = vec![
            "rx0_packets".into(),
            "rx0_bytes".into(),
            "tx0_bytes".into(),
            "rx_out_of_buffer".into(),
            "irrelevant_stat".into(),
        ];
        let st = build_state(&reg, "eth0", &names);
        assert_eq!(st.n_stats, 5);
        assert_eq!(st.queue_metrics.len(), 3);
        assert_eq!(st.drop_stats.len(), 1);
        assert_eq!(st.rx_packet_idxs, vec![(0, 0)]);
        // queue metrics (3) + drop (1) + rss_cv + 3 sysfs = 8 registered ids
        assert_eq!(reg.metric_ids().len(), 8);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "nic_stats",
    init: |reg, iv| Box::new(NicStatsSampler::new(reg, iv)),
};
