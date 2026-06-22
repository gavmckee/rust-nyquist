use std::sync::Arc;
use arrow_array::array::{Int64Array, StringArray, UInt64Array};
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::snapshot::MetricSnapshot;

pub fn nyquist_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_unix_ms",  DataType::Int64,  false),
        Field::new("name",        DataType::Utf8,   false),
        Field::new("labels_json", DataType::Utf8,   false),
        Field::new("kind",        DataType::Utf8,   false),
        Field::new("unit",        DataType::Utf8,   false),
        Field::new("raw",         DataType::UInt64, false),
        Field::new("p50",         DataType::UInt64, false),
        Field::new("p90",         DataType::UInt64, false),
        Field::new("p99",         DataType::UInt64, false),
        Field::new("p99_9",       DataType::UInt64, false),
    ]))
}

pub fn labels_json(labels: &Labels) -> String {
    let mut buf = String::from("{");
    for (i, (k, v)) in labels.iter().enumerate() {
        if i > 0 { buf.push(','); }
        buf.push('"');
        buf.push_str(k);
        buf.push_str("\":\"");
        buf.push_str(v);
        buf.push('"');
    }
    buf.push('}');
    buf
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Counter      => "counter",
        Kind::Gauge        => "gauge",
        Kind::Distribution => "distribution",
    }
}

fn unit_str(u: Unit) -> &'static str {
    match u {
        Unit::Bytes   => "bytes",
        Unit::Count   => "count",
        Unit::Seconds => "seconds",
        Unit::Percent => "percent",
        Unit::None    => "none",
    }
}

fn find_pct(percentiles: &[(f64, u64)], target: f64) -> u64 {
    percentiles.iter()
        .find(|(p, _)| (*p - target).abs() < 0.001)
        .map(|(_, v)| *v)
        .unwrap_or(0)
}

pub struct RowAccumulator {
    ts_unix_ms:  Vec<i64>,
    name:        Vec<String>,
    labels_json: Vec<String>,
    kind:        Vec<String>,
    unit:        Vec<String>,
    raw:         Vec<u64>,
    p50:         Vec<u64>,
    p90:         Vec<u64>,
    p99:         Vec<u64>,
    p99_9:       Vec<u64>,
}

impl RowAccumulator {
    pub fn new() -> Self {
        RowAccumulator {
            ts_unix_ms:  Vec::new(),
            name:        Vec::new(),
            labels_json: Vec::new(),
            kind:        Vec::new(),
            unit:        Vec::new(),
            raw:         Vec::new(),
            p50:         Vec::new(),
            p90:         Vec::new(),
            p99:         Vec::new(),
            p99_9:       Vec::new(),
        }
    }

    pub fn push(&mut self, ts: i64, m: &MetricSnapshot) {
        self.ts_unix_ms.push(ts);
        self.name.push(m.name.clone());
        self.labels_json.push(labels_json(&m.labels));
        self.kind.push(kind_str(m.kind).to_string());
        self.unit.push(unit_str(m.unit).to_string());
        self.raw.push(m.raw);
        self.p50.push(find_pct(&m.percentiles, 50.0));
        self.p90.push(find_pct(&m.percentiles, 90.0));
        self.p99.push(find_pct(&m.percentiles, 99.0));
        self.p99_9.push(find_pct(&m.percentiles, 99.9));
    }

    pub fn len(&self) -> usize { self.ts_unix_ms.len() }
    pub fn is_empty(&self) -> bool { self.ts_unix_ms.is_empty() }

    pub fn drain(&mut self) -> RecordBatch {
        let schema = nyquist_schema();
        RecordBatch::try_new(schema, vec![
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_unix_ms))),
            Arc::new(StringArray::from(std::mem::take(&mut self.name))),
            Arc::new(StringArray::from(std::mem::take(&mut self.labels_json))),
            Arc::new(StringArray::from(std::mem::take(&mut self.kind))),
            Arc::new(StringArray::from(std::mem::take(&mut self.unit))),
            Arc::new(UInt64Array::from(std::mem::take(&mut self.raw))),
            Arc::new(UInt64Array::from(std::mem::take(&mut self.p50))),
            Arc::new(UInt64Array::from(std::mem::take(&mut self.p90))),
            Arc::new(UInt64Array::from(std::mem::take(&mut self.p99))),
            Arc::new(UInt64Array::from(std::mem::take(&mut self.p99_9))),
        ]).expect("schema matches column types")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::MetricSnapshot;

    fn make_metric(name: &str, raw: u64, p50: u64, p90: u64, p99: u64, p99_9: u64) -> MetricSnapshot {
        MetricSnapshot {
            name: name.to_string(),
            kind: Kind::Counter,
            unit: Unit::Bytes,
            labels: Labels::new(),
            raw,
            percentiles: vec![(50.0, p50), (90.0, p90), (99.0, p99), (99.9, p99_9)],
        }
    }

    #[test]
    fn labels_json_sorted_keys() {
        let labels = Labels::new().insert("z", "1").insert("a", "2");
        let json = labels_json(&labels);
        assert_eq!(json, r#"{"a":"2","z":"1"}"#);
    }

    #[test]
    fn labels_json_empty() {
        let json = labels_json(&Labels::new());
        assert_eq!(json, "{}");
    }

    #[test]
    fn accumulator_push_and_drain() {
        let mut acc = RowAccumulator::new();
        assert!(acc.is_empty());
        let m = make_metric("cpu/usage/user", 42, 10, 20, 30, 35);
        acc.push(1_000_000, &m);
        assert_eq!(acc.len(), 1);
        let batch = acc.drain();
        assert_eq!(batch.num_rows(), 1);
        assert!(acc.is_empty());
        use arrow_array::array::Int64Array;
        let ts_col = batch.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(ts_col.value(0), 1_000_000);
    }

    #[test]
    fn accumulator_drain_multiple_rows() {
        let mut acc = RowAccumulator::new();
        acc.push(1000, &make_metric("m1", 1, 1, 1, 1, 1));
        acc.push(2000, &make_metric("m2", 2, 2, 2, 2, 2));
        let batch = acc.drain();
        assert_eq!(batch.num_rows(), 2);
    }

    #[test]
    fn missing_percentile_defaults_to_zero() {
        let m = MetricSnapshot {
            name: "g".to_string(),
            kind: Kind::Gauge,
            unit: Unit::None,
            labels: Labels::new(),
            raw: 7,
            percentiles: vec![(50.0, 7)],
        };
        let mut acc = RowAccumulator::new();
        acc.push(0, &m);
        let batch = acc.drain();
        use arrow_array::array::UInt64Array;
        // p99 is column index 8
        let p99_col = batch.column(8).as_any().downcast_ref::<UInt64Array>().unwrap();
        assert_eq!(p99_col.value(0), 0);
    }
}
