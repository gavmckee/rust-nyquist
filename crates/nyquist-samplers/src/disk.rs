use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_diskstats;

const SECTOR_BYTES: u64 = 512;

pub struct DiskSampler { interval: Duration, path: String }

impl DiskSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        DiskSampler { interval, path: "/proc/diskstats".to_string() }
    }
    fn ingest(&self, reg: &Registry, now: Instant, text: &str) {
        for (device, sread, swritten) in parse_diskstats(text) {
            let r = reg.register(
                MetricDef::new("disk/read/bytes", Kind::Counter)
                    .unit(Unit::Bytes).labels(Labels::new().insert("device", &device)));
            let w = reg.register(
                MetricDef::new("disk/write/bytes", Kind::Counter)
                    .unit(Unit::Bytes).labels(Labels::new().insert("device", &device)));
            reg.record_counter(r, now, sread * SECTOR_BYTES);
            reg.record_counter(w, now, swritten * SECTOR_BYTES);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for DiskSampler {
    fn name(&self) -> &str { "disk" }
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
    fn ingest_registers_read_write_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let s = DiskSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_diskstats");
        s.ingest(&reg, Instant::now(), text);
        assert_eq!(reg.metric_ids().len(), 4);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "disk",
    init: |reg, iv| Box::new(DiskSampler::new(reg, iv)),
};
