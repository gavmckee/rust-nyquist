use std::collections::HashMap;
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_net_dev;

pub struct NetworkSampler {
    interval: Duration,
    path: String,
    // (iface, driver, mtu_str) -> [rx_bytes, rx_errors, rx_dropped, tx_bytes, tx_errors, tx_dropped]
    ids: HashMap<(String, String, String), [MetricId; 6]>,
    // iface -> (driver, mtu) — refreshed every 60s via ethtool
    context: HashMap<String, (String, u32)>,
    // None = never loaded; Some(t) = absolute time of next refresh
    context_next: Option<Instant>,
}

impl NetworkSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        NetworkSampler {
            interval,
            path: "/proc/net/dev".to_string(),
            ids: HashMap::new(),
            context: HashMap::new(),
            context_next: None,
        }
    }

    fn refresh_context(&mut self, now: Instant) {
        if self.context_next.map(|t| now < t).unwrap_or(false) { return; }
        self.context = nyquist_sysconfig::collect_interfaces()
            .into_iter()
            .map(|(name, b)| (name, (b.driver, b.mtu)))
            .collect();
        self.context_next = Some(now + Duration::from_secs(60));
    }

    fn ingest(&mut self, reg: &Registry, now: Instant, text: &str) {
        self.refresh_context(now);

        for e in parse_net_dev(text) {
            let (driver, mtu) = self.context
                .get(&e.iface)
                .map(|(d, m)| (d.clone(), m.to_string()))
                .unwrap_or_default();

            let key = (e.iface.clone(), driver.clone(), mtu.clone());
            let ids = self.ids.entry(key).or_insert_with(|| {
                let lbl = || Labels::new()
                    .insert("iface",  e.iface.as_str())
                    .insert("driver", driver.as_str())
                    .insert("mtu",    mtu.as_str());
                [
                    reg.register(MetricDef::new("network/receive/bytes",    Kind::Counter).unit(Unit::Bytes).labels(lbl())),
                    reg.register(MetricDef::new("network/receive/errors",   Kind::Counter).unit(Unit::Count).labels(lbl())),
                    reg.register(MetricDef::new("network/receive/dropped",  Kind::Counter).unit(Unit::Count).labels(lbl())),
                    reg.register(MetricDef::new("network/transmit/bytes",   Kind::Counter).unit(Unit::Bytes).labels(lbl())),
                    reg.register(MetricDef::new("network/transmit/errors",  Kind::Counter).unit(Unit::Count).labels(lbl())),
                    reg.register(MetricDef::new("network/transmit/dropped", Kind::Counter).unit(Unit::Count).labels(lbl())),
                ]
            });

            reg.record_counter(ids[0], now, e.rx_bytes);
            reg.record_counter(ids[1], now, e.rx_errors);
            reg.record_counter(ids[2], now, e.rx_dropped);
            reg.record_counter(ids[3], now, e.tx_bytes);
            reg.record_counter(ids[4], now, e.tx_errors);
            reg.record_counter(ids[5], now, e.tx_dropped);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for NetworkSampler {
    fn name(&self) -> &str { "network" }
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
    fn ingest_registers_rx_tx_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = NetworkSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_net_dev");
        // context_next = None so refresh runs; on CI with no real NICs the
        // context map stays empty and labels fall back to empty strings — fine.
        s.ingest(&reg, Instant::now(), text);
        // 2 interfaces × 6 metrics
        assert_eq!(reg.metric_ids().len(), 12);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "network",
    init: |reg, iv| Box::new(NetworkSampler::new(reg, iv)),
};
