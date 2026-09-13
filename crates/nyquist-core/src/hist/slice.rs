use histogram::Histogram;

pub const HIST_GROUPING_POWER: u8 = 7;
/// 2^52-1 ≈ 4.5e15 covers byte-valued gauges on multi-TiB hosts and aggregate
/// byte rates far beyond multi-NIC line speed. The previous 2^39-1 (~550 GB)
/// was exceeded by MemTotal on >512 GiB machines, and out-of-range values were
/// silently dropped at merge time — deflating percentiles invisibly.
pub const HIST_MAX_VALUE_POWER: u8 = 52;
/// Largest value representable in the windowed histograms. Samples above this
/// are clamped in `HistogramSlice::record` so they pin the top bucket (a
/// visible signal) instead of vanishing.
pub const HIST_MAX_TRACKABLE: u64 = (1 << HIST_MAX_VALUE_POWER) - 1;

/// Default per-slice sample capacity: the nominal 10 ms tick over a 100 ms
/// slice yields 10 samples; 2x headroom absorbs scheduler jitter clustering.
/// Registries built via `Registry::with_min_interval` derive a larger cap for
/// faster tick rates before promoting to bucket storage.
pub const DEFAULT_SAMPLES_PER_SLICE: usize = 20;

/// A compact accumulator for one time slice.
///
/// Stores raw observed values (a Vec grown geometrically on demand, capped at
/// `max_samples`) instead of a pre-allocated histogram bucket grid. The full
/// histogram is allocated only when the raw buffer fills, rather than for
/// every one of the 600 ring slots.
pub struct HistogramSlice {
    values: Vec<u64>,
    max_samples: u32,
    overflow: Option<Histogram>,
}

impl HistogramSlice {
    pub fn new(max_samples: usize) -> Self {
        HistogramSlice { values: Vec::new(), max_samples: max_samples.max(1) as u32, overflow: None }
    }

    pub fn record(&mut self, value: u64) {
        // Clamp instead of letting merge_into silently drop out-of-range
        // values: a percentile pinned at the top bucket is visible; a vanished
        // sample deflates percentiles with no signal at all.
        let value = if value > HIST_MAX_TRACKABLE {
            warn_clamped_once(value);
            HIST_MAX_TRACKABLE
        } else {
            value
        };
        if let Some(h) = &mut self.overflow {
            h.increment(value).expect("clamped value");
        } else if self.values.len() < self.max_samples as usize {
            self.values.push(value);
        } else {
            // Promote a busy slice to bounded bucket storage, preserving every
            // observation even when one tick samples thousands of connections.
            let mut h = empty_accumulator();
            for v in self.values.drain(..) { h.increment(v).expect("clamped value"); }
            h.increment(value).expect("clamped value");
            self.values = Vec::new();
            self.overflow = Some(h);
        }
    }

    pub fn clear(&mut self) {
        self.values.clear();
        self.overflow = None;
    }

    /// Insert this slice's values into an accumulator histogram.
    /// Called from `SlidingHistogram::merge_window` for each in-window slice.
    pub fn merge_into(&self, acc: &mut Histogram) {
        if let Some(h) = &self.overflow {
            for b in h.iter().filter(|b| b.count() > 0) {
                acc.add(b.end(), b.count()).expect("matching histogram bounds");
            }
        }
        for &v in &self.values {
            // Values are pre-clamped to HIST_MAX_TRACKABLE in record(), so
            // increment cannot fail with OutOfRange.
            debug_assert!(v <= HIST_MAX_TRACKABLE);
            let _ = acc.increment(v);
        }
    }
}

pub fn empty_accumulator() -> Histogram {
    Histogram::new(HIST_GROUPING_POWER, HIST_MAX_VALUE_POWER).expect("valid histogram parameters")
}

pub fn percentile(acc: &Histogram, p: f64) -> u64 {
    match acc.percentile(p) {
        Ok(Some(bucket)) => bucket.end(),
        _ => 0,
    }
}

/// One warning per process: clamping means some metric's true magnitude
/// exceeds the histogram range and its top percentiles are pinned.
fn warn_clamped_once(value: u64) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            value,
            max = HIST_MAX_TRACKABLE,
            "histogram sample clamped to max trackable; affected percentiles pin at the top bucket"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_merges_values() {
        let mut s = HistogramSlice::new(DEFAULT_SAMPLES_PER_SLICE);
        for v in 1u64..=10 { s.record(v); }
        let mut acc = empty_accumulator();
        s.merge_into(&mut acc);
        // p50 of [1..=10] — bucket end will be ≥5
        let p50 = percentile(&acc, 50.0);
        assert!((4..=7).contains(&p50), "p50 was {p50}");
        // All 10 values present: total count = 10
        let total: u64 = acc.iter().map(|b| b.count()).sum();
        assert_eq!(total, 10);
    }

    #[test]
    fn samples_beyond_capacity_are_preserved() {
        let mut s = HistogramSlice::new(16);
        for v in 0..100u64 { s.record(v); }
        let mut h = empty_accumulator();
        s.merge_into(&mut h);
        assert_eq!(h.iter().map(|b| b.count()).sum::<u64>(), 100);
        assert!(percentile(&h, 99.0) >= 98);
    }

    #[test]
    fn capacity_is_runtime_configurable() {
        let mut s = HistogramSlice::new(64);
        for v in 0..100u64 { s.record(v); }
        assert!(s.overflow.is_some());
        s.clear();
        let mut h = empty_accumulator();
        s.merge_into(&mut h);
        assert_eq!(h.iter().map(|b| b.count()).sum::<u64>(), 0);
    }

    #[test]
    fn clear_resets_counts() {
        let mut s = HistogramSlice::new(DEFAULT_SAMPLES_PER_SLICE);
        s.record(100);
        s.clear();
        let mut acc = empty_accumulator();
        s.merge_into(&mut acc);
        assert_eq!(percentile(&acc, 99.0), 0);
    }

    #[test]
    fn values_above_max_are_clamped_not_dropped() {
        // Regression: values > max trackable were silently discarded at merge
        // time, deflating percentiles (the inverse of the 2^39-1 saturation
        // bug). They must clamp and stay countable.
        let mut s = HistogramSlice::new(DEFAULT_SAMPLES_PER_SLICE);
        s.record(u64::MAX);
        let mut acc = empty_accumulator();
        s.merge_into(&mut acc);
        let total: u64 = acc.iter().map(|b| b.count()).sum();
        assert_eq!(total, 1, "clamped sample vanished from the histogram");
        let p99 = percentile(&acc, 99.0);
        assert!(p99 >= HIST_MAX_TRACKABLE / 2, "clamped sample not at top bucket: {p99}");
    }

    #[test]
    fn terabyte_scale_values_are_representable() {
        // MemTotal on a 1 TiB host, in bytes — above the old 2^39-1 ceiling.
        let mem_total: u64 = 1 << 40;
        let mut s = HistogramSlice::new(DEFAULT_SAMPLES_PER_SLICE);
        s.record(mem_total);
        let mut acc = empty_accumulator();
        s.merge_into(&mut acc);
        let p50 = percentile(&acc, 50.0);
        // Bucket end within grouping-power-7 relative error (~1%).
        assert!(
            p50 >= mem_total && p50 <= mem_total + (mem_total >> 6),
            "1 TiB sample misplaced: p50 = {p50}"
        );
    }
}
