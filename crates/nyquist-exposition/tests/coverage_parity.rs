//! Parity gate (design §6.5): the live emitted base-metric set must remain a
//! superset of the committed golden baseline. Migrations may ADD tuples; the
//! baseline set must never drop.
use nyquist_core::coverage::parse_prometheus_tuples;

const GOLDEN: &str = include_str!("fixtures/golden_metrics.txt");

#[test]
fn golden_fixture_parses_nonempty() {
    // Sanity: the parser is deterministic and the fixture is non-empty.
    let golden = parse_prometheus_tuples(GOLDEN);
    assert!(!golden.is_empty(), "golden baseline fixture is empty");
    // The reference-slice TCP tuples must be in the baseline.
    assert!(golden.iter().any(|(n, _)| n == "tcp_rtt_us"), "tcp_rtt_us missing from baseline");
    assert!(golden.iter().any(|(n, _)| n == "tcp_retransmits"), "tcp_retransmits missing from baseline");
}

/// When a live capture is available (env `NYQUIST_LIVE_METRICS` points to a
/// /metrics dump), assert the baseline is a subset of the live set. Skipped if
/// unset so the test runs on macOS without a live agent.
#[test]
fn golden_baseline_never_drops() {
    let live_path = match std::env::var("NYQUIST_LIVE_METRICS") {
        Ok(p) => p,
        Err(_) => return, // no live capture available in this environment
    };
    let live = std::fs::read_to_string(&live_path).expect("read live metrics");
    let golden = parse_prometheus_tuples(GOLDEN);
    let live = parse_prometheus_tuples(&live);
    let dropped: Vec<_> = golden.difference(&live).collect();
    assert!(dropped.is_empty(), "coverage regression — dropped tuples: {dropped:?}");
}
