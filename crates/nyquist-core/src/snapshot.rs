use std::time::SystemTime;
use crate::model::{Kind, Labels, Unit};

#[derive(Clone, Debug)]
pub struct MetricSnapshot {
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    pub labels: Labels,
    pub raw: u64,
    /// Sparse H2 bucket array `(upper_bound, count)`, ascending. Consumers
    /// compute percentiles from this via `percentiles::percentiles_from_buckets`.
    pub buckets: Vec<(u64, u64)>,
}

#[derive(Clone, Debug)]
pub struct RegistrySnapshot {
    pub metrics: Vec<MetricSnapshot>,
    pub captured: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;
    use crate::registry::{MetricDef, Registry};
    use std::time::{Duration, Instant};

    #[test]
    fn snapshot_contains_raw_and_buckets() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("g", Kind::Gauge));
        let t0 = Instant::now();
        for i in 0..50u64 { reg.record_gauge(id, t0 + Duration::from_millis(i * 10), 7); }
        let snap = reg.snapshot(t0 + Duration::from_millis(500));
        let m = snap.metrics.iter().find(|m| m.name == "g").unwrap();
        assert_eq!(m.raw, 7);
        let total: u64 = m.buckets.iter().map(|&(_, c)| c).sum();
        assert!(total > 0, "no buckets recorded");
        // Recover p50 via the consumer helper; value 7 fits a small bucket.
        let pcts = crate::percentiles::percentiles_from_buckets(&m.buckets, &[50.0]);
        assert!((7..=8).contains(&pcts[0].1), "p50 was {}", pcts[0].1);
    }
}
