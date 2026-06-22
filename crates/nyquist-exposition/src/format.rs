use nyquist_core::model::{Kind, Labels};
use nyquist_core::snapshot::RegistrySnapshot;

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c == '/' || c == '-' { '_' } else { c }).collect()
}

fn fmt_pct(p: f64) -> String {
    if p.fract() == 0.0 { format!("{}", p as u64) } else { format!("{p}") }
}

fn label_str(labels: &Labels, extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
    if let Some((k, v)) = extra { parts.push(format!("{k}=\"{v}\"")); }
    if parts.is_empty() { String::new() } else { format!("{{{}}}", parts.join(",")) }
}

pub fn to_prometheus(snap: &RegistrySnapshot) -> String {
    let mut out = String::new();
    for m in &snap.metrics {
        let base = sanitize(&m.name);
        let kind = match m.kind { Kind::Counter => "counter", Kind::Gauge => "gauge", Kind::Distribution => "summary" };
        out.push_str(&format!("# TYPE {base} {kind}\n"));
        out.push_str(&format!("{base}{} {}\n", label_str(&m.labels, None), m.raw));
        let suffix = match m.kind { Kind::Gauge => "value", _ => "rate" };
        for (p, v) in &m.percentiles {
            out.push_str(&format!(
                "{base}_{suffix}{} {}\n",
                label_str(&m.labels, Some(("percentile", &fmt_pct(*p)))),
                v
            ));
        }
    }
    out
}

pub fn to_json(snap: &RegistrySnapshot) -> String {
    let mut items = Vec::new();
    for m in &snap.metrics {
        let pcts: Vec<String> = m.percentiles.iter()
            .map(|(p, v)| format!("\"{}\":{}", fmt_pct(*p), v)).collect();
        items.push(format!(
            "{{\"name\":\"{}\",\"raw\":{},\"percentiles\":{{{}}}}}",
            m.name, m.raw, pcts.join(",")
        ));
    }
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
    use std::time::SystemTime;

    fn sample_snapshot() -> RegistrySnapshot {
        RegistrySnapshot {
            captured: SystemTime::UNIX_EPOCH,
            metrics: vec![MetricSnapshot {
                name: "net/tx-bytes".into(),
                kind: Kind::Counter,
                unit: Unit::Bytes,
                labels: Labels::new().insert("dev", "eth0"),
                raw: 50_000,
                percentiles: vec![(50.0, 100_000), (99.0, 900_000)],
            }],
        }
    }

    #[test]
    fn prometheus_emits_raw_and_percentiles() {
        let out = to_prometheus(&sample_snapshot());
        assert!(out.contains("net_tx_bytes{dev=\"eth0\"} 50000"), "{out}");
        assert!(out.contains("net_tx_bytes_rate{dev=\"eth0\",percentile=\"99\"} 900000"), "{out}");
        assert!(out.contains("# TYPE net_tx_bytes counter"), "{out}");
    }

    #[test]
    fn json_contains_metric_fields() {
        let out = to_json(&sample_snapshot());
        assert!(out.contains("\"net/tx-bytes\""), "{out}");
        assert!(out.contains("\"raw\":50000"), "{out}");
    }
}
