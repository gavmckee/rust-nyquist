use std::collections::HashMap;
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_proc_stat;

const FIELDS: [&str; 8] = ["user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal"];

pub struct CpuSampler {
    interval: Duration,
    path: String,
    // Keyed by the cpu LABEL, not row position: the previous Vec was only
    // rebuilt when the row COUNT changed, so a same-count hotplug swap
    // (cpu2 offline while cpu5 comes online) recorded cpu5's counters
    // under cpu2's MetricIds — and the value jump read as a counter reset.
    ids: HashMap<String, [MetricId; 8]>,
}

impl CpuSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        CpuSampler { interval, path: "/proc/stat".to_string(), ids: HashMap::new() }
    }

    fn ingest(&mut self, reg: &Registry, now: Instant, text: &str) {
        for (cpu, vals) in parse_proc_stat(text) {
            let row = match self.ids.get(cpu.as_str()) {
                Some(r) => *r,
                None => {
                    let mut row = [MetricId(0); 8];
                    for (i, &field) in FIELDS.iter().enumerate() {
                        row[i] = reg.register(
                            MetricDef::new(format!("cpu/usage/{field}"), Kind::Counter)
                                .unit(Unit::Count)
                                .labels(Labels::new().insert("cpu", cpu.as_str())),
                        );
                    }
                    self.ids.insert(cpu.clone(), row);
                    row
                }
            };
            for (field_idx, &val) in vals.iter().enumerate() {
                reg.record_counter(row[field_idx], now, val);
            }
        }
    }
}

#[async_trait::async_trait]
impl Sampler for CpuSampler {
    fn name(&self) -> &str { "cpu" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(&self.path)?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_registers_and_records_per_cpu_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = CpuSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_stat");
        let t0 = Instant::now();
        s.ingest(&reg, t0, text);
        s.ingest(&reg, t0 + Duration::from_millis(10), text);
        // 3 cpu rows x 8 fields (incl. steal)
        assert_eq!(reg.metric_ids().len(), 24);
    }

    #[test]
    fn same_count_hotplug_swap_does_not_misattribute() {
        // cpu1 goes offline while cpu2 comes online: row count is unchanged,
        // so the old positional mapping recorded cpu2's counters under
        // cpu1's ids. Values must land under the label they belong to.
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = CpuSampler::new(&reg, Duration::from_millis(10));
        let t0 = Instant::now();
        s.ingest(&reg, t0, "cpu  1 0 0 0 0 0 0 0\ncpu0 10 0 0 0 0 0 0 0\ncpu1 20 0 0 0 0 0 0 0\n");
        s.ingest(&reg, t0 + Duration::from_millis(10),
                 "cpu  1 0 0 0 0 0 0 0\ncpu0 11 0 0 0 0 0 0 0\ncpu2 999 0 0 0 0 0 0 0\n");
        let id_for = |cpu: &str| reg.register(
            MetricDef::new("cpu/usage/user", Kind::Counter)
                .unit(Unit::Count)
                .labels(Labels::new().insert("cpu", cpu)),
        );
        // cpu2's reading is under cpu2, not smeared into cpu1's series.
        assert_eq!(reg.raw(id_for("cpu2")), 999);
        assert_eq!(reg.raw(id_for("cpu1")), 20, "offline cpu1 must retain its last value");
        assert_eq!(reg.raw(id_for("cpu0")), 11);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "cpu",
    init: |reg, iv| Box::new(CpuSampler::new(reg, iv)),
};
