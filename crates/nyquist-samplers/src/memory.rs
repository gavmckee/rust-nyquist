use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_meminfo;

pub struct MemorySampler { interval: Duration, path: String }

impl MemorySampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        MemorySampler { interval, path: "/proc/meminfo".to_string() }
    }
    fn ingest(&self, reg: &Registry, now: Instant, text: &str) {
        for (key, bytes) in parse_meminfo(text) {
            let name = format!("memory/{}", key.to_lowercase());
            let id = reg.register(MetricDef::new(name, Kind::Gauge).unit(Unit::Bytes));
            reg.record_gauge(id, now, bytes);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for MemorySampler {
    fn name(&self) -> &str { "memory" }
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
    fn ingest_registers_gauges() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let s = MemorySampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_meminfo");
        s.ingest(&reg, Instant::now(), text);
        assert_eq!(reg.metric_ids().len(), 4);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "memory",
    init: |reg, iv| Box::new(MemorySampler::new(reg, iv)),
};
