use std::time::{Duration, Instant};
use super::slice::{HistogramSlice, empty_accumulator, percentile as compute_percentile};

/// A ring of histogram slices forming a sliding time window.
/// Slices are allocated lazily on first write; an empty histogram costs only the ring metadata.
pub struct SlidingHistogram {
    slices: Vec<Option<HistogramSlice>>,
    slice_index: Vec<Option<u64>>,
    slice_width: Duration,
    n_slices: usize,
    origin: Option<Instant>,
}

impl SlidingHistogram {
    pub fn new(slice_width: Duration, window: Duration) -> Self {
        let n_slices = (window.as_nanos() / slice_width.as_nanos()).max(1) as usize;
        SlidingHistogram {
            slices: (0..n_slices).map(|_| None).collect(),
            slice_index: vec![None; n_slices],
            slice_width,
            n_slices,
            origin: None,
        }
    }

    fn absolute_slice(&mut self, now: Instant) -> u64 {
        let origin = *self.origin.get_or_insert(now);
        let elapsed = now.saturating_duration_since(origin);
        (elapsed.as_nanos() / self.slice_width.as_nanos()) as u64
    }

    pub fn record(&mut self, now: Instant, value: u64) {
        let abs = self.absolute_slice(now);
        let cell = (abs as usize) % self.n_slices;
        if self.slice_index[cell] != Some(abs) {
            // Ring cell holds a stale epoch: drop the old allocation and start fresh.
            self.slices[cell] = None;
            self.slice_index[cell] = Some(abs);
        }
        self.slices[cell]
            .get_or_insert_with(HistogramSlice::new)
            .record(value);
    }

    /// Merge all slices within the window into a single accumulator histogram.
    fn merge_window(&mut self, now: Instant) -> histogram::Histogram {
        let abs_now = self.absolute_slice(now);
        let oldest = abs_now.saturating_sub(self.n_slices as u64 - 1);
        let mut acc = empty_accumulator();
        for cell in 0..self.n_slices {
            if let (Some(abs), Some(slice)) = (self.slice_index[cell], &self.slices[cell]) {
                if abs >= oldest && abs <= abs_now {
                    slice.merge_into(&mut acc);
                }
            }
        }
        acc
    }

    pub fn percentile(&mut self, now: Instant, p: f64) -> u64 {
        compute_percentile(&self.merge_window(now), p)
    }

    /// Compute multiple percentiles from a single window merge.
    /// Always prefer this over calling `percentile()` N times — each call to
    /// `percentile()` re-merges the entire window.
    pub fn percentile_batch(&mut self, now: Instant, ps: &[f64]) -> Vec<u64> {
        let acc = self.merge_window(now);
        ps.iter().map(|&p| compute_percentile(&acc, p)).collect()
    }

    /// Merge the window and return the non-empty H2 buckets as
    /// `(upper_bound, count)` pairs in ascending order. This is the
    /// downstream bucket-array representation (design §3.4); consumers
    /// compute percentiles from it via `percentiles_from_buckets`.
    pub fn bucket_counts(&mut self, now: Instant) -> Vec<(u64, u64)> {
        let acc = self.merge_window(now);
        acc.iter()
            .filter(|b| b.count() > 0)
            .map(|b| (b.end(), b.count()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn records_within_window_are_counted() {
        let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(1));
        let t0 = Instant::now();
        for i in 0..1000u64 {
            h.record(t0 + Duration::from_micros(i * 100), 42);
        }
        let p50 = h.percentile(t0 + Duration::from_millis(999), 50.0);
        assert!((41..=43).contains(&p50), "p50 was {p50}");
    }

    #[test]
    fn bucket_counts_are_sparse_and_ascending() {
        let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(1));
        let t0 = Instant::now();
        for _ in 0..10 { h.record(t0, 5); }
        for _ in 0..3 { h.record(t0, 1000); }
        let buckets = h.bucket_counts(t0 + Duration::from_millis(50));
        // Non-empty buckets only.
        assert!(buckets.iter().all(|&(_, c)| c > 0), "empty bucket leaked: {buckets:?}");
        // Ascending by upper bound.
        let bounds: Vec<u64> = buckets.iter().map(|&(b, _)| b).collect();
        let mut sorted = bounds.clone();
        sorted.sort_unstable();
        assert_eq!(bounds, sorted, "buckets not ascending: {buckets:?}");
        // Total count is preserved.
        let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
        assert_eq!(total, 13, "total count wrong: {buckets:?}");
    }

    #[test]
    fn old_slices_expire_out_of_window() {
        let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(1));
        let t0 = Instant::now();
        for _ in 0..100 { h.record(t0, 1_000_000); }
        let later = t0 + Duration::from_secs(2);
        for i in 0..100u64 { h.record(later + Duration::from_millis(i), 10); }
        let p99 = h.percentile(later + Duration::from_millis(100), 99.0);
        assert!(p99 < 100, "stale burst leaked into window: p99 = {p99}");
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn percentiles_are_monotonic(values in proptest::collection::vec(1u64..1_000_000, 1..500)) {
            let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(10));
            let t0 = Instant::now();
            for (i, v) in values.iter().enumerate() {
                h.record(t0 + Duration::from_millis(i as u64), *v);
            }
            let now = t0 + Duration::from_millis(values.len() as u64);
            let pcts = h.percentile_batch(now, &[50.0, 90.0, 99.0]);
            let (p50, p90, p99) = (pcts[0], pcts[1], pcts[2]);
            prop_assert!(p50 <= p90, "p50={p50} p90={p90}");
            prop_assert!(p90 <= p99, "p90={p90} p99={p99}");
        }
    }
}
