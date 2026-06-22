//! Software perf event sampler.
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

#[cfg(target_os = "linux")]
use perf_event::events::Software;
#[cfg(target_os = "linux")]
use crate::events::{PerfCounter, linux::PerfError};

#[cfg(target_os = "linux")]
const SW_EVENTS: &[(&str, Software)] = &[
    ("perf/sw/context_switches", Software::CONTEXT_SWITCHES),
    ("perf/sw/page_faults",      Software::PAGE_FAULTS),
    ("perf/sw/cpu_migrations",   Software::CPU_MIGRATIONS),
];

pub struct SoftwareSampler {
    interval: Duration,
    num_cpus: usize,
    #[cfg(target_os = "linux")]
    counters:    Vec<(MetricId, PerfCounter)>,
    #[cfg(target_os = "linux")]
    disabled:    bool,
    #[cfg(target_os = "linux")]
    initialized: bool,
}

impl SoftwareSampler {
    pub fn new(num_cpus: usize, interval: Duration) -> Self {
        SoftwareSampler {
            interval,
            num_cpus,
            #[cfg(target_os = "linux")]
            counters:    Vec::new(),
            #[cfg(target_os = "linux")]
            disabled:    false,
            #[cfg(target_os = "linux")]
            initialized: false,
        }
    }

    // Test helper: register + record synthetic readings without perf_event_open.
    pub fn ingest(&self, reg: &Registry, now: Instant, readings: &[(&str, &str, u64)]) {
        for &(metric_name, cpu_label, value) in readings {
            let labels = Labels::new().insert("cpu", cpu_label);
            let id = reg.register(
                MetricDef::new(metric_name, Kind::Counter)
                    .unit(Unit::Count)
                    .labels(labels),
            );
            reg.record_counter(id, now, value);
        }
    }

    #[cfg(target_os = "linux")]
    fn try_init(&mut self, reg: &Registry) -> Result<(), PerfError> {
        for cpu in 0..self.num_cpus {
            let cpu_label = format!("cpu{cpu}");
            for (name, event) in SW_EVENTS {
                let counter = PerfCounter::open(cpu, *event)?;
                let id = reg.register(
                    MetricDef::new(*name, Kind::Counter)
                        .unit(Unit::Count)
                        .labels(Labels::new().insert("cpu", &cpu_label)),
                );
                self.counters.push((id, counter));
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Sampler for SoftwareSampler {
    fn name(&self) -> &str { "perf_software" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        #[cfg(target_os = "linux")]
        {
            if self.disabled { return Ok(()); }
            if !self.initialized {
                self.initialized = true;
                if let Err(e) = self.try_init(reg) {
                    if matches!(e, PerfError::Permission) {
                        tracing::warn!(
                            "perf/sw disabled: {}. Run as root or: sudo sysctl kernel.perf_event_paranoid=1",
                            e
                        );
                        self.disabled = true;
                        return Ok(());
                    }
                    return Err(Box::new(e));
                }
            }
            for (id, counter) in &mut self.counters {
                let v = counter.read()?;
                reg.record_counter(*id, now, v);
            }
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
    fn ingest_registers_sw_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let sampler = SoftwareSampler::new(2, Duration::from_millis(10));
        let t0 = Instant::now();
        let r: Vec<(&str, &str, u64)> = vec![
            ("perf/sw/context_switches", "cpu0", 1000),
            ("perf/sw/page_faults",      "cpu0", 200),
            ("perf/sw/cpu_migrations",   "cpu0", 5),
            ("perf/sw/context_switches", "cpu1", 800),
            ("perf/sw/page_faults",      "cpu1", 100),
            ("perf/sw/cpu_migrations",   "cpu1", 3),
        ];
        sampler.ingest(&reg, t0, &r);
        assert_eq!(reg.metric_ids().len(), 6);
    }
}
