use histogram::Histogram;

/// Grouping power 7 (~1% relative error) and max value power 39
/// (covers up to ~5.4e11, enough for byte/sec rates).
pub const HIST_GROUPING_POWER: u8 = 7;
pub const HIST_MAX_VALUE_POWER: u8 = 39;

pub struct HistogramSlice {
    inner: Histogram,
}

impl HistogramSlice {
    pub fn new() -> Self {
        HistogramSlice {
            inner: Histogram::new(HIST_GROUPING_POWER, HIST_MAX_VALUE_POWER)
                .expect("valid histogram parameters"),
        }
    }

    pub fn record(&mut self, value: u64) {
        let _ = self.inner.increment(value);
    }

    pub fn clear(&mut self) {
        self.inner = Histogram::new(HIST_GROUPING_POWER, HIST_MAX_VALUE_POWER)
            .expect("valid histogram parameters");
    }

    pub fn merge_into(&self, acc: &mut Histogram) {
        if let Ok(sum) = acc.checked_add(&self.inner) {
            *acc = sum;
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
    fn percentile_of_uniform_values_is_near_input() {
        let mut s = HistogramSlice::new();
        for v in 1..=1000u64 { s.record(v); }
        let mut acc = empty_accumulator();
        s.merge_into(&mut acc);
        let p50 = percentile(&acc, 50.0);
        assert!((490..=515).contains(&p50), "p50 was {p50}");
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
