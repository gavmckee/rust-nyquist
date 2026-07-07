use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::ProcReader;
use crate::procfs::parse_loadavg;

pub struct LoadAvgSampler {
    interval: Duration,
    reader: ProcReader,
    ids: Option<[MetricId; 3]>,
}

impl LoadAvgSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        LoadAvgSampler { interval, reader: ProcReader::new("/proc/loadavg"), ids: None }
    }

    fn ingest(&mut self, reg: &Registry, now: Instant, text: &str) {
        let Some((l1, l5, l15)) = parse_loadavg(text) else { return };
        let ids = self.ids.get_or_insert_with(|| [
            reg.register(MetricDef::new("cpu/load/load1",  Kind::Gauge).unit(Unit::None)),
            reg.register(MetricDef::new("cpu/load/load5",  Kind::Gauge).unit(Unit::None)),
            reg.register(MetricDef::new("cpu/load/load15", Kind::Gauge).unit(Unit::None)),
        ]);
        reg.record_gauge(ids[0], now, l1);
        reg.record_gauge(ids[1], now, l5);
        reg.record_gauge(ids[2], now, l15);
    }
}

#[async_trait::async_trait]
impl Sampler for LoadAvgSampler {
    fn name(&self) -> &str { "loadavg" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = self.reader.read()?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn ingest_registers_three_load_gauges() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = LoadAvgSampler::new(&reg, Duration::from_millis(10));
        s.ingest(&reg, Instant::now(), "0.15 0.45 1.20 2/312 98765\n");
        assert_eq!(reg.metric_ids().len(), 3);
        let ids = s.ids.unwrap();
        assert_eq!(reg.raw(ids[0]), 15);
        assert_eq!(reg.raw(ids[1]), 45);
        assert_eq!(reg.raw(ids[2]), 120);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "loadavg",
    init: |reg, iv| Box::new(LoadAvgSampler::new(reg, iv)),
};
