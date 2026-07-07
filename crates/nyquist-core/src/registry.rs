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
    /// BPF path: externally-aggregated bucket array bypasses the windowing engine.
    direct_buckets: Option<Vec<(u64, u64)>>,
}

pub struct Registry {
    metrics: DashMap<MetricId, Mutex<MetricState>>,
    slice_width: Duration,
    window: Duration,
    samples_per_slice: usize,
}

impl Registry {
    pub fn new(slice_width: Duration, window: Duration) -> Self {
        // Default capacity assumes the nominal 10 ms tick; agents with faster
        // configured samplers must use with_min_interval or slices truncate.
        Self::with_min_interval(slice_width, window, Duration::from_millis(10))
    }

    /// `min_interval` is the fastest sampler tick that will record into any
    /// metric; per-slice sample capacity is sized from it (with 2x headroom
    /// for scheduler-jitter clustering) so fast tickers aren't truncated.
    pub fn with_min_interval(slice_width: Duration, window: Duration, min_interval: Duration) -> Self {
        let per_slice = (slice_width.as_nanos() / min_interval.as_nanos().max(1)) as usize;
        let samples_per_slice = (per_slice * 2).clamp(crate::hist::DEFAULT_SAMPLES_PER_SLICE, 4096);
        Registry { metrics: DashMap::new(), slice_width, window, samples_per_slice }
    }

    pub fn register(&self, def: MetricDef) -> MetricId {
        let id = metric_id(&def.name, &def.labels);
        // Fast path: shared read lock if already registered (common case on every tick).
        if !self.metrics.contains_key(&id) {
            self.metrics.entry(id).or_insert_with(|| {
                Mutex::new(MetricState {
                    def,
                    window: SlidingHistogram::with_capacity(self.slice_width, self.window, self.samples_per_slice),
                    raw: 0,
                    prev: None,
                    direct_buckets: None,
                })
            });
        }
        id
    }

    /// Minimum interval over which a counter delta becomes a rate sample.
    /// A delta divided by scheduler-jitter-sized dt (e.g. 1 ms when the tick
    /// fires twice in rapid succession) inflates the rate ~10x and corrupts
    /// tail percentiles.
    const MIN_RATE_DT: f64 = 0.005;

    pub fn record_counter(&self, id: MetricId, now: Instant, value: u64) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            match s.prev {
                None => s.prev = Some((now, value)),
                // Counter reset: re-baseline, skip this interval.
                Some((_, prev_v)) if value < prev_v => s.prev = Some((now, value)),
                Some((prev_t, prev_v)) => {
                    let dt = now.saturating_duration_since(prev_t).as_secs_f64();
                    if dt >= Self::MIN_RATE_DT {
                        let rate = ((value - prev_v) as f64 / dt).round() as u64;
                        s.window.record(now, rate);
                        s.prev = Some((now, value));
                    }
                    // dt < MIN_RATE_DT: leave prev in place so the delta
                    // coalesces into the next qualifying tick and is averaged
                    // over the combined interval. Advancing prev here would
                    // excise the delta from the distribution entirely —
                    // biasing tail percentiles low exactly during bursts, and
                    // discarding ALL samples for sub-5 ms sampler intervals.
                }
            }
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

