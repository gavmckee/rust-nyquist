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

        // A sample line is `name{labels} value [timestamp]`. The head is
        // everything up to the first whitespace OUTSIDE the label braces —
        // rsplit_once(' ') mis-split lines carrying the optional trailing
        // timestamp, and label values can legitimately contain spaces.
        let head_end = {
            let (mut in_braces, mut end) = (false, line.len());
            for (i, c) in line.char_indices() {
                match c {
                    '{' => in_braces = true,
                    '}' => in_braces = false,
                    ' ' if !in_braces => { end = i; break; }
                    _ => {}
                }
            }
            end
        };
        let head = &line[..head_end];

        let (name, labels) = match head.split_once('{') {
            Some((n, rest)) => (n, rest.trim_end_matches('}')),
            None => (head, ""),
        };
        // Strip the synthetic percentile suffix at most ONCE (trim_end_matches
        // looped, mangling a base name that legitimately ends in _rate/_value).
        let base = name
            .strip_suffix("_rate")
            .or_else(|| name.strip_suffix("_value"))
            .unwrap_or(name)
            .to_string();

        let mut keys: Vec<String> = split_labels(labels)
            .into_iter()
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.trim().to_string()))
            .filter(|k| k != "percentile" && !k.is_empty())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        out.insert((base, keys));
    }
    out
}

/// Split a Prometheus label list on commas OUTSIDE quotes — a label value
/// like `device="a,b"` must not split into two keys.
fn split_labels(labels: &str) -> Vec<&str> {
    let (mut out, mut in_quotes, mut start) = (Vec::new(), false, 0);
    for (i, b) in labels.bytes().enumerate() {
        match b {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => { out.push(&labels[start..i]); start = i + 1; }
            _ => {}
        }
    }
    if start < labels.len() { out.push(&labels[start..]); }
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

    #[test]
    fn parser_handles_timestamps_quoted_commas_and_repeated_suffix() {
        let text = "\
disk_read_rate{device=\"sda\"} 42 1699999999000\n\
net_bytes{path=\"a,b\",iface=\"eth0\"} 7\n\
some_value_value{host=\"h\"} 3\n";
        let got = parse_prometheus_tuples(text);
        // Trailing timestamp doesn't corrupt the head; only "_rate" strips
        // (once), leaving the real base name intact.
        assert!(got.contains(&("disk_read".to_string(), vec!["device".to_string()])),
            "timestamp or single-suffix strip failed: {got:?}");
        // Quoted comma stays inside one label value → two keys, not three.
        assert!(got.contains(&(
            "net_bytes".to_string(),
            vec!["iface".to_string(), "path".to_string()],
        )), "quoted-comma label split wrong: {got:?}");
        // "_value" stripped exactly once → "some_value", not "some".
        assert!(got.contains(&("some_value".to_string(), vec!["host".to_string()])),
            "repeated-suffix strip mangled base name: {got:?}");
    }
}
