use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use crate::registry::Registry;
use crate::sampler::Sampler;
use crate::sink::Sink;

pub fn spawn_sampler(
    mut sampler: Box<dyn Sampler>,
    reg: Arc<Registry>,
    fault_tolerant: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(sampler.interval());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let now = Instant::now();
            if let Err(e) = sampler.sample(&reg, now).await {
                tracing::warn!(sampler = sampler.name(), error = %e, "sample failed");
                if !fault_tolerant {
                    tracing::error!(sampler = sampler.name(), "exiting: fault_tolerant=false");
                    break;
                }
            }
        }
    })
}

/// Spawn a task that calls `sink.export()` every `export_interval`,
/// passing a fresh bucket-array snapshot from `reg`. Percentile selection is a
/// consumer concern owned by each sink (design §3.4).
pub fn spawn_sink(
    mut sink: Box<dyn Sink>,
    reg: Arc<Registry>,
    export_interval: Duration,
    fault_tolerant: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(export_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let snapshot = reg.snapshot(Instant::now());
            if let Err(e) = sink.export(&snapshot).await {
                tracing::warn!(error = %e, "sink export failed");
                if !fault_tolerant {
                    tracing::error!("exiting: fault_tolerant=false");
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{MetricDef, Registry};
    use crate::model::Kind;
    use crate::sampler::{Sampler, SamplerError};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct CountingSampler { id: crate::model::MetricId, n: u64 }

    #[async_trait::async_trait]
    impl Sampler for CountingSampler {
        fn name(&self) -> &str { "counting" }
        fn interval(&self) -> Duration { Duration::from_millis(10) }
        async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
            self.n += 100;
            reg.record_counter(self.id, now, self.n);
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn scheduler_ticks_sampler_on_its_interval() {
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let id = reg.register(MetricDef::new("ticks", Kind::Counter));
        let handle = spawn_sampler(Box::new(CountingSampler { id, n: 0 }), reg.clone(), true);
        tokio::time::sleep(Duration::from_millis(105)).await;
        handle.abort();
        assert!(reg.raw(id) >= 500, "raw was {}", reg.raw(id));
    }
}
