use std::time::SystemTime;
use crate::model::{Kind, Labels, Unit};

#[derive(Clone, Debug)]
pub struct MetricSnapshot {
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    pub labels: Labels,
    pub raw: u64,
    pub percentiles: Vec<(f64, u64)>,
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
    fn snapshot_contains_raw_and_percentiles() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("g", Kind::Gauge));
        let t0 = Instant::now();
        for i in 0..50u64 { reg.record_gauge(id, t0 + Duration::from_millis(i * 10), 7); }
        let snap = reg.snapshot(t0 + Duration::from_millis(500), &[50.0, 99.0]);
        let m = snap.metrics.iter().find(|m| m.name == "g").unwrap();
        assert_eq!(m.raw, 7);
        assert_eq!(m.percentiles.len(), 2);
        assert_eq!(m.percentiles[0].0, 50.0);
        // Value 7 fits in a small bucket; accept ~1% relative error
        assert!((7..=8).contains(&m.percentiles[0].1), "p50 was {}", m.percentiles[0].1);
    }
}
