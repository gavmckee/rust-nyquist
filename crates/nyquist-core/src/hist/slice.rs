use histogram::Histogram;

pub const HIST_GROUPING_POWER: u8 = 7;
pub const HIST_MAX_VALUE_POWER: u8 = 39;

/// Maximum raw samples stored per time slice.
///
/// At 10 ms tick rate and 100 ms slice width each slice sees exactly 10 samples.
/// 16 gives headroom for finer tick rates without heap allocation.
const MAX_SAMPLES_PER_SLICE: usize = 16;

/// A compact accumulator for one time slice.
///
/// Stores raw observed values in a fixed inline array instead of a pre-allocated
/// 33 KB histogram bucket grid. The full histogram is built on-demand in
/// `merge_into` rather than kept live for every one of the 600 ring slots.
///
/// Memory: 16 × 8 + 1 = 129 bytes (padded to 136) vs 33,792 bytes previously.
pub struct HistogramSlice {
    values: [u64; MAX_SAMPLES_PER_SLICE],
    len: u8,
}

impl HistogramSlice {
    pub fn new() -> Self {
        HistogramSlice { values: [0; MAX_SAMPLES_PER_SLICE], len: 0 }
    }

    pub fn record(&mut self, value: u64) {
        if (self.len as usize) < MAX_SAMPLES_PER_SLICE {
            self.values[self.len as usize] = value;
            self.len += 1;
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Insert this slice's values into an accumulator histogram.
    /// Called from `SlidingHistogram::merge_window` for each in-window slice.
    pub fn merge_into(&self, acc: &mut Histogram) {
        for &v in &self.values[..self.len as usize] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_merges_values() {
        let mut s = HistogramSlice::new();
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
    fn samples_beyond_capacity_are_dropped_gracefully() {
        let mut s = HistogramSlice::new();
        for v in 0..100u64 { s.record(v); }
        assert_eq!(s.len, MAX_SAMPLES_PER_SLICE as u8);
    }

    #[test]
    fn clear_resets_counts() {
        let mut s = HistogramSlice::new();
        s.record(100);
        s.clear();
        let mut acc = empty_accumulator();
        s.merge_into(&mut acc);
        assert_eq!(percentile(&acc, 99.0), 0);
    }
}
