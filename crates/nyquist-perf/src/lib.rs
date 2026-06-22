//! perf_event_open samplers for nyquist.
pub mod events;
pub mod hw;
pub mod sw;

use std::time::Duration;
use nyquist_core::registry::Registry;
use nyquist_core::sampler::Sampler;

pub struct PerfConfig {
    pub enabled:    bool,
    pub max_cpus:   usize,
    pub hw_enabled: bool,
    pub sw_enabled: bool,
}

impl Default for PerfConfig {
    fn default() -> Self {
        PerfConfig { enabled: false, max_cpus: 32, hw_enabled: true, sw_enabled: true }
    }
}

pub fn build_perf_enabled(
    _reg: &Registry,
    default_interval: Duration,
    cfg: &PerfConfig,
) -> Vec<Box<dyn Sampler>> {
    if !cfg.enabled { return Vec::new(); }
    let mut out: Vec<Box<dyn Sampler>> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let ncpus = events::num_cpus();
        let effective = if cfg.max_cpus == 0 { ncpus } else { cfg.max_cpus.min(ncpus) };
        if cfg.hw_enabled {
            out.push(Box::new(hw::HardwareSampler::new(effective, default_interval)));
        }
        if cfg.sw_enabled {
            out.push(Box::new(sw::SoftwareSampler::new(effective, default_interval)));
        }
    }
    out
}

#[cfg(all(test, target_os = "linux"))]
mod integration {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::sync::Arc;
    use std::time::Duration;

    /// Requires CAP_PERFMON or perf_event_paranoid <= 1. Self-skips otherwise.
    #[tokio::test]
    async fn hw_sampler_increments_over_time() {
        use nyquist_core::sampler::Sampler;
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let mut s = hw::HardwareSampler::new(1, Duration::from_millis(10));
        let t0 = std::time::Instant::now();
        let _ = s.sample(&reg, t0).await;
        if reg.metric_ids().is_empty() {
            eprintln!("SKIP: perf_event_open unavailable (no CAP_PERFMON / paranoid too high)");
            return;
        }
        let _ = (0u64..1_000_000).sum::<u64>();
        let _ = s.sample(&reg, t0 + Duration::from_millis(10)).await;
        let ids = reg.metric_ids();
        assert!(!ids.is_empty());
        let any_nonzero = ids.iter().any(|id| reg.raw(*id) > 0);
        assert!(any_nonzero, "all hw counters are zero after CPU burn");
    }
}
