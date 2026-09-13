use nyquist_core::model::{Kind, Labels};
use nyquist_core::snapshot::RegistrySnapshot;
use nyquist_core::percentiles::percentiles_from_buckets;

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c == '/' || c == '-' { '_' } else { c }).collect()
}

fn fmt_pct(p: f64) -> String {
    if p.fract() == 0.0 { format!("{}", p as u64) } else { format!("{p}") }
}

fn escape_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn label_str(labels: &Labels, extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{}\"", escape_label(v))).collect();
    if let Some((k, v)) = extra { parts.push(format!("{k}=\"{}\"", escape_label(v))); }
    if parts.is_empty() { String::new() } else { format!("{{{}}}", parts.join(",")) }
}

pub fn to_prometheus(snap: &RegistrySnapshot, percentiles: &[f64]) -> String {
    // Collect complete families so metadata appears once and all samples of a
    // family are contiguous. Distribution raw values are cumulative counts,
    // not summaries; the count suffix separates them from same-name gauges.
    let mut families: std::collections::BTreeMap<String, (&str, Vec<String>)> =
        std::collections::BTreeMap::new();
    for m in &snap.metrics {
        let base = sanitize(&m.name);
        let (raw_name, kind) = match m.kind {
            Kind::Counter => (base.clone(), "counter"),
            Kind::Gauge => (base.clone(), "gauge"),
            Kind::Distribution => (format!("{base}_count"), "counter"),
        };
        families.entry(raw_name.clone()).or_insert((kind, Vec::new())).1.push(
            format!("{raw_name}{} {}\n", label_str(&m.labels, None), m.raw));
        let suffix = match m.kind { Kind::Gauge => "value", _ => "rate" };
        let name = format!("{base}_{suffix}");
        for (p, v) in percentiles_from_buckets(&m.buckets, percentiles) {
            families.entry(name.clone()).or_insert(("gauge", Vec::new())).1.push(format!(
                "{name}{} {}\n", label_str(&m.labels, Some(("percentile", &fmt_pct(p)))), v));
        }
    }
    let mut out = String::new();
    for (name, (kind, samples)) in families {
        out.push_str(&format!("# TYPE {name} {kind}\n"));
        for sample in samples { out.push_str(&sample); }
    }
    out
}

pub fn to_json(snap: &RegistrySnapshot, percentiles: &[f64]) -> String {
    let items: Vec<_> = snap.metrics.iter().map(|m| {
        let labels: std::collections::BTreeMap<_, _> = m.labels.iter().collect();
        let pcts: std::collections::BTreeMap<_, _> =
            percentiles_from_buckets(&m.buckets, percentiles).into_iter()
                .map(|(p, v)| (fmt_pct(p), v)).collect();
        serde_json::json!({"name": m.name, "labels": labels, "raw": m.raw, "percentiles": pcts})
    }).collect();
    serde_json::to_string(&items).expect("serializable metric values")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
    use std::time::SystemTime;

    #[test]
    fn families_are_unique_and_mixed_sources_have_consistent_types() {
        let mut snap = sample_snapshot();
        let mut second = snap.metrics[0].clone();
        second.labels = Labels::new().insert("dev", "eth1");
        snap.metrics.push(second);
        let mut dist = snap.metrics[0].clone();
        dist.name = "tcp/rtt_us".into();
        dist.kind = Kind::Distribution;
        dist.labels = Labels::new().insert("source", "ebpf");
        snap.metrics.push(dist.clone());
        dist.kind = Kind::Gauge;
        dist.labels = Labels::new().insert("port", "443");
        snap.metrics.push(dist);
        let out = to_prometheus(&snap, &[99.0]);
        let mut types = std::collections::HashSet::new();
        for line in out.lines().filter(|l| l.starts_with("# TYPE")) {
            assert!(types.insert(line.split_whitespace().nth(2).unwrap()), "duplicate family: {out}");
        }
        assert!(out.contains("# TYPE tcp_rtt_us gauge"));
        assert!(out.contains("# TYPE tcp_rtt_us_count counter"));
        assert!(out.contains("# TYPE tcp_rtt_us_rate gauge"));
        assert_eq!(out.matches("# TYPE net_tx_bytes counter").count(), 1);
    }

    #[test]
    fn json_roundtrips_labels_and_escaping() {
        let mut snap = sample_snapshot();
        snap.metrics[0].name = "a\"b".into();
        snap.metrics[0].labels = Labels::new().insert("dev", "a\"b\\c\nd");
        let parsed: serde_json::Value = serde_json::from_str(&to_json(&snap, &[99.0])).unwrap();
        assert_eq!(parsed[0]["name"], "a\"b");
        assert_eq!(parsed[0]["labels"]["dev"], "a\"b\\c\nd");
        assert_eq!(escape_label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    fn sample_snapshot() -> RegistrySnapshot {
        RegistrySnapshot {
            captured: SystemTime::UNIX_EPOCH,
            metrics: vec![MetricSnapshot {
                name: "net/tx-bytes".into(),
                kind: Kind::Counter,
                unit: Unit::Bytes,
                labels: Labels::new().insert("dev", "eth0"),
                raw: 50_000,
                // 90 samples at 100_000, 10 at 900_000
                buckets: vec![(100_000, 90), (900_000, 10)],
            }],
        }
    }

    #[test]
    fn prometheus_emits_raw_and_percentiles() {
        let out = to_prometheus(&sample_snapshot(), &[50.0, 99.0]);
        assert!(out.contains("net_tx_bytes{dev=\"eth0\"} 50000"), "{out}");
        assert!(out.contains("net_tx_bytes_rate{dev=\"eth0\",percentile=\"99\"} 900000"), "{out}");
        assert!(out.contains("# TYPE net_tx_bytes counter"), "{out}");
    }

    #[test]
    fn json_contains_metric_fields() {
        let out = to_json(&sample_snapshot(), &[50.0, 99.0]);
        assert!(out.contains("\"net/tx-bytes\""), "{out}");
        assert!(out.contains("\"raw\":50000"), "{out}");
    }
}
