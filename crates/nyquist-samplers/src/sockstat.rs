use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_sockstat;

pub struct SockstatSampler {
    interval: Duration,
    ids: Option<[MetricId; 4]>,
}

impl SockstatSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        SockstatSampler { interval, ids: None }
    }

    fn ingest(&mut self, reg: &Registry, now: Instant, text: &str) {
        let snap = parse_sockstat(text);
        let ids = self.ids.get_or_insert_with(|| [
            reg.register(MetricDef::new("net/sockets/tcp_inuse",  Kind::Gauge).unit(Unit::Count)),
            reg.register(MetricDef::new("net/sockets/tcp_tw",     Kind::Gauge).unit(Unit::Count)),
            reg.register(MetricDef::new("net/sockets/tcp_orphan", Kind::Gauge).unit(Unit::Count)),
            reg.register(MetricDef::new("net/sockets/udp_inuse",  Kind::Gauge).unit(Unit::Count)),
        ]);
        reg.record_gauge(ids[0], now, snap.tcp_inuse);
        reg.record_gauge(ids[1], now, snap.tcp_tw);
        reg.record_gauge(ids[2], now, snap.tcp_orphan);
        reg.record_gauge(ids[3], now, snap.udp_inuse);
    }
}

#[async_trait::async_trait]
impl Sampler for SockstatSampler {
    fn name(&self) -> &str { "sockstat" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string("/proc/net/sockstat")?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn ingest_registers_four_socket_gauges() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = SockstatSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_sockstat");
        s.ingest(&reg, Instant::now(), text);
        assert_eq!(reg.metric_ids().len(), 4);
        let ids = s.ids.unwrap();
        assert_eq!(reg.raw(ids[0]), 12);
        assert_eq!(reg.raw(ids[1]), 5);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "sockstat",
    init: |reg, iv| Box::new(SockstatSampler::new(reg, iv)),
};
