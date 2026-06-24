use histogram::Histogram;

pub const BPF_GROUPING_POWER: u8 = 3;
pub const BPF_MAX_VALUE_POWER: u8 = 64;
pub const BPF_BUCKETS: usize = 496;

/// Reconstruct sparse `(upper_bound, count)` pairs from raw mmap'd kernel counts.
pub fn buckets_from_counts(counts: &[u64]) -> Vec<(u64, u64)> {
    let mut v = counts.to_vec();
    v.resize(BPF_BUCKETS, 0);
    let h = Histogram::from_buckets(BPF_GROUPING_POWER, BPF_MAX_VALUE_POWER, v)
        .expect("valid H2 config");
    (&h)
        .into_iter()
        .filter(|b| b.count() > 0)
        .map(|b| (b.end(), b.count()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use histogram::Histogram;

    #[test]
    fn sparse_and_ascending() {
        let mut counts = vec![0u64; BPF_BUCKETS];
        counts[5] = 10;
        counts[12] = 3;
        let buckets = buckets_from_counts(&counts);
        let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
        assert_eq!(total, 13);
        assert!(buckets.iter().all(|&(_, c)| c > 0));
        let bounds: Vec<u64> = buckets.iter().map(|&(b, _)| b).collect();
        let mut sorted = bounds.clone();
        sorted.sort_unstable();
        assert_eq!(bounds, sorted);
    }

    #[test]
    fn relative_error_within_bound() {
        // Build a reference histogram to find the bucket upper_bound for each test value,
        // then verify our buckets_from_counts round-trips correctly.
        for &value in &[1u64, 100, 1_000, 10_000, 1_000_000, 1_000_000_000] {
            let mut ref_h = Histogram::new(BPF_GROUPING_POWER, BPF_MAX_VALUE_POWER).unwrap();
            ref_h.increment(value).unwrap();
            // The bucket that contains `value`
            let bucket = (&ref_h).into_iter().find(|b| b.count() > 0).unwrap();
            let upper = bucket.end();
            // Reconstruct via buckets_from_counts
            let raw = ref_h.as_slice().to_vec();
            let sparse = buckets_from_counts(&raw);
            assert_eq!(sparse.len(), 1, "value={value}");
            assert_eq!(sparse[0].0, upper, "value={value}");
            // upper bound >= value (bucket ceiling)
            assert!(upper >= value, "upper={upper} < value={value}");
            // For grouping_power=3, max relative error per bucket width is 1/2^3 = 12.5%
            let err = (upper as f64 - value as f64) / value as f64;
            assert!(err < 0.13, "relative error {err:.3} > 12.5% for value={value}");
        }
    }

    #[test]
    fn total_bucket_count_is_496() {
        // Confirm the kernel histogram size matches our constant.
        let h = Histogram::new(BPF_GROUPING_POWER, BPF_MAX_VALUE_POWER).unwrap();
        assert_eq!(h.as_slice().len(), BPF_BUCKETS);
    }
}
