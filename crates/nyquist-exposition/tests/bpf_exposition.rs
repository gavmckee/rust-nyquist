/// Verify that BPF-sourced bucket arrays flow correctly through the exposition
/// layer and produce percentile values within the H2 error bound (design §3.4).
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
use nyquist_core::percentiles::percentiles_from_buckets;
use nyquist_exposition::format::{to_prometheus, to_json};
use histogram::Histogram;
use std::time::SystemTime;

const BPF_GP: u8 = 3;
const BPF_MVP: u8 = 64;

/// Build a RegistrySnapshot from raw kernel bucket counts (as if mmap'd from BPF).
fn snapshot_from_bpf_counts(name: &str, counts: &[u64]) -> RegistrySnapshot {
    // Reconstruct via the same path the BPF sampler uses.
    let buckets = nyquist_ebpf::libbpf::h2::buckets_from_counts(counts);
    RegistrySnapshot {
        captured: SystemTime::UNIX_EPOCH,
        metrics: vec![MetricSnapshot {
            name: name.to_string(),
            kind: Kind::Distribution,
            unit: Unit::None,
            labels: Labels::new(),
            raw: counts.iter().sum(),
            buckets,
        }],
    }
}

/// Build counts from a reference Histogram for testing.
fn counts_for_value(value: u64, n: u64) -> Vec<u64> {
    let mut h = Histogram::new(BPF_GP, BPF_MVP).unwrap();
    for _ in 0..n { h.increment(value).unwrap(); }
    h.as_slice().to_vec()
}

#[test]
fn prometheus_output_contains_metric_name_and_type() {
    let counts = counts_for_value(1_000, 100);
    let snap = snapshot_from_bpf_counts("tcp/rtt_us", &counts);
    let out = to_prometheus(&snap, &[50.0, 99.0]);
    assert!(out.contains("tcp_rtt_us"), "metric name missing:\n{out}");
    assert!(out.contains("# TYPE tcp_rtt_us"), "TYPE declaration missing:\n{out}");
    assert!(out.contains("percentile=\"50\""), "p50 label missing:\n{out}");
    assert!(out.contains("percentile=\"99\""), "p99 label missing:\n{out}");
}

#[test]
fn percentile_values_within_h2_error_bound() {
    // Insert 1000 observations of ~10_000 ns RTT into a BPF-style histogram.
    let target = 10_000u64;
    let counts = counts_for_value(target, 1000);
    let snap = snapshot_from_bpf_counts("tcp/rtt_us", &counts);
    let m = &snap.metrics[0];

    let pcts = percentiles_from_buckets(&m.buckets, &[50.0, 90.0, 99.0]);
    for (p, v) in &pcts {
        assert!(*v >= target, "p{p} value={v} < target={target} (bucket ceiling violated)");
        let err = (*v as f64 - target as f64) / target as f64;
        assert!(err < 0.13, "p{p} relative error {err:.4} > 12.5% (gp=3 bound): value={v}, target={target}");
    }
    // All same input → all percentiles should be in the same bucket.
    assert_eq!(pcts[0].1, pcts[1].1, "p50 and p90 should be same bucket for uniform input");
    assert_eq!(pcts[1].1, pcts[2].1, "p90 and p99 should be same bucket for uniform input");
}

#[test]
fn raw_field_is_total_observation_count() {
    let counts = counts_for_value(500, 42);
    let snap = snapshot_from_bpf_counts("tcp/rtt_us", &counts);
    assert_eq!(snap.metrics[0].raw, 42, "raw should equal total observation count");
}

#[test]
fn empty_bpf_histogram_produces_zero_percentiles() {
    let counts = vec![0u64; 496];
    let snap = snapshot_from_bpf_counts("tcp/rtt_us", &counts);
    let pcts = percentiles_from_buckets(&snap.metrics[0].buckets, &[50.0, 99.0]);
    assert_eq!(pcts[0].1, 0, "empty histogram p50 should be 0");
    assert_eq!(pcts[1].1, 0, "empty histogram p99 should be 0");
}

#[test]
fn json_output_contains_metric_and_raw() {
    let counts = counts_for_value(1_000, 50);
    let snap = snapshot_from_bpf_counts("tcp/rtt_us", &counts);
    let out = to_json(&snap, &[50.0]);
    assert!(out.contains("\"tcp/rtt_us\""), "metric name missing in JSON:\n{out}");
    assert!(out.contains("\"raw\":50"), "raw count missing in JSON:\n{out}");
}

#[test]
fn mixed_rtts_produce_monotonic_percentiles() {
    // Mix of fast (100ns) and slow (1_000_000ns) observations.
    let mut h = Histogram::new(BPF_GP, BPF_MVP).unwrap();
    for _ in 0..900 { h.increment(100).unwrap(); }
    for _ in 0..100 { h.increment(1_000_000).unwrap(); }
    let counts = h.as_slice().to_vec();
    let snap = snapshot_from_bpf_counts("tcp/rtt_us", &counts);
    let pcts = percentiles_from_buckets(&snap.metrics[0].buckets, &[50.0, 90.0, 99.0]);
    let vals: Vec<u64> = pcts.iter().map(|&(_, v)| v).collect();
    let mut sorted = vals.clone();
    sorted.sort_unstable();
    assert_eq!(vals, sorted, "percentiles not monotonic: {vals:?}");
    // p50 should be near 100 (fast bucket), p99 near 1_000_000 (slow bucket).
    assert!(pcts[0].1 <= 200, "p50={} should be in fast bucket", pcts[0].1);
    assert!(pcts[2].1 >= 900_000, "p99={} should be in slow bucket", pcts[2].1);
}
