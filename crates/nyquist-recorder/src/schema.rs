use std::sync::Arc;
use arrow_array::array::{Int64Array, StringArray, UInt64Array};
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::snapshot::MetricSnapshot;

pub fn nyquist_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_unix_ms",   DataType::Int64,  false),
        Field::new("name",         DataType::Utf8,   false),
        Field::new("labels_json",  DataType::Utf8,   false),
        Field::new("kind",         DataType::Utf8,   false),
        Field::new("unit",         DataType::Utf8,   false),
        Field::new("raw",          DataType::UInt64, false),
        Field::new("buckets_json", DataType::Utf8,   false),
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

fn buckets_to_json(buckets: &[(u64, u64)]) -> String {
    let mut s = String::from("[");
    for (i, (v, c)) in buckets.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push('[');
        s.push_str(&v.to_string());
        s.push(',');
        s.push_str(&c.to_string());
        s.push(']');
    }
    s.push(']');
    s
}

pub struct RowAccumulator {
    ts_unix_ms:   Vec<i64>,
    name:         Vec<String>,
    labels_json:  Vec<String>,
    kind:         Vec<String>,
    unit:         Vec<String>,
    raw:          Vec<u64>,
    buckets_json: Vec<String>,
}

impl RowAccumulator {
    pub fn new() -> Self {
        RowAccumulator {
            ts_unix_ms:   Vec::new(),
            name:         Vec::new(),
            labels_json:  Vec::new(),
            kind:         Vec::new(),
            unit:         Vec::new(),
            raw:          Vec::new(),
            buckets_json: Vec::new(),
        }
    }

    pub fn push(&mut self, ts: i64, m: &MetricSnapshot) {
        self.ts_unix_ms.push(ts);
        self.name.push(m.name.clone());
        self.labels_json.push(labels_json(&m.labels));
        self.kind.push(kind_str(m.kind).to_string());
        self.unit.push(unit_str(m.unit).to_string());
        self.raw.push(m.raw);
        self.buckets_json.push(buckets_to_json(&m.buckets));
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
            Arc::new(StringArray::from(std::mem::take(&mut self.buckets_json))),
        ]).expect("schema matches column types")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::MetricSnapshot;

    fn make_metric(name: &str, raw: u64, buckets: Vec<(u64, u64)>) -> MetricSnapshot {
        MetricSnapshot {
            name: name.to_string(),
            kind: Kind::Counter,
            unit: Unit::Bytes,
            labels: Labels::new(),
            raw,
            buckets,
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
    fn buckets_to_json_roundtrip() {
        assert_eq!(buckets_to_json(&[(10, 5), (30, 2)]), "[[10,5],[30,2]]");
        assert_eq!(buckets_to_json(&[]), "[]");
    }

    #[test]
    fn accumulator_push_and_drain() {
        let mut acc = RowAccumulator::new();
        assert!(acc.is_empty());
        let m = make_metric("cpu/usage/user", 42, vec![(10, 5), (30, 5)]);
        acc.push(1_000_000, &m);
        assert_eq!(acc.len(), 1);
        let batch = acc.drain();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 7);
        assert!(acc.is_empty());
        let ts_col = batch.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(ts_col.value(0), 1_000_000);
        // buckets_json is column 6
        let bj_col = batch.column(6).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(bj_col.value(0), "[[10,5],[30,5]]");
    }

    #[test]
    fn accumulator_drain_multiple_rows() {
        let mut acc = RowAccumulator::new();
        acc.push(1000, &make_metric("m1", 1, vec![(1, 1)]));
        acc.push(2000, &make_metric("m2", 2, vec![(2, 1)]));
        let batch = acc.drain();
        assert_eq!(batch.num_rows(), 2);
    }

    #[test]
    fn empty_buckets_stored_as_empty_array() {
        let m = MetricSnapshot {
            name: "g".to_string(),
            kind: Kind::Gauge,
            unit: Unit::None,
            labels: Labels::new(),
            raw: 7,
            buckets: vec![],
        };
        let mut acc = RowAccumulator::new();
        acc.push(0, &m);
        let batch = acc.drain();
        let bj_col = batch.column(6).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(bj_col.value(0), "[]");
    }
}
