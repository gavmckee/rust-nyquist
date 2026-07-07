/// H2 histogram indexing and error-bound tests for grouping_power=3 (design §3.1).
/// Mirrors rezolus's histogram.h companion tests.
use nyquist_ebpf::libbpf::h2::{BPF_BUCKETS, BPF_GROUPING_POWER, BPF_MAX_VALUE_POWER, buckets_from_counts};
use histogram::Histogram;

fn ref_histogram() -> Histogram {
    Histogram::new(BPF_GROUPING_POWER, BPF_MAX_VALUE_POWER).unwrap()
}

/// Round-trip a single value through the kernel index → Histogram → sparse buckets.
fn round_trip(value: u64) -> (u64, u64) {
    let mut h = ref_histogram();
    h.increment(value).unwrap();
    let counts = h.as_slice().to_vec();
    let buckets = buckets_from_counts(&counts);
    assert_eq!(buckets.len(), 1, "value={value} produced {} buckets", buckets.len());
    buckets[0]
}

// Faithful transliteration of value_to_index() from bpf/histogram.h. The C
// program's indexing MUST agree with the Rust `histogram` crate bucket-for-
// bucket, or kernel-produced counts land in the wrong buckets when userspace
// reconstructs them. The existing tests only exercised the crate; nothing
// guarded the C algorithm — the exact gap that let the `1 << power` int-shift
// UB (now `1ULL << power`) mis-bucket every value >= 2^31 until it was caught.
// Kept in sync by eye with the header; the sweep below is the tripwire.
fn c_value_to_index(value: u64, grouping_power: u8) -> usize {
    if value < (2u64 << grouping_power) {
        return value as usize;
    }
    let power = 63 - value.leading_zeros() as u64; // 63 - clz(value)
    let bin = power - grouping_power as u64 + 1;
    let offset = (value - (1u64 << power)) >> (power - grouping_power as u64);
    (bin * (1u64 << grouping_power) + offset) as usize
}

/// The crate's own index for `value`: increment it and return the single
/// nonzero bucket position. This is value_to_index observed through the
/// public API (the crate's own fn is pub(crate)).
fn crate_index(value: u64) -> usize {
    let mut h = ref_histogram();
    h.increment(value).unwrap();
    h.as_slice()
        .iter()
        .position(|&c| c > 0)
        .unwrap_or_else(|| panic!("value={value} produced no nonzero bucket"))
}

#[test]
fn c_indexing_matches_the_crate_across_the_range() {
    // Boundary-heavy sweep: the linear/exponential seam (2^gp), every
    // power-of-two edge, and crucially the 2^31 region where the int-shift
    // UB lived. u64::MAX must land in the last bucket (495).
    let mut values = vec![0u64, 1, 7, 8, 15, 16, 17, 100, 1000, u64::MAX];
    for p in 3..64 {
        values.push(1u64 << p);
        values.push((1u64 << p) - 1);
        values.push((1u64 << p) + 1);
    }
    for &v in &values {
        let c = c_value_to_index(v, BPF_GROUPING_POWER);
        let rust = crate_index(v);
        assert_eq!(
            c, rust,
            "C value_to_index({v}, {BPF_GROUPING_POWER}) = {c} but crate = {rust}"
        );
    }
    // The documented endpoint invariant.
    assert_eq!(c_value_to_index(u64::MAX, BPF_GROUPING_POWER), BPF_BUCKETS - 1);
}

#[test]
fn bucket_count_is_496() {
    assert_eq!(BPF_BUCKETS, 496);
    let h = ref_histogram();
    assert_eq!(h.as_slice().len(), 496);
}

#[test]
fn small_values_are_exact() {
    // The linear region (values < 2^grouping_power = 8) has width-1 buckets.
    for v in 0u64..8 {
        let (upper, count) = round_trip(v);
        assert_eq!(count, 1, "value={v}");
        assert_eq!(upper, v, "linear region should be exact for value={v}");
    }
}

#[test]
fn upper_bound_is_at_least_value() {
    for &v in &[8u64, 16, 100, 1_000, 10_000, 100_000, 1_000_000, 1_000_000_000, u32::MAX as u64] {
        let (upper, _) = round_trip(v);
        assert!(upper >= v, "upper={upper} < value={v} — bucket ceiling violated");
    }
}

#[test]
fn relative_error_within_grouping_power_bound() {
    // For grouping_power=3, max relative error per bucket = 1/2^3 = 12.5%.
    // The upper_bound is the bucket ceiling so error = (upper - value) / value.
    for &v in &[8u64, 100, 1_000, 10_000, 100_000, 1_000_000, 1_000_000_000] {
        let (upper, _) = round_trip(v);
        let err = (upper as f64 - v as f64) / v as f64;
        assert!(
            err < 0.13,
            "relative error {err:.4} >= 12.5% for value={v}, upper={upper}"
        );
    }
}

#[test]
fn buckets_are_ascending_and_non_empty() {
    // Multiple values spread across the range.
    let mut h = ref_histogram();
    for &v in &[1u64, 50, 1_000, 50_000, 1_000_000] {
        h.increment(v).unwrap();
    }
    let buckets = buckets_from_counts(h.as_slice());
    assert!(buckets.iter().all(|&(_, c)| c > 0), "empty bucket leaked");
    let bounds: Vec<u64> = buckets.iter().map(|&(b, _)| b).collect();
    let mut sorted = bounds.clone();
    sorted.sort_unstable();
    assert_eq!(bounds, sorted, "buckets not ascending");
}

#[test]
fn total_count_is_preserved() {
    let mut h = ref_histogram();
    let inputs = [1u64, 100, 1_000, 10_000, 100_000];
    for &v in &inputs {
        for _ in 0..10 { h.increment(v).unwrap(); }
    }
    let buckets = buckets_from_counts(h.as_slice());
    let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
    assert_eq!(total, (inputs.len() * 10) as u64);
}

#[test]
fn empty_counts_yield_no_buckets() {
    let counts = vec![0u64; BPF_BUCKETS];
    let buckets = buckets_from_counts(&counts);
    assert!(buckets.is_empty(), "empty histogram should produce no buckets");
}

#[test]
fn truncated_counts_are_padded() {
    // buckets_from_counts pads short slices to BPF_BUCKETS.
    let counts = vec![0u64; 10]; // much shorter than 496
    let buckets = buckets_from_counts(&counts);
    assert!(buckets.is_empty());
}
