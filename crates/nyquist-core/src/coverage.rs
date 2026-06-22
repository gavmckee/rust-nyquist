//! Coverage parity (design §6): reduce emitted metrics to the canonical
//! `(name, label-key set, kind, unit)` tuple set so a parity gate can assert
//! the baseline never shrinks.
use std::collections::BTreeSet;
use crate::model::{Kind, Unit};
use crate::snapshot::RegistrySnapshot;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tuple {
    pub name: String,
    pub label_keys: Vec<String>,
    pub kind: Kind,
    pub unit: Unit,
}

/// Reduce a snapshot to its canonical `(name, label-key set, kind, unit)` tuples.
pub fn canonical_tuples(snap: &RegistrySnapshot) -> BTreeSet<Tuple> {
    snap.metrics
        .iter()
        .map(|m| {
            let mut label_keys: Vec<String> = m.labels.iter().map(|(k, _)| k.clone()).collect();
            label_keys.sort_unstable();
            Tuple { name: m.name.clone(), label_keys, kind: m.kind, unit: m.unit }
        })
        .collect()
}

/// Parse `(metric_base_name, sorted_label_keys)` pairs from a Prometheus text
/// capture, ignoring the synthetic `percentile` label and `_rate`/`_value`
/// suffixes so the comparison is against base metric identity.
pub fn parse_prometheus_tuples(text: &str) -> BTreeSet<(String, Vec<String>)> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        // name{labels} value   OR   name value
        let (head, _value) = match line.rsplit_once(' ') {
            Some(x) => x,
            None => continue,
        };
        let (name, labels) = match head.split_once('{') {
            Some((n, rest)) => (n, rest.trim_end_matches('}')),
            None => (head, ""),
        };
        let base = name
            .trim_end_matches("_rate")
            .trim_end_matches("_value")
            .to_string();
        let mut keys: Vec<String> = labels
            .split(',')
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.trim().to_string()))
            .filter(|k| k != "percentile" && !k.is_empty())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        out.insert((base, keys));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Labels, Unit};
    use crate::snapshot::MetricSnapshot;
    use std::time::SystemTime;

    fn snap(metrics: Vec<MetricSnapshot>) -> RegistrySnapshot {
        RegistrySnapshot { metrics, captured: SystemTime::UNIX_EPOCH }
    }

    #[test]
    fn reduces_to_name_labels_kind_unit() {
        let s = snap(vec![MetricSnapshot {
            name: "cpu/usage".into(), kind: Kind::Counter, unit: Unit::Count,
            labels: Labels::new().insert("cpu", "0").insert("state", "user"),
            raw: 1, buckets: vec![],
        }]);
        let t = canonical_tuples(&s);
        assert_eq!(t.len(), 1);
        let only = t.iter().next().unwrap();
        assert_eq!(only.name, "cpu/usage");
        assert_eq!(only.label_keys, vec!["cpu".to_string(), "state".to_string()]);
    }

    #[test]
    fn prometheus_parser_strips_suffixes_and_percentile_label() {
        let text = "\
# TYPE cpu_usage counter
cpu_usage{cpu=\"0\",state=\"user\"} 42
cpu_usage_rate{cpu=\"0\",state=\"user\",percentile=\"99\"} 100
mem_free_value{} 2048
";
        let got = parse_prometheus_tuples(text);
        // cpu_usage (base) with keys [cpu, state]; mem_free with no keys.
        assert!(got.contains(&("cpu_usage".to_string(), vec!["cpu".to_string(), "state".to_string()])));
        assert!(got.contains(&("mem_free".to_string(), vec![])));
        // percentile label never leaks into the key set.
        assert!(got.iter().all(|(_, keys)| !keys.iter().any(|k| k == "percentile")));
    }
}
