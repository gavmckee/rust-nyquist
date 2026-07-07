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
///
/// When `shutdown` flips (or its sender drops), the task performs one final
/// export + `sink.flush()` so internally buffered data (e.g. the Parquet
/// recorder's pending rows) survives process termination, then exits.
pub fn spawn_sink(
    mut sink: Box<dyn Sink>,
    reg: Arc<Registry>,
    export_interval: Duration,
    fault_tolerant: bool,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(export_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let snapshot = reg.snapshot(Instant::now());
                    if let Err(e) = sink.export(&snapshot).await {
                        tracing::warn!(error = %e, "sink export failed");
                        if !fault_tolerant {
                            tracing::error!("exiting: fault_tolerant=false");
                            return;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    let snapshot = reg.snapshot(Instant::now());
                    if let Err(e) = sink.export(&snapshot).await {
                        tracing::warn!(error = %e, "final sink export failed during shutdown");
                    }
                    if let Err(e) = sink.flush().await {
                        tracing::warn!(error = %e, "final sink flush failed during shutdown");
                    }
                    return;
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

    struct ObservableSink {
        exports: Arc<std::sync::atomic::AtomicU64>,
        flushed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::sink::Sink for ObservableSink {
        async fn export(&mut self, _s: &crate::snapshot::RegistrySnapshot) -> Result<(), crate::sink::SinkError> {
            self.exports.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn flush(&mut self) -> Result<(), crate::sink::SinkError> {
            self.flushed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sink_shutdown_does_final_export_and_flush() {
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let exports = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let flushed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sink = ObservableSink { exports: exports.clone(), flushed: flushed.clone() };
        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = spawn_sink(Box::new(sink), reg, Duration::from_secs(10), true, rx);
        tokio::time::sleep(Duration::from_millis(1)).await;
        let before = exports.load(std::sync::atomic::Ordering::SeqCst);
        tx.send(true).unwrap();
        handle.await.unwrap();
        assert!(flushed.load(std::sync::atomic::Ordering::SeqCst), "flush not called on shutdown");
        assert_eq!(
            exports.load(std::sync::atomic::Ordering::SeqCst),
            before + 1,
            "shutdown must do exactly one final export"
        );
    }
}
