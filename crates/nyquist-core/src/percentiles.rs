//! Consumer-side percentile computation from H2 bucket arrays (design §3.4).
//! The collection path emits full bucket arrays; the percentile set is a
//! consumer concern, computed here at read time.

/// Compute percentiles from a sparse H2 bucket array.
///
/// `buckets` is `(upper_bound, count)` pairs sorted ascending by upper bound
/// (as produced by `SlidingHistogram::bucket_counts`). `ps` are percentiles in
/// `0.0..=100.0`. Returns `(percentile, value)` pairs in the same order as `ps`.
/// The returned value is the upper bound of the bucket containing the rank,
/// matching the `histogram` crate's percentile semantics.
pub fn percentiles_from_buckets(buckets: &[(u64, u64)], ps: &[f64]) -> Vec<(f64, u64)> {
    let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
    if total == 0 {
        return ps.iter().map(|&p| (p, 0)).collect();
    }
    ps.iter()
        .map(|&p| {
            let rank = ((p / 100.0) * total as f64).ceil() as u64;
            let rank = rank.clamp(1, total);
            let mut cum = 0u64;
            let mut val = 0u64;
            for &(end, count) in buckets {
                cum += count;
                if cum >= rank {
                    val = end;
                    break;
                }
            }
            (p, val)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buckets_yield_zero() {
        let out = percentiles_from_buckets(&[], &[50.0, 99.0]);
        assert_eq!(out, vec![(50.0, 0), (99.0, 0)]);
    }

    #[test]
    fn single_bucket_returns_its_bound() {
        let out = percentiles_from_buckets(&[(42, 100)], &[50.0, 99.9]);
        assert_eq!(out, vec![(50.0, 42), (99.9, 42)]);
    }

    #[test]
    fn percentiles_pick_the_rank_bucket() {
        // 90 samples at bound 10, 10 samples at bound 1000.
        let buckets = [(10u64, 90u64), (1000u64, 10u64)];
        let out = percentiles_from_buckets(&buckets, &[50.0, 90.0, 99.0]);
        assert_eq!(out[0], (50.0, 10));   // rank 50 -> first bucket
        assert_eq!(out[1], (90.0, 10));   // rank 90 -> still first bucket
        assert_eq!(out[2], (99.0, 1000)); // rank 99 -> second bucket
    }

    #[test]
    fn percentiles_are_monotonic() {
        let buckets = [(1u64, 5u64), (10u64, 5u64), (100u64, 5u64), (1000u64, 5u64)];
        let out = percentiles_from_buckets(&buckets, &[50.0, 90.0, 99.0, 99.9]);
        let vals: Vec<u64> = out.iter().map(|&(_, v)| v).collect();
        let mut sorted = vals.clone();
        sorted.sort_unstable();
        assert_eq!(vals, sorted, "percentiles not monotonic: {vals:?}");
    }
}
