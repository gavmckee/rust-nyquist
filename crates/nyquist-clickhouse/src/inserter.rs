/// Converts a `MetricSnapshot` into a ClickHouse INSERT row.
///
/// One row per unique (metric_name, labels) combination per snapshot.
/// All four percentiles are stored as separate columns — no label fanout
/// needed for percentile queries, unlike the Prometheus/PromQL model.

use nyquist_core::snapshot::MetricSnapshot;
use nyquist_core::percentiles::percentiles_from_buckets;
use crate::grouping::metric_name;

pub struct NarrowRow {
    pub ts_ms: i64,
    pub name:  String,
    /// ClickHouse Map literal: {'key': 'val', ...}
    pub tags:  String,
    pub raw:   u64,
    pub p50:   u64,
    pub p90:   u64,
    pub p99:   u64,
    pub p999:  u64,
}

impl NarrowRow {
    pub fn from_snapshot(ts_ms: i64, m: &MetricSnapshot) -> Self {
        // The ClickHouse schema has fixed percentile columns, so the percentile
        // set here is fixed; values are computed consumer-side from the bucket
        // array (design §3.4).
        let p = percentiles_from_buckets(&m.buckets, &[50.0, 90.0, 99.0, 99.9]);
        let get = |target: f64| p.iter()
            .find(|(pp, _)| (*pp - target).abs() < 0.001)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        NarrowRow {
            ts_ms,
            name: metric_name(&m.name),
            tags: map_literal(&m.labels),
            raw:  m.raw,
            p50:  get(50.0),
            p90:  get(90.0),
            p99:  get(99.0),
            p999: get(99.9),
        }
    }

    /// Append this row as a VALUES tuple to `buf` (comma-separated, caller
    /// handles leading commas between rows).
    pub fn append_to(&self, buf: &mut String) {
        // toDateTime64(int, precision) treats the integer as SECONDS regardless of
        // precision, so 1782077908234 ms would overflow to year 2299.
        // fromUnixTimestamp64Milli() is the correct function for ms-precision epoch values.
        buf.push_str(&format!(
            "(fromUnixTimestamp64Milli({ts_ms}, 'UTC'), '{name}', {tags}, {raw}, {p50}, {p90}, {p99}, {p999})",
            ts_ms = self.ts_ms,
            name  = esc(&self.name),
            tags  = self.tags,
            raw   = self.raw,
            p50   = self.p50,
            p90   = self.p90,
            p99   = self.p99,
            p999  = self.p999,
        ));
    }
}

fn map_literal(labels: &nyquist_core::model::Labels) -> String {
    let pairs: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("'{}': '{}'", esc(k), esc(v)))
        .collect();
    format!("{{{}}}", pairs.join(", "))
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};

    fn make_snap(name: &str, labels: Labels, raw: u64, buckets: Vec<(u64, u64)>) -> MetricSnapshot {
        MetricSnapshot {
            name: name.to_string(),
            kind: Kind::Counter,
            unit: Unit::Count,
            labels,
            raw,
            buckets,
        }
    }

    #[test]
    fn basic_row_fields() {
        // total 1000; ranks land so p50->10, p90->15, p99->30, p99.9->31.
        let buckets = vec![(10, 600), (15, 300), (30, 98), (31, 2)];
        let m = make_snap("cpu/usage/user", Labels::new(), 42, buckets);
        let row = NarrowRow::from_snapshot(1_000, &m);
        assert_eq!(row.name, "cpu_usage_user");
        assert_eq!(row.raw, 42);
        assert_eq!(row.p50, 10);
        assert_eq!(row.p90, 15);
        assert_eq!(row.p99, 30);
        assert_eq!(row.p999, 31);
        assert_eq!(row.tags, "{}");
    }

    #[test]
    fn labels_become_map_literal() {
        let labels = Labels::new().insert("iface", "ens1f1np1").insert("queue", "48");
        let m = make_snap("nic/queue/rx_packets", labels, 0, vec![]);
        let row = NarrowRow::from_snapshot(0, &m);
        // BTreeMap iterates in key order
        assert_eq!(row.tags, "{'iface': 'ens1f1np1', 'queue': '48'}");
    }

    #[test]
    fn values_sql_is_well_formed() {
        let m = make_snap("net/rx", Labels::new().insert("iface", "eth0"), 5, vec![(9, 10)]);
        let row = NarrowRow::from_snapshot(1_700_000_000_000, &m);
        let mut buf = String::new();
        row.append_to(&mut buf);
        assert!(buf.starts_with("(fromUnixTimestamp64Milli(1700000000000"));
        assert!(buf.contains("'net_rx'"));
        assert!(buf.contains("'iface': 'eth0'"));
    }

    #[test]
    fn apostrophe_in_label_is_escaped() {
        let labels = Labels::new().insert("desc", "it's a test");
        let m = make_snap("x", labels, 0, vec![]);
        let row = NarrowRow::from_snapshot(0, &m);
        assert!(row.tags.contains("\\'"), "apostrophe must be escaped: {}", row.tags);
    }
}
