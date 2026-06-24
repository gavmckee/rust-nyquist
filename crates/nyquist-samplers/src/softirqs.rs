use std::collections::HashMap;
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_softirqs;

pub struct SoftirqSampler {
    interval: Duration,
    ids: HashMap<String, MetricId>,
}

impl SoftirqSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        SoftirqSampler { interval, ids: HashMap::new() }
    }

    fn id(&mut self, reg: &Registry, name: &str) -> MetricId {
        if let Some(&id) = self.ids.get(name) { return id; }
        let id = reg.register(MetricDef::new(name, Kind::Counter).unit(Unit::Count));
        self.ids.insert(name.to_string(), id);
        id
    }
}

#[async_trait::async_trait]
impl Sampler for SoftirqSampler {
    fn name(&self) -> &str { "softirqs" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string("/proc/softirqs")?;
        for (irq_type, total) in parse_softirqs(&text) {
            let name = format!("softirq/{}", irq_type.to_lowercase());
            let id = self.id(reg, &name);
            reg.record_counter(id, now, total);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::time::{Duration, Instant};

    #[test]
    fn registers_softirq_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = SoftirqSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_softirqs");
        let entries = parse_softirqs(text);
        let now = Instant::now();
        for (irq_type, total) in entries {
            let name = format!("softirq/{}", irq_type.to_lowercase());
            let id = s.id(&reg, &name);
            reg.record_counter(id, now, total);
        }
        // NET_RX: 5000000 + 10000000 = 15000000
        let net_rx_id = s.id(&reg, "softirq/net_rx");
        assert_eq!(reg.raw(net_rx_id), 15_000_000);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "softirqs",
    init: |reg, iv| Box::new(SoftirqSampler::new(reg, iv)),
};
