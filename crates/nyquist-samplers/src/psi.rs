use std::collections::HashMap;
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_psi;

const RESOURCES: &[(&str, &str)] = &[
    ("cpu",    "/proc/pressure/cpu"),
    ("memory", "/proc/pressure/memory"),
    ("io",     "/proc/pressure/io"),
];

pub struct PsiSampler {
    interval: Duration,
    ids: HashMap<String, MetricId>,
}

impl PsiSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        PsiSampler { interval, ids: HashMap::new() }
    }

    fn id(&mut self, reg: &Registry, name: &str) -> MetricId {
        if let Some(&id) = self.ids.get(name) { return id; }
        let id = reg.register(MetricDef::new(name, Kind::Gauge).unit(Unit::None));
        self.ids.insert(name.to_string(), id);
        id
    }

    fn push_resource(&mut self, reg: &Registry, now: Instant, resource: &str, text: &str) {
        let snap = parse_psi(text);
        let id = self.id(reg, &format!("psi/{resource}/some_avg10"));
        reg.record_gauge(id, now, snap.some_avg10);
        let id = self.id(reg, &format!("psi/{resource}/some_avg60"));
        reg.record_gauge(id, now, snap.some_avg60);
        let id = self.id(reg, &format!("psi/{resource}/some_avg300"));
        reg.record_gauge(id, now, snap.some_avg300);
        if snap.has_full {
            let id = self.id(reg, &format!("psi/{resource}/full_avg10"));
            reg.record_gauge(id, now, snap.full_avg10);
            let id = self.id(reg, &format!("psi/{resource}/full_avg60"));
            reg.record_gauge(id, now, snap.full_avg60);
            let id = self.id(reg, &format!("psi/{resource}/full_avg300"));
            reg.record_gauge(id, now, snap.full_avg300);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for PsiSampler {
    fn name(&self) -> &str { "psi" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        for (resource, path) in RESOURCES {
            match std::fs::read_to_string(path) {
                Ok(text) => self.push_resource(reg, now, resource, &text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Box::new(e)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn push_resource_registers_some_only_when_no_full() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = PsiSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_pressure_cpu");
        s.push_resource(&reg, Instant::now(), "cpu", text);
        assert_eq!(reg.metric_ids().len(), 3);
    }

    #[test]
    fn push_resource_registers_some_and_full() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = PsiSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_pressure_memory");
        s.push_resource(&reg, Instant::now(), "memory", text);
        assert_eq!(reg.metric_ids().len(), 6);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "psi",
    init: |reg, iv| Box::new(PsiSampler::new(reg, iv)),
};
