use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_proc_stat;

const FIELDS: [&str; 7] = ["user", "nice", "system", "idle", "iowait", "irq", "softirq"];

pub struct CpuSampler {
    interval: Duration,
    path: String,
    // ids[cpu_row][field_index]: populated on first sample; reused every subsequent tick.
    ids: Vec<[MetricId; 7]>,
}

impl CpuSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        CpuSampler { interval, path: "/proc/stat".to_string(), ids: Vec::new() }
    }

    fn ingest(&mut self, reg: &Registry, now: Instant, text: &str) {
        let cpus = parse_proc_stat(text);
        if self.ids.len() != cpus.len() {
            self.ids = cpus.iter().map(|(cpu, _)| {
                let mut row = [MetricId(0); 7];
                for (i, &field) in FIELDS.iter().enumerate() {
                    let labels = Labels::new().insert("cpu", cpu.as_str());
                    row[i] = reg.register(
                        MetricDef::new(format!("cpu/usage/{field}"), Kind::Counter)
                            .unit(Unit::Count)
                            .labels(labels),
                    );
                }
                row
            }).collect();
        }
        for (cpu_idx, (_, vals)) in cpus.iter().enumerate() {
            for (field_idx, &val) in vals.iter().enumerate() {
                reg.record_counter(self.ids[cpu_idx][field_idx], now, val);
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
        assert_eq!(reg.metric_ids().len(), 21);
    }
}
