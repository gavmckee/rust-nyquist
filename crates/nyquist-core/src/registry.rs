use std::sync::Mutex;
use std::time::{Duration, Instant};
use dashmap::DashMap;
use crate::hist::SlidingHistogram;
use crate::model::{Kind, Labels, MetricId, Unit, metric_id};

#[derive(Clone, Debug)]
pub struct MetricDef {
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    pub description: Option<String>,
    pub labels: Labels,
}

impl MetricDef {
    pub fn new(name: impl Into<String>, kind: Kind) -> Self {
        MetricDef { name: name.into(), kind, unit: Unit::None, description: None, labels: Labels::new() }
    }
    pub fn unit(mut self, u: Unit) -> Self { self.unit = u; self }
    pub fn description(mut self, d: impl Into<String>) -> Self { self.description = Some(d.into()); self }
    pub fn labels(mut self, l: Labels) -> Self { self.labels = l; self }
}

struct MetricState {
    def: MetricDef,
    window: SlidingHistogram,
    raw: u64,
    prev: Option<(Instant, u64)>,
}

pub struct Registry {
    metrics: DashMap<MetricId, Mutex<MetricState>>,
    slice_width: Duration,
    window: Duration,
}

impl Registry {
    pub fn new(slice_width: Duration, window: Duration) -> Self {
        Registry { metrics: DashMap::new(), slice_width, window }
    }

    pub fn register(&self, def: MetricDef) -> MetricId {
        let id = metric_id(&def.name, &def.labels);
        // Fast path: shared read lock if already registered (common case on every tick).
        if !self.metrics.contains_key(&id) {
            self.metrics.entry(id).or_insert_with(|| {
                Mutex::new(MetricState {
                    def,
                    window: SlidingHistogram::new(self.slice_width, self.window),
                    raw: 0,
                    prev: None,
                })
            });
        }
        id
    }

    pub fn record_counter(&self, id: MetricId, now: Instant, value: u64) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            if let Some((prev_t, prev_v)) = s.prev {
                if value >= prev_v {
                    let dt = now.saturating_duration_since(prev_t).as_secs_f64();
                    if dt > 0.0 {
                        let rate = ((value - prev_v) as f64 / dt).round() as u64;
                        s.window.record(now, rate);
                    }
                }
                // value < prev_v => counter reset; skip this interval.
            }
            s.prev = Some((now, value));
            s.raw = value;
        }
    }

    pub fn record_gauge(&self, id: MetricId, now: Instant, value: u64) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            s.window.record(now, value);
            s.raw = value;
        }
    }

    pub fn percentile(&self, id: MetricId, now: Instant, p: f64) -> u64 {
        match self.metrics.get(&id) {
            Some(state) => state.lock().unwrap().window.percentile(now, p),
            None => 0,
        }
    }

    pub fn raw(&self, id: MetricId) -> u64 {
        self.metrics.get(&id).map(|s| s.lock().unwrap().raw).unwrap_or(0)
    }

    pub fn metric_ids(&self) -> Vec<MetricId> {
        self.metrics.iter().map(|e| *e.key()).collect()
    }

    pub fn snapshot(&self, now: std::time::Instant) -> crate::snapshot::RegistrySnapshot {
        let mut metrics = Vec::new();
        for entry in self.metrics.iter() {
            let mut s = entry.value().lock().unwrap();
            let buckets = s.window.bucket_counts(now);
            metrics.push(crate::snapshot::MetricSnapshot {
                name: s.def.name.clone(),
                kind: s.def.kind,
                unit: s.def.unit,
                labels: s.def.labels.clone(),
                raw: s.raw,
                buckets,
            });
        }
        crate::snapshot::RegistrySnapshot { metrics, captured: std::time::SystemTime::now() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;
    use std::time::{Duration, Instant};

    #[test]
    fn counter_rate_reflects_delta_over_time() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("net/tx_bytes", Kind::Counter));
        let t0 = Instant::now();
        let mut total = 0u64;
        for i in 1..=50u64 {
            total += 1000;
            reg.record_counter(id, t0 + Duration::from_millis(i * 10), total);
        }
        let now = t0 + Duration::from_millis(500);
        let p50 = reg.percentile(id, now, 50.0);
        assert!((95_000..=105_000).contains(&p50), "rate p50 was {p50}");
        assert_eq!(reg.raw(id), 50_000);
    }

    #[test]
    fn counter_reset_is_ignored_for_one_interval() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("c", Kind::Counter));
        let t0 = Instant::now();
        reg.record_counter(id, t0 + Duration::from_millis(10), 1000);
        reg.record_counter(id, t0 + Duration::from_millis(20), 2000);
        reg.record_counter(id, t0 + Duration::from_millis(30), 5); // reset
        assert_eq!(reg.raw(id), 5);
    }

    #[test]
    fn gauge_records_reading_directly() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("mem/free", Kind::Gauge));
        let t0 = Instant::now();
        for i in 0..50u64 { reg.record_gauge(id, t0 + Duration::from_millis(i * 10), 4096); }
        let now = t0 + Duration::from_millis(500);
        let p50 = reg.percentile(id, now, 50.0);
        assert!((4096..=4128).contains(&p50), "gauge p50 was {p50}");
        assert_eq!(reg.raw(id), 4096);
    }
}
