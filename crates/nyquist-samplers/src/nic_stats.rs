use std::collections::HashMap;
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

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
    ("buff_alloc_err","nic/queue/rx_buff_alloc_err", Unit::Count),
];
const TX_QUEUE_STAT_SUFFIXES: &[(&str, &str, Unit)] = &[
    ("packets", "nic/queue/tx_packets", Unit::Count),
    ("bytes",   "nic/queue/tx_bytes",   Unit::Bytes),
    ("dropped", "nic/queue/tx_dropped", Unit::Count),
];

struct QueueMetric {
    metric_name: String,
    unit: Unit,
    queue: u32,
    stat_idx: usize,
}

struct IfaceState {
    n_stats: usize,
    drop_stats: Vec<(String, usize)>,   // (metric_name_suffix, stat_idx)
    queue_metrics: Vec<QueueMetric>,
    // subset of queue_metrics that are rx_packets, for RSS CV
    rx_packet_idxs: Vec<(u32, usize)>,  // (queue, stat_idx)
    queue_prev: HashMap<u32, u64>,
}

pub struct NicStatsSampler {
    interval: Duration,
    state: HashMap<String, IfaceState>,
}

impl NicStatsSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        NicStatsSampler { interval, state: HashMap::new() }
    }

    fn sample_iface(&mut self, reg: &Registry, now: Instant, iface: &str) {
        let stats = nyquist_sysconfig::get_driver_stats(iface);
        if stats.is_empty() { return; }

        if self.state.get(iface).map(|s| s.n_stats) != Some(stats.len()) {
            self.state.insert(iface.to_string(), build_state(&stats));
        }
        let st = self.state.get_mut(iface).unwrap();

        // ── aggregate drop/miss/error counters ────────────────────────────────
        for (name, idx) in &st.drop_stats {
            if let Some((_, val)) = stats.get(*idx) {
                let id = reg.register(
                    MetricDef::new(name, Kind::Counter)
                        .unit(Unit::Count)
                        .labels(Labels::new().insert("iface", iface)),
                );
                reg.record_counter(id, now, *val);
            }
        }

        // ── per-queue metrics ─────────────────────────────────────────────────
        for qm in &st.queue_metrics {
            if let Some((_, val)) = stats.get(qm.stat_idx) {
                let q_str = qm.queue.to_string();
                let id = reg.register(
                    MetricDef::new(&qm.metric_name, Kind::Counter)
                        .unit(qm.unit)
                        .labels(Labels::new()
                            .insert("iface", iface)
                            .insert("queue", &q_str)),
                );
                reg.record_counter(id, now, *val);
            }
        }

        // ── RSS coefficient of variation (from rx_packets deltas) ─────────────
        let mut deltas: Vec<f64> = Vec::new();
        for &(q, idx) in &st.rx_packet_idxs {
            if let Some((_, cur)) = stats.get(idx) {
                let prev = st.queue_prev.get(&q).copied().unwrap_or(*cur);
                let delta = cur.saturating_sub(prev) as f64;
                st.queue_prev.insert(q, *cur);
                if delta > 0.0 { deltas.push(delta); }
            }
        }
        if !deltas.is_empty() {
            let cv = coefficient_of_variation(&deltas);
            let id = reg.register(
                MetricDef::new("nic/rss_cv", Kind::Gauge)
                    .unit(Unit::Percent)
                    .labels(Labels::new().insert("iface", iface)),
            );
            reg.record_gauge(id, now, (cv * 100.0) as u64);
        }

        // ── sysfs counters ────────────────────────────────────────────────────
        record_sysfs(reg, now, iface, "rx_missed_errors", "nic/rx_missed");
        record_sysfs(reg, now, iface, "rx_fifo_errors",   "nic/rx_fifo_errors");
        record_sysfs(reg, now, iface, "rx_frame_errors",  "nic/rx_frame_errors");
    }
}

#[async_trait::async_trait]
impl Sampler for NicStatsSampler {
    fn name(&self) -> &str { "nic_stats" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        for iface in list_physical_ifaces() {
            self.sample_iface(reg, now, &iface);
        }
        Ok(())
    }
}

fn build_state(stats: &[(String, u64)]) -> IfaceState {
    let mut drop_stats = Vec::new();
    let mut queue_metrics: Vec<QueueMetric> = Vec::new();
    let mut rx_packet_idxs = Vec::new();

    for (idx, (name, _)) in stats.iter().enumerate() {
        // Per-queue RX stats
        let mut matched_queue = false;
        for &(suffix, metric, unit) in QUEUE_STAT_SUFFIXES {
            if let Some(q) = parse_queue_stat(name, "rx", suffix) {
                if suffix == "packets" {
                    rx_packet_idxs.push((q, idx));
                }
                queue_metrics.push(QueueMetric {
                    metric_name: metric.to_string(),
                    unit,
                    queue: q,
                    stat_idx: idx,
                });
                matched_queue = true;
                break;
            }
        }
        if matched_queue { continue; }

        // Per-queue TX stats
        let mut matched_tx = false;
        for &(suffix, metric, unit) in TX_QUEUE_STAT_SUFFIXES {
            if let Some(q) = parse_queue_stat(name, "tx", suffix) {
                queue_metrics.push(QueueMetric {
                    metric_name: metric.to_string(),
                    unit,
                    queue: q,
                    stat_idx: idx,
                });
                matched_tx = true;
                break;
            }
        }
        if matched_tx { continue; }

        // Aggregate drop/miss stats
        if KEEP_PATTERNS.iter().any(|p| name.contains(p)) {
            let metric_name = format!("nic/driver/{}", sanitize(name));
            drop_stats.push((metric_name, idx));
        }
    }

    IfaceState {
        n_stats: stats.len(),
        drop_stats,
        queue_metrics,
        rx_packet_idxs,
        queue_prev: HashMap::new(),
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

fn coefficient_of_variation(vals: &[f64]) -> f64 {
    if vals.len() < 2 { return 0.0; }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    if mean < 1.0 { return 0.0; }
    let var = vals.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / vals.len() as f64;
    var.sqrt() / mean
}

fn record_sysfs(reg: &Registry, now: Instant, iface: &str, file: &str, metric: &str) {
    let path = format!("/sys/class/net/{iface}/statistics/{file}");
    let Ok(s) = std::fs::read_to_string(&path) else { return };
    let Ok(v) = s.trim().parse::<u64>() else { return };
    let id = reg.register(
        MetricDef::new(metric, Kind::Counter)
            .unit(Unit::Count)
            .labels(Labels::new().insert("iface", iface)),
    );
    reg.record_counter(id, now, v);
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
