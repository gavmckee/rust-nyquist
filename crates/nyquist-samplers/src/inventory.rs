//! Sampler registration now lives in `nyquist_core::registration` via a
//! `linkme` distributed slice. Each sampler registers itself in its own module
//! via `#[distributed_slice(SAMPLERS)]`. This module re-exports the iterator
//! entry points for back-compat with `main.rs`.
pub use nyquist_core::registration::{all_sampler_names, build_enabled};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use nyquist_core::registry::Registry;

    #[test]
    fn respects_enabled_flag() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let samplers = build_enabled(
            &reg,
            Duration::from_millis(10),
            |name| name != "network",
            |_| None,
        );
        let names: Vec<_> = samplers.iter().map(|s| s.name().to_string()).collect();
        assert!(!names.iter().any(|n| n == "network"), "network should be disabled: {names:?}");
        assert!(names.iter().any(|n| n == "cpu"), "cpu should be enabled: {names:?}");
    }

    #[test]
    fn slice_contains_registered_samplers() {
        let names = nyquist_core::registration::all_sampler_names();
        assert!(names.contains(&"cpu"), "cpu not registered: {names:?}");
    }

    #[test]
    fn all_expected_samplers_registered() {
        let mut names = nyquist_core::registration::all_sampler_names();
        names.sort_unstable();
        for expected in ["cpu", "disk", "ip", "loadavg", "memory", "netstat", "network",
                         "nic_stats", "psi", "sockstat", "softirqs", "tcp", "tcpinfo", "udp"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }
}