    /// Store an externally-aggregated H2 bucket array directly (BPF path, design §3.4).
    /// Bypasses the windowing engine — these buckets come from the kernel and are exposed as-is.
    pub fn record_distribution_buckets(&self, id: MetricId, buckets: Vec<(u64, u64)>) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            s.raw = buckets.iter().map(|&(_, c)| c).sum();
            s.direct_buckets = Some(buckets);
        }
    }

    pub fn snapshot(&self, now: std::time::Instant) -> crate::snapshot::RegistrySnapshot {
        let mut metrics = Vec::new();
        for entry in self.metrics.iter() {
            let mut s = entry.value().lock().unwrap();
            let buckets = match &s.direct_buckets {
                Some(b) => b.clone(),
                None => s.window.bucket_counts(now),
            };
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
    fn jitter_sample_below_min_dt_is_coalesced_not_inflated() {
        // A record_counter call arriving only 1 ms after the previous one would
        // produce rate = delta / 0.001 s — ~10× inflated. The guard must not
        // record over that tiny dt; instead the delta stays pending (prev is
        // not advanced) and is averaged over the combined interval at the next
        // qualifying tick: 25 MB over 10 ms = 2.5 GB/s.
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("net/rx_bytes", Kind::Counter));
        let t0 = Instant::now();
        reg.record_counter(id, t0, 0);
        // Jitter: second call only 1 ms later — not recorded over 1 ms.
        reg.record_counter(id, t0 + Duration::from_millis(1), 25_000_000);
        // Normal 10 ms tick — records the coalesced delta over the full 10 ms.
        reg.record_counter(id, t0 + Duration::from_millis(10), 25_000_000);
        let now = t0 + Duration::from_millis(50);
        // The inflated 25 GB/s jitter rate must not appear...
        let p99 = reg.percentile(id, now, 99.0);
        assert!(p99 < 10_000_000_000, "inflated jitter rate leaked: p99 = {p99} bytes/s");
        // ...but the 25 MB delta must not vanish either (2.5 GB/s ± bucket error).
        let p50 = reg.percentile(id, now, 50.0);
        assert!(
            (2_400_000_000..=2_600_000_000).contains(&p50),
            "coalesced delta lost or wrong: p50 = {p50} bytes/s"
        );
    }

    #[test]
    fn sub_5ms_sampler_intervals_still_produce_rates() {
        // A sampler configured at interval = 2ms (supported by nyquist-config)
        // ticks entirely below MIN_RATE_DT. Deltas must coalesce into ≥5 ms
        // windows rather than being dropped wholesale, which previously left
        // the histogram permanently empty (rate percentiles read 0).
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("fast/counter", Kind::Counter));
        let t0 = Instant::now();
        // 1000 bytes every 2 ms = a steady 500_000 bytes/s.
        for i in 1..=400u64 {
            reg.record_counter(id, t0 + Duration::from_millis(i * 2), i * 1000);
        }
        let now = t0 + Duration::from_millis(800);
        let p50 = reg.percentile(id, now, 50.0);
        assert!(
            (450_000..=550_000).contains(&p50),
            "sub-5ms interval rates missing or wrong: p50 = {p50} bytes/s"
        );
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
    fn direct_buckets_appear_in_snapshot() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("tcp/packet_latency", Kind::Distribution));
        reg.record_distribution_buckets(id, vec![(100, 5), (1000, 2)]);
        let snap = reg.snapshot(Instant::now());
        let m = snap.metrics.iter().find(|m| m.name == "tcp/packet_latency").unwrap();
        assert_eq!(m.buckets, vec![(100, 5), (1000, 2)]);
        assert_eq!(m.raw, 7);
    }

    #[test]
    fn fast_ticks_are_not_truncated_with_sized_capacity() {
        // Regression: with the old fixed 16-sample slice, a 2 ms gauge filled
        // a 100 ms slice with 50 samples but kept only the first 16 — a
        // temporally-biased 68% loss. with_min_interval must size capacity so
        // the whole slice survives.
        let reg = Registry::with_min_interval(
            Duration::from_millis(100),
            Duration::from_secs(1),
            Duration::from_millis(2),
        );
        let id = reg.register(MetricDef::new("fast/gauge", Kind::Gauge));
        let t0 = Instant::now();
        // Ramp 1..=50 inside a single 100 ms slice (2 ms apart). Head-only
        // truncation would keep 1..=16 and report p90 ≈ 15.
        for i in 1..=50u64 {
            reg.record_gauge(id, t0 + Duration::from_millis((i - 1) * 2), i);
        }
        let now = t0 + Duration::from_millis(99);
        let p90 = reg.percentile(id, now, 90.0);
        assert!(p90 >= 44, "late-slice samples truncated: p90 = {p90}");
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
