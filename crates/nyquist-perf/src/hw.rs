//! Hardware perf event sampler.
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

#[cfg(target_os = "linux")]
use perf_event::events::Hardware;
#[cfg(target_os = "linux")]
use crate::events::{PerfCounter, linux::PerfError};

#[cfg(target_os = "linux")]
const HW_EVENTS: &[(&str, Hardware)] = &[
    ("perf/hw/cpu_cycles",          Hardware::CPU_CYCLES),
    ("perf/hw/instructions",        Hardware::INSTRUCTIONS),
    ("perf/hw/cache_references",    Hardware::CACHE_REFERENCES),
    ("perf/hw/cache_misses",        Hardware::CACHE_MISSES),
    ("perf/hw/branch_instructions", Hardware::BRANCH_INSTRUCTIONS),
    ("perf/hw/branch_misses",       Hardware::BRANCH_MISSES),
];

pub struct HardwareSampler {
    interval: Duration,
    num_cpus: usize,
    #[cfg(target_os = "linux")]
    counters:    Vec<(MetricId, PerfCounter)>,
    #[cfg(target_os = "linux")]
    disabled:    bool,
    #[cfg(target_os = "linux")]
    initialized: bool,
}

impl HardwareSampler {
    pub fn new(num_cpus: usize, interval: Duration) -> Self {
        HardwareSampler {
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
            for (name, event) in HW_EVENTS {
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
impl Sampler for HardwareSampler {
    fn name(&self) -> &str { "perf_hardware" }
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
                            "perf/hw disabled: {}. Run as root or: sudo sysctl kernel.perf_event_paranoid=1",
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
    fn ingest_registers_counters_and_records_rates() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let sampler = HardwareSampler::new(2, Duration::from_millis(10));
        let t0 = Instant::now();
        let r0: Vec<(&str, &str, u64)> = vec![
            ("perf/hw/cpu_cycles", "cpu0", 1_000_000),
            ("perf/hw/instructions", "cpu0", 2_000_000),
            ("perf/hw/cpu_cycles", "cpu1", 500_000),
            ("perf/hw/instructions", "cpu1", 1_000_000),
        ];
        sampler.ingest(&reg, t0, &r0);
        let r1: Vec<(&str, &str, u64)> = vec![
            ("perf/hw/cpu_cycles", "cpu0", 1_100_000),
            ("perf/hw/instructions", "cpu0", 2_200_000),
            ("perf/hw/cpu_cycles", "cpu1", 550_000),
            ("perf/hw/instructions", "cpu1", 1_100_000),
        ];
        sampler.ingest(&reg, t0 + Duration::from_millis(10), &r1);
        assert_eq!(reg.metric_ids().len(), 4);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn disabled_sampler_returns_ok_immediately() {
        let mut sampler = HardwareSampler::new(1, Duration::from_millis(10));
        sampler.disabled = true;
        sampler.initialized = true;
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(sampler.sample(&reg, Instant::now()));
        assert!(result.is_ok());
        assert_eq!(reg.metric_ids().len(), 0);
    }
}
