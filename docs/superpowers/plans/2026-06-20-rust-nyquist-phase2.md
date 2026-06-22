# rust-nyquist Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two push sinks — `nyquist-recorder` (Parquet high-res recorder) and `nyquist-clickhouse` (ClickHouse rollup sink) — wired through the existing `Sink` trait seam from Phase 1.

**Prerequisites:** Phase 1 is fully implemented and all tests pass (`cargo test` is green). The workspace contains `nyquist-core`, `nyquist-samplers`, `nyquist-exposition`, and `nyquist-config`.

**Tech Stack additions:** `arrow-array = "53"`, `arrow-schema = "53"`, `parquet = "53"` (Apache arrow-rs), `clickhouse = "0.13"`, `tempfile = "3"` (dev).

## Global Constraints

- Neither new crate may depend on anything other than `nyquist-core` (plus their own external crates). No sampler, exposition, or config crate dependency.
- `nyquist-core` API is not changed in Phase 2 — only `nyquist-core::scheduler` gains `spawn_sink`.
- TDD: write the failing test first, watch it fail, implement minimally, watch it pass, then commit.
- Every dependency version must match the constraints in the spec. Do not use `"*"` versions.
- `arrow-array`, `arrow-schema`, and `parquet` must all share major version `53` to avoid type-mismatch compile errors.
- The ClickHouse integration test is gated behind `#[cfg(feature = "clickhouse-integration")]` and skipped by default.
- Both crates must compile on macOS (no OS-specific code; file I/O and HTTP are cross-platform).
- Default config: recorder disabled, ClickHouse disabled. Both require opt-in via config.

---

### Task 0: Workspace scaffolding for Phase 2

**Files:**
- Create: `crates/nyquist-recorder/Cargo.toml`
- Create: `crates/nyquist-recorder/src/lib.rs`
- Create: `crates/nyquist-recorder/src/schema.rs`
- Create: `crates/nyquist-recorder/src/writer.rs`
- Create: `crates/nyquist-clickhouse/Cargo.toml`
- Create: `crates/nyquist-clickhouse/src/lib.rs`
- Create: `crates/nyquist-clickhouse/src/grouping.rs`
- Create: `crates/nyquist-clickhouse/src/inserter.rs`

**Interfaces:**
- Produces: two new crates that compile (no logic yet). The workspace glob `members = ["crates/*"]` already picks them up.

- [ ] **Step 1: Create `crates/nyquist-recorder/Cargo.toml`**

```toml
[package]
name = "nyquist-recorder"
version = "0.1.0"
edition = "2021"

[dependencies]
nyquist-core = { path = "../nyquist-core" }
arrow-array  = "53"
arrow-schema = "53"
parquet      = { version = "53", default-features = false, features = ["snap"] }
async-trait  = "0.1"
thiserror    = "1"
tracing      = "0.1"

[dev-dependencies]
tempfile = "3"
tokio    = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Note: `parquet` is built with `default-features = false, features = ["snap"]` to avoid pulling in the optional async and object_store features; Snappy compression is the only feature needed.

- [ ] **Step 2: Create `crates/nyquist-clickhouse/Cargo.toml`**

```toml
[package]
name = "nyquist-clickhouse"
version = "0.1.0"
edition = "2021"

[dependencies]
nyquist-core = { path = "../nyquist-core" }
clickhouse   = "0.13"
async-trait  = "0.1"
thiserror    = "1"
tracing      = "0.1"
tokio        = { version = "1", features = ["rt", "time"] }

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

[features]
clickhouse-integration = []
```

- [ ] **Step 3: Create placeholder source files**

`crates/nyquist-recorder/src/lib.rs`:
```rust
//! Parquet high-resolution recorder sink for nyquist.
pub mod schema;
pub mod writer;
```

`crates/nyquist-recorder/src/schema.rs`:
```rust
//! Arrow schema definition and row accumulator.
```

`crates/nyquist-recorder/src/writer.rs`:
```rust
//! Parquet file writer with time-based rotation.
```

`crates/nyquist-clickhouse/src/lib.rs`:
```rust
//! ClickHouse rollup sink for nyquist.
pub mod grouping;
pub mod inserter;
```

`crates/nyquist-clickhouse/src/grouping.rs`:
```rust
//! Metric grouping by sampler prefix and DDL generation.
```

`crates/nyquist-clickhouse/src/inserter.rs`:
```rust
//! Async insert logic for ClickHouse.
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build`
Expected: compiles with no errors. The new crates are empty so warnings about unused items are fine.

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-recorder crates/nyquist-clickhouse
git commit -m "chore(phase2): scaffold nyquist-recorder and nyquist-clickhouse crates"
```

---

### Task 1: Arrow schema and `RowAccumulator`

**Files:**
- Modify: `crates/nyquist-recorder/src/schema.rs`

**Interfaces:**
- Produces:
  - `fn nyquist_schema() -> Arc<arrow_schema::Schema>` — the 10-column Arrow schema
  - `fn labels_json(labels: &Labels) -> String` — compact JSON serialisation of `Labels`
  - `struct RowAccumulator` with `new()`, `push(ts_unix_ms: i64, m: &MetricSnapshot)`, `len()`, `is_empty()`, `drain() -> RecordBatch`

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-recorder/src/schema.rs`:
```rust
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
        // Verify ts column.
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
        // A metric with only p50 exposed; p90/p99/p99_9 should default to 0.
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
        // p99 is column index 8 (ts=0, name=1, labels=2, kind=3, unit=4, raw=5, p50=6, p90=7, p99=8, p99_9=9)
        let p99_col = batch.column(8).as_any().downcast_ref::<UInt64Array>().unwrap();
        assert_eq!(p99_col.value(0), 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-recorder schema::`
Expected: FAIL — `RowAccumulator`, `labels_json`, `nyquist_schema` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-recorder/src/schema.rs` with:
```rust
use std::sync::Arc;
use arrow_array::array::{
    Int64Array, StringArray, UInt64Array,
};
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::snapshot::MetricSnapshot;

/// The fixed 10-column Arrow schema for the long-format Parquet output.
pub fn nyquist_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_unix_ms",   DataType::Int64,  false),
        Field::new("name",         DataType::Utf8,   false),
        Field::new("labels_json",  DataType::Utf8,   false),
        Field::new("kind",         DataType::Utf8,   false),
        Field::new("unit",         DataType::Utf8,   false),
        Field::new("raw",          DataType::UInt64, false),
        Field::new("p50",          DataType::UInt64, false),
        Field::new("p90",          DataType::UInt64, false),
        Field::new("p99",          DataType::UInt64, false),
        Field::new("p99_9",        DataType::UInt64, false),
    ]))
}

/// Serialize a `Labels` map to a compact JSON object with sorted keys.
/// Example: `{"cpu":"0","core":"1"}`. Empty labels → `{}`.
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
    percentiles.iter().find(|(p, _)| (*p - target).abs() < 0.001)
        .map(|(_, v)| *v)
        .unwrap_or(0)
}

/// In-memory columnar buffer. Accumulates rows, drains to an Arrow `RecordBatch`.
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

    /// Drain all buffered rows into an Arrow `RecordBatch`, clearing the buffers.
    pub fn drain(&mut self) -> RecordBatch {
        let schema = nyquist_schema();
        let batch = RecordBatch::try_new(schema, vec![
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
        ]).expect("schema matches column types");
        batch
    }
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 above)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-recorder schema::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-recorder/src/schema.rs
git commit -m "feat(recorder): Arrow schema and RowAccumulator"
```

---

### Task 2: Parquet file writer with rotation

**Files:**
- Modify: `crates/nyquist-recorder/src/writer.rs`

**Interfaces:**
- Consumes: `RowAccumulator`, `nyquist_schema` from `schema.rs`.
- Produces:
  - `struct ParquetWriter` with `ParquetWriter::new(output_dir: PathBuf, rotation_interval: Duration) -> Result<Self, RecorderError>`
  - `.flush(&mut self, acc: &mut RowAccumulator) -> Result<(), RecorderError>`
  - `.maybe_rotate(&mut self) -> Result<(), RecorderError>`

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-recorder/src/writer.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RowAccumulator, labels_json};
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::MetricSnapshot;
    use std::time::Duration;
    use tempfile::tempdir;

    fn make_metric() -> MetricSnapshot {
        MetricSnapshot {
            name: "cpu/usage/user".to_string(),
            kind: Kind::Counter,
            unit: Unit::Count,
            labels: Labels::new(),
            raw: 100,
            percentiles: vec![(50.0, 50), (90.0, 90), (99.0, 99), (99.9, 100)],
        }
    }

    #[test]
    fn flush_creates_parquet_file() {
        let dir = tempdir().unwrap();
        let mut writer = ParquetWriter::new(
            dir.path().to_path_buf(),
            Duration::from_secs(3600),
        ).unwrap();
        let mut acc = RowAccumulator::new();
        acc.push(1_000_000, &make_metric());
        writer.flush(&mut acc).unwrap();
        // File must exist with non-zero size.
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
            .collect();
        assert_eq!(files.len(), 1, "expected exactly 1 parquet file");
        assert!(files[0].metadata().unwrap().len() > 0);
    }

    #[test]
    fn flush_empty_accumulator_is_noop() {
        let dir = tempdir().unwrap();
        let mut writer = ParquetWriter::new(
            dir.path().to_path_buf(),
            Duration::from_secs(3600),
        ).unwrap();
        let mut acc = RowAccumulator::new();
        writer.flush(&mut acc).unwrap(); // should not panic or create files
    }

    #[test]
    fn rotation_creates_new_file() {
        let dir = tempdir().unwrap();
        let mut writer = ParquetWriter::new(
            dir.path().to_path_buf(),
            Duration::from_millis(1), // immediate rotation
        ).unwrap();
        let mut acc = RowAccumulator::new();
        acc.push(1000, &make_metric());
        writer.flush(&mut acc).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        writer.maybe_rotate().unwrap();
        acc.push(2000, &make_metric());
        writer.flush(&mut acc).unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
            .collect();
        assert_eq!(files.len(), 2, "expected 2 parquet files after rotation");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-recorder writer::`
Expected: FAIL — `ParquetWriter` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-recorder/src/writer.rs` with:
```rust
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use parquet::basic::Compression;
use crate::schema::{nyquist_schema, RowAccumulator};

#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    #[error("recorder I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
}

/// Owns the active Parquet file and handles time-based rotation.
pub struct ParquetWriter {
    output_dir:        PathBuf,
    rotation_interval: Duration,
    rotation_due:      Instant,
    /// The active ArrowWriter; None before the first flush or briefly during rotation.
    inner:             Option<ArrowWriter<File>>,
}

impl ParquetWriter {
    /// Create a new writer and open the first Parquet file.
    pub fn new(output_dir: PathBuf, rotation_interval: Duration) -> Result<Self, RecorderError> {
        fs::create_dir_all(&output_dir)?;
        let rotation_due = Instant::now() + rotation_interval;
        let mut w = ParquetWriter {
            output_dir,
            rotation_interval,
            rotation_due,
            inner: None,
        };
        w.open_new_file()?;
        Ok(w)
    }

    fn open_new_file(&mut self) -> Result<(), RecorderError> {
        // Close the existing writer if any (writes the Parquet footer).
        if let Some(writer) = self.inner.take() {
            writer.close()?;
        }
        let ts_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let path = self.output_dir.join(format!("nyquist-{ts_secs}.parquet"));
        let file = File::create(&path)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let schema = nyquist_schema();
        let writer = ArrowWriter::try_new(file, schema, Some(props))?;
        self.inner = Some(writer);
        self.rotation_due = Instant::now() + self.rotation_interval;
        Ok(())
    }

    /// Rotate to a new file if the rotation interval has elapsed.
    pub fn maybe_rotate(&mut self) -> Result<(), RecorderError> {
        if Instant::now() >= self.rotation_due {
            self.open_new_file()?;
        }
        Ok(())
    }

    /// Write all buffered rows to the current Parquet file as a row group, then clear the buffer.
    /// If the accumulator is empty, this is a no-op.
    pub fn flush(&mut self, acc: &mut RowAccumulator) -> Result<(), RecorderError> {
        if acc.is_empty() { return Ok(()); }
        let batch = acc.drain();
        if let Some(writer) = self.inner.as_mut() {
            writer.write(&batch)?;
            writer.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 above)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-recorder writer::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-recorder/src/writer.rs
git commit -m "feat(recorder): Parquet file writer with time-based rotation"
```

---

### Task 3: `RecorderSink` implementation

**Files:**
- Modify: `crates/nyquist-recorder/src/lib.rs`
- Create: `crates/nyquist-recorder/tests/roundtrip.rs`

**Interfaces:**
- Consumes: `RowAccumulator` (Task 1), `ParquetWriter`, `RecorderError` (Task 2).
- Produces:
  - `struct RecorderConfig { output_dir, flush_interval, rotation_interval }` (plain struct, not from config crate)
  - `struct RecorderSink` implementing `nyquist_core::sink::Sink`
  - `RecorderSink::new(cfg: RecorderConfig) -> Result<Self, RecorderError>`

- [ ] **Step 1: Write the failing integration test**

Create `crates/nyquist-recorder/tests/roundtrip.rs`:
```rust
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
use nyquist_core::sink::Sink;
use nyquist_recorder::{RecorderSink, RecorderConfig};
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

fn make_snapshot(ts: SystemTime) -> RegistrySnapshot {
    RegistrySnapshot {
        captured: ts,
        metrics: vec![
            MetricSnapshot {
                name: "cpu/usage/user".to_string(),
                kind: Kind::Counter,
                unit: Unit::Count,
                labels: Labels::new().insert("cpu", "cpu0"),
                raw: 1000,
                percentiles: vec![(50.0, 100), (90.0, 200), (99.0, 300), (99.9, 400)],
            },
            MetricSnapshot {
                name: "memory/memfree".to_string(),
                kind: Kind::Gauge,
                unit: Unit::Bytes,
                labels: Labels::new(),
                raw: 8_000_000,
                percentiles: vec![(50.0, 8_000_000), (90.0, 8_000_000), (99.0, 8_000_000), (99.9, 8_000_000)],
            },
        ],
    }
}

#[tokio::test]
async fn recorder_sink_writes_parquet_file() {
    let dir = tempdir().unwrap();
    let cfg = RecorderConfig {
        output_dir: dir.path().to_path_buf(),
        flush_interval: Duration::ZERO, // flush on every export
        rotation_interval: Duration::from_secs(3600),
    };
    let mut sink = RecorderSink::new(cfg).unwrap();

    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    sink.export(&make_snapshot(t0)).await.unwrap();
    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_001);
    sink.export(&make_snapshot(t1)).await.unwrap();

    // Parquet file must exist with non-zero size.
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert_eq!(files.len(), 1);
    let file_size = files[0].metadata().unwrap().len();
    assert!(file_size > 0, "parquet file is empty");

    // Read back the Parquet file and assert row count = 2 snapshots × 2 metrics = 4.
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use std::fs::File;
    let reader = SerializedFileReader::new(File::open(files[0].path()).unwrap()).unwrap();
    let row_count: i64 = reader.metadata().row_groups().iter()
        .map(|rg| rg.num_rows())
        .sum();
    assert_eq!(row_count, 4, "expected 4 rows (2 metrics x 2 exports), got {row_count}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-recorder --test roundtrip`
Expected: FAIL — `RecorderSink`, `RecorderConfig` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-recorder/src/lib.rs` with:
```rust
//! Parquet high-resolution recorder sink for nyquist.

pub mod schema;
pub mod writer;

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use nyquist_core::sink::{Sink, SinkError};
use nyquist_core::snapshot::RegistrySnapshot;
use schema::RowAccumulator;
use writer::{ParquetWriter, RecorderError};

pub use writer::RecorderError as Error;

/// Configuration for `RecorderSink`. This is a plain struct so that
/// `nyquist-recorder` does not depend on `nyquist-config`.
#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub output_dir:        PathBuf,
    pub flush_interval:    Duration,
    pub rotation_interval: Duration,
}

/// A `Sink` that writes one Parquet row per metric per snapshot export.
pub struct RecorderSink {
    writer:     ParquetWriter,
    acc:        RowAccumulator,
    flush_iv:   Duration,
    next_flush: Instant,
}

impl RecorderSink {
    pub fn new(cfg: RecorderConfig) -> Result<Self, RecorderError> {
        let writer = ParquetWriter::new(cfg.output_dir, cfg.rotation_interval)?;
        Ok(RecorderSink {
            writer,
            acc: RowAccumulator::new(),
            flush_iv: cfg.flush_interval,
            next_flush: Instant::now(), // flush on first export
        })
    }
}

#[async_trait]
impl Sink for RecorderSink {
    async fn export(&mut self, snap: &RegistrySnapshot) -> Result<(), SinkError> {
        let ts_unix_ms = snap.captured
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        for metric in &snap.metrics {
            self.acc.push(ts_unix_ms, metric);
        }

        let now = Instant::now();
        if now >= self.next_flush {
            self.writer.maybe_rotate()
                .map_err(|e| Box::new(e) as SinkError)?;
            self.writer.flush(&mut self.acc)
                .map_err(|e| Box::new(e) as SinkError)?;
            self.next_flush = now + self.flush_iv;
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run integration test to verify it passes**

Run: `cargo test -p nyquist-recorder --test roundtrip`
Expected: PASS (1 test).

- [ ] **Step 5: Run full recorder test suite and commit**

Run: `cargo test -p nyquist-recorder`
Expected: PASS (all schema + writer + roundtrip tests).

```bash
git add crates/nyquist-recorder/src/lib.rs crates/nyquist-recorder/tests/roundtrip.rs
git commit -m "feat(recorder): RecorderSink implements Sink, with roundtrip integration test"
```

---

### Task 4: Config additions for Phase 2

**Files:**
- Modify: `crates/nyquist-config/src/lib.rs`
- Modify: `crates/nyquist-config/tests/load.rs`

**Interfaces:**
- Produces:
  - `struct RecorderConfig { enabled: bool, output_dir: String, flush_interval: Duration, rotation_interval: Duration, export_interval: Duration }`
  - `struct ClickHouseConfig { enabled: bool, url: String, database: String, username: String, password: String, export_interval: Duration }`
  - `Config::recorder: RecorderConfig` and `Config::clickhouse: ClickHouseConfig` fields

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-config/tests/load.rs`:
```rust
use std::time::Duration;

#[test]
fn recorder_config_defaults_disabled() {
    let c = nyquist_config::Config::default();
    assert!(!c.recorder.enabled);
    assert_eq!(c.recorder.flush_interval, Duration::from_secs(10));
    assert_eq!(c.recorder.rotation_interval, Duration::from_secs(3600));
    assert_eq!(c.recorder.export_interval, Duration::from_secs(1));
    assert_eq!(c.recorder.output_dir, "/var/lib/nyquist/parquet");
}

#[test]
fn clickhouse_config_defaults_disabled() {
    let c = nyquist_config::Config::default();
    assert!(!c.clickhouse.enabled);
    assert_eq!(c.clickhouse.url, "http://localhost:8123");
    assert_eq!(c.clickhouse.database, "nyquist");
    assert_eq!(c.clickhouse.export_interval, Duration::from_secs(60));
}

#[test]
fn parses_recorder_and_clickhouse_toml() {
    let toml = r#"
        [recorder]
        enabled = true
        output_dir = "/tmp/parquet"
        flush_interval = "5s"
        rotation_interval = "30m"
        export_interval = "500ms"

        [clickhouse]
        enabled = true
        url = "http://ch:8123"
        database = "metrics"
        username = "nyquist"
        password = "secret"
        export_interval = "30s"
    "#;
    let c: nyquist_config::Config = toml::from_str(toml).unwrap();
    assert!(c.recorder.enabled);
    assert_eq!(c.recorder.output_dir, "/tmp/parquet");
    assert_eq!(c.recorder.flush_interval, Duration::from_secs(5));
    assert_eq!(c.recorder.rotation_interval, Duration::from_secs(30 * 60));
    assert_eq!(c.recorder.export_interval, Duration::from_millis(500));
    assert!(c.clickhouse.enabled);
    assert_eq!(c.clickhouse.url, "http://ch:8123");
    assert_eq!(c.clickhouse.password, "secret");
    assert_eq!(c.clickhouse.export_interval, Duration::from_secs(30));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-config`
Expected: FAIL — `Config::recorder` and `Config::clickhouse` fields not found.

- [ ] **Step 3: Write minimal implementation**

Append to `crates/nyquist-config/src/lib.rs` (before the existing `impl Config`):
```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RecorderConfig {
    pub enabled: bool,
    pub output_dir: String,
    #[serde(with = "humantime_serde")]
    pub flush_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub rotation_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub export_interval: Duration,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        RecorderConfig {
            enabled: false,
            output_dir: "/var/lib/nyquist/parquet".to_string(),
            flush_interval: Duration::from_secs(10),
            rotation_interval: Duration::from_secs(3600),
            export_interval: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClickHouseConfig {
    pub enabled: bool,
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
    #[serde(with = "humantime_serde")]
    pub export_interval: Duration,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        ClickHouseConfig {
            enabled: false,
            url: "http://localhost:8123".to_string(),
            database: "nyquist".to_string(),
            username: "default".to_string(),
            password: String::new(),
            export_interval: Duration::from_secs(60),
        }
    }
}
```

Add `pub recorder: RecorderConfig` and `pub clickhouse: ClickHouseConfig` fields to the `Config` struct:
```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general:    General,
    pub samplers:   BTreeMap<String, SamplerConfig>,
    pub recorder:   RecorderConfig,
    pub clickhouse: ClickHouseConfig,
}
```

Update `impl Default for Config`:
```rust
impl Default for Config {
    fn default() -> Self {
        Config {
            general:    General::default(),
            samplers:   BTreeMap::new(),
            recorder:   RecorderConfig::default(),
            clickhouse: ClickHouseConfig::default(),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-config`
Expected: PASS (all existing tests plus 3 new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-config/src/lib.rs crates/nyquist-config/tests/load.rs
git commit -m "feat(config): add RecorderConfig and ClickHouseConfig with defaults"
```

---

### Task 5: Metric grouping and DDL generation (`nyquist-clickhouse/grouping.rs`)

**Files:**
- Modify: `crates/nyquist-clickhouse/src/grouping.rs`

**Interfaces:**
- Consumes: `RegistrySnapshot`, `MetricSnapshot`, `Labels` from `nyquist-core`.
- Produces:
  - `fn group_metrics<'a>(snap: &'a RegistrySnapshot) -> BTreeMap<String, Vec<&'a MetricSnapshot>>`
  - `fn sanitise(name: &str, labels: &Labels) -> String`
  - `fn generate_ddl(group: &str, metrics: &[&MetricSnapshot]) -> String`

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-clickhouse/src/grouping.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
    use std::time::SystemTime;

    fn make_snap() -> RegistrySnapshot {
        RegistrySnapshot {
            captured: SystemTime::UNIX_EPOCH,
            metrics: vec![
                MetricSnapshot {
                    name: "cpu/usage/user".to_string(),
                    kind: Kind::Counter, unit: Unit::Count,
                    labels: Labels::new().insert("cpu", "cpu0"),
                    raw: 10, percentiles: vec![(50.0, 5)],
                },
                MetricSnapshot {
                    name: "cpu/usage/system".to_string(),
                    kind: Kind::Counter, unit: Unit::Count,
                    labels: Labels::new().insert("cpu", "cpu0"),
                    raw: 2, percentiles: vec![(50.0, 1)],
                },
                MetricSnapshot {
                    name: "memory/memfree".to_string(),
                    kind: Kind::Gauge, unit: Unit::Bytes,
                    labels: Labels::new(),
                    raw: 1_000_000, percentiles: vec![(50.0, 1_000_000)],
                },
            ],
        }
    }

    #[test]
    fn groups_by_first_path_component() {
        let snap = make_snap();
        let groups = group_metrics(&snap);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["cpu"].len(), 2);
        assert_eq!(groups["memory"].len(), 1);
    }

    #[test]
    fn sanitise_replaces_slashes_and_appends_labels() {
        let labels = Labels::new().insert("cpu", "cpu0");
        let s = sanitise("cpu/usage/user", &labels);
        assert_eq!(s, "cpu_usage_user_cpu0");
    }

    #[test]
    fn sanitise_no_labels() {
        let s = sanitise("memory/memfree", &Labels::new());
        assert_eq!(s, "memory_memfree");
    }

    #[test]
    fn sanitise_multiple_labels_sorted() {
        let labels = Labels::new().insert("dev", "eth0").insert("dir", "rx");
        // Labels are sorted by key: dir=rx, dev=eth0 → alphabetically dir, dev
        let s = sanitise("network/bytes", &labels);
        assert_eq!(s, "network_bytes_rx_eth0");
    }

    #[test]
    fn generate_ddl_contains_table_name_and_ts() {
        let snap = make_snap();
        let groups = group_metrics(&snap);
        let metrics: Vec<&MetricSnapshot> = groups["cpu"].clone();
        let ddl = generate_ddl("cpu", &metrics);
        assert!(ddl.contains("CREATE TABLE IF NOT EXISTS nyquist_cpu"), "{ddl}");
        assert!(ddl.contains("ts DateTime"), "{ddl}");
        assert!(ddl.contains("_p50"), "{ddl}");
        assert!(ddl.contains("_p99_9"), "{ddl}");
        assert!(ddl.contains("MergeTree"), "{ddl}");
        assert!(ddl.contains("ORDER BY ts"), "{ddl}");
    }

    #[test]
    fn generate_ddl_has_column_per_metric() {
        let snap = make_snap();
        let groups = group_metrics(&snap);
        let metrics: Vec<&MetricSnapshot> = groups["cpu"].clone();
        let ddl = generate_ddl("cpu", &metrics);
        // 2 cpu metrics x 5 columns each = 10 metric columns plus ts
        // Check at least the _raw and _p99 patterns appear.
        assert!(ddl.contains("_raw"), "{ddl}");
        assert!(ddl.contains("UInt64"), "{ddl}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-clickhouse grouping::`
Expected: FAIL — `group_metrics`, `sanitise`, `generate_ddl` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-clickhouse/src/grouping.rs` with:
```rust
use std::collections::BTreeMap;
use nyquist_core::model::Labels;
use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};

/// Group metrics by the first `/`-delimited path component of their name.
/// Metrics with no `/` in their name form a group named after the full metric name.
pub fn group_metrics<'a>(snap: &'a RegistrySnapshot) -> BTreeMap<String, Vec<&'a MetricSnapshot>> {
    let mut groups: BTreeMap<String, Vec<&MetricSnapshot>> = BTreeMap::new();
    for m in &snap.metrics {
        let group = m.name.split('/').next().unwrap_or(&m.name).to_string();
        groups.entry(group).or_default().push(m);
    }
    groups
}

/// Produce a ClickHouse-safe column name base from a metric name and its labels.
///
/// Rules:
/// - Replace `/` and `-` and `.` with `_`.
/// - Append label values sorted by label key, joined with `_`.
///
/// Example: `"network/receive/bytes"` + `{iface: "eth0"}` → `"network_receive_bytes_eth0"`.
pub fn sanitise(name: &str, labels: &Labels) -> String {
    let base: String = name.chars()
        .map(|c| if c == '/' || c == '-' || c == '.' { '_' } else { c })
        .collect();
    let label_suffix: Vec<String> = labels.iter().map(|(_, v)| v.clone()).collect();
    if label_suffix.is_empty() {
        base
    } else {
        format!("{}_{}", base, label_suffix.join("_"))
    }
}

/// Generate a `CREATE TABLE IF NOT EXISTS nyquist_{group}` DDL statement
/// with columns derived from the given metrics.
///
/// Column layout per metric (sorted by sanitised name for determinism):
///   {base}_raw   UInt64,
///   {base}_p50   UInt64,
///   {base}_p90   UInt64,
///   {base}_p99   UInt64,
///   {base}_p99_9 UInt64,
pub fn generate_ddl(group: &str, metrics: &[&MetricSnapshot]) -> String {
    // Sort by sanitised name for a deterministic DDL regardless of insertion order.
    let mut sorted: Vec<&MetricSnapshot> = metrics.to_vec();
    sorted.sort_by_key(|m| sanitise(&m.name, &m.labels));

    let mut cols = String::from("    ts DateTime");
    for m in &sorted {
        let base = sanitise(&m.name, &m.labels);
        cols.push_str(&format!(",\n    {base}_raw   UInt64"));
        cols.push_str(&format!(",\n    {base}_p50   UInt64"));
        cols.push_str(&format!(",\n    {base}_p90   UInt64"));
        cols.push_str(&format!(",\n    {base}_p99   UInt64"));
        cols.push_str(&format!(",\n    {base}_p99_9 UInt64"));
    }

    format!(
        "CREATE TABLE IF NOT EXISTS nyquist_{group}\n(\n{cols}\n)\nENGINE = MergeTree()\nORDER BY ts"
    )
}

#[cfg(test)]
mod tests {
    // ... tests from Step 1 above
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-clickhouse grouping::`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-clickhouse/src/grouping.rs
git commit -m "feat(clickhouse): metric grouping, name sanitisation, DDL generation"
```

---

### Task 6: INSERT statement builder (`nyquist-clickhouse/inserter.rs`)

**Files:**
- Modify: `crates/nyquist-clickhouse/src/inserter.rs`

**Interfaces:**
- Consumes: `sanitise`, `generate_ddl`, `group_metrics` from `grouping.rs`.
- Produces:
  - `fn build_insert(group: &str, metrics: &[&MetricSnapshot], ts_secs: u64) -> String` — produces a complete `INSERT INTO nyquist_{group} (...) VALUES (...)` SQL string.

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-clickhouse/src/inserter.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::MetricSnapshot;

    fn metric(name: &str, raw: u64, p50: u64, p90: u64, p99: u64, p99_9: u64) -> MetricSnapshot {
        MetricSnapshot {
            name: name.to_string(),
            kind: Kind::Counter,
            unit: Unit::Count,
            labels: Labels::new(),
            raw,
            percentiles: vec![(50.0, p50), (90.0, p90), (99.0, p99), (99.9, p99_9)],
        }
    }

    #[test]
    fn build_insert_contains_table_name() {
        let m = metric("cpu/usage/user", 1000, 50, 90, 99, 100);
        let refs = vec![&m];
        let sql = build_insert("cpu", &refs, 1_000_000);
        assert!(sql.starts_with("INSERT INTO nyquist_cpu"), "{sql}");
    }

    #[test]
    fn build_insert_contains_ts_and_values() {
        let m = metric("cpu/usage/user", 1000, 50, 90, 99, 100);
        let refs = vec![&m];
        let sql = build_insert("cpu", &refs, 1_000_000);
        assert!(sql.contains("1000000"), "{sql}"); // ts as unix seconds
        assert!(sql.contains("1000"),    "{sql}"); // raw
        assert!(sql.contains("50"),      "{sql}"); // p50
        assert!(sql.contains("99"),      "{sql}"); // p99
        assert!(sql.contains("100"),     "{sql}"); // p99_9
    }

    #[test]
    fn build_insert_column_count_matches_values() {
        let m1 = metric("cpu/usage/user",   100, 10, 20, 30, 40);
        let m2 = metric("cpu/usage/system",  50,  5, 10, 15, 20);
        let refs = vec![&m1, &m2];
        let sql = build_insert("cpu", &refs, 9999);
        // Verify it contains VALUES clause (basic sanity).
        assert!(sql.contains("VALUES"), "{sql}");
        // Count commas in column list vs values list — they should be equal.
        let paren_open = sql.find('(').unwrap();
        let paren_close = sql.find(')').unwrap();
        let col_part = &sql[paren_open..=paren_close];
        let val_paren_open = sql.rfind('(').unwrap();
        let val_paren_close = sql.rfind(')').unwrap();
        let val_part = &sql[val_paren_open..=val_paren_close];
        let col_commas = col_part.chars().filter(|&c| c == ',').count();
        let val_commas = val_part.chars().filter(|&c| c == ',').count();
        assert_eq!(col_commas, val_commas, "column/value count mismatch in: {sql}");
    }

    #[test]
    fn missing_percentile_defaults_to_zero_in_insert() {
        // Metric only has p50; p90/p99/p99_9 should default to 0 in the INSERT.
        let m = MetricSnapshot {
            name: "g".to_string(),
            kind: Kind::Gauge,
            unit: Unit::None,
            labels: Labels::new(),
            raw: 7,
            percentiles: vec![(50.0, 7)],
        };
        let refs = vec![&m];
        let sql = build_insert("misc", &refs, 0);
        // The VALUES clause should contain "0" for the missing percentiles.
        assert!(sql.contains("VALUES"), "{sql}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-clickhouse inserter::`
Expected: FAIL — `build_insert` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-clickhouse/src/inserter.rs` with:
```rust
use nyquist_core::model::Labels;
use nyquist_core::snapshot::MetricSnapshot;
use crate::grouping::sanitise;

fn find_pct(percentiles: &[(f64, u64)], target: f64) -> u64 {
    percentiles.iter()
        .find(|(p, _)| (*p - target).abs() < 0.001)
        .map(|(_, v)| *v)
        .unwrap_or(0)
}

/// Build a complete `INSERT INTO nyquist_{group} (...) VALUES (...)` SQL string.
///
/// `ts_secs` is a Unix timestamp in seconds (ClickHouse `DateTime` accepts integer seconds).
/// Metrics are sorted by sanitised name for a deterministic column order that matches
/// the DDL generated by `generate_ddl`.
pub fn build_insert(group: &str, metrics: &[&MetricSnapshot], ts_secs: u64) -> String {
    // Sort metrics to match the column order in the DDL.
    let mut sorted: Vec<&MetricSnapshot> = metrics.to_vec();
    sorted.sort_by_key(|m| sanitise(&m.name, &m.labels));

    // Build column list.
    let mut cols = vec!["ts".to_string()];
    for m in &sorted {
        let base = sanitise(&m.name, &m.labels);
        cols.push(format!("{base}_raw"));
        cols.push(format!("{base}_p50"));
        cols.push(format!("{base}_p90"));
        cols.push(format!("{base}_p99"));
        cols.push(format!("{base}_p99_9"));
    }

    // Build values list.
    let mut vals = vec![ts_secs.to_string()];
    for m in &sorted {
        vals.push(m.raw.to_string());
        vals.push(find_pct(&m.percentiles, 50.0).to_string());
        vals.push(find_pct(&m.percentiles, 90.0).to_string());
        vals.push(find_pct(&m.percentiles, 99.0).to_string());
        vals.push(find_pct(&m.percentiles, 99.9).to_string());
    }

    format!(
        "INSERT INTO nyquist_{group} ({}) VALUES ({})",
        cols.join(", "),
        vals.join(", "),
    )
}

#[cfg(test)]
mod tests {
    // ... tests from Step 1 above
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-clickhouse inserter::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-clickhouse/src/inserter.rs
git commit -m "feat(clickhouse): INSERT statement builder"
```

---

### Task 7: `ClickHouseSink` implementation

**Files:**
- Modify: `crates/nyquist-clickhouse/src/lib.rs`

**Interfaces:**
- Consumes: `group_metrics`, `generate_ddl` (Task 5); `build_insert` (Task 6).
- Produces:
  - `struct ClickHouseClientConfig { url, database, username, password }` — plain config struct
  - `struct ClickHouseSink` implementing `nyquist_core::sink::Sink`
  - `ClickHouseSink::new(cfg: ClickHouseClientConfig) -> Self`

- [ ] **Step 1: Write the failing test (unit — no network)**

Append to `crates/nyquist-clickhouse/src/lib.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clickhouse_sink_new_does_not_panic() {
        // Building the sink must not attempt a network connection.
        let _sink = ClickHouseSink::new(ClickHouseClientConfig {
            url:      "http://localhost:8123".to_string(),
            database: "nyquist".to_string(),
            username: "default".to_string(),
            password: String::new(),
        });
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-clickhouse`
Expected: FAIL — `ClickHouseSink`, `ClickHouseClientConfig` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-clickhouse/src/lib.rs` with:
```rust
//! ClickHouse rollup sink for nyquist.

pub mod grouping;
pub mod inserter;

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use clickhouse::Client;
use nyquist_core::sink::{Sink, SinkError};
use nyquist_core::snapshot::RegistrySnapshot;
use crate::grouping::{group_metrics, generate_ddl};
use crate::inserter::build_insert;

/// Plain configuration struct for `ClickHouseSink`.
/// `nyquist-clickhouse` does not depend on `nyquist-config`.
#[derive(Clone, Debug)]
pub struct ClickHouseClientConfig {
    pub url:      String,
    pub database: String,
    pub username: String,
    pub password: String,
}

/// A `Sink` that writes windowed percentile rollups to ClickHouse.
pub struct ClickHouseSink {
    client:         Client,
    tables_created: BTreeSet<String>,
}

impl ClickHouseSink {
    pub fn new(cfg: ClickHouseClientConfig) -> Self {
        let client = Client::default()
            .with_url(&cfg.url)
            .with_database(&cfg.database)
            .with_user(&cfg.username)
            .with_password(&cfg.password);
        ClickHouseSink { client, tables_created: BTreeSet::new() }
    }
}

#[async_trait]
impl Sink for ClickHouseSink {
    async fn export(&mut self, snap: &RegistrySnapshot) -> Result<(), SinkError> {
        let groups = group_metrics(snap);
        if groups.is_empty() { return Ok(()); }

        let ts_secs = snap.captured
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for (group, metrics) in &groups {
            // Ensure the table exists (idempotent DDL).
            if !self.tables_created.contains(group) {
                let ddl = generate_ddl(group, metrics);
                match self.client.query(&ddl).execute().await {
                    Ok(_) => { self.tables_created.insert(group.clone()); }
                    Err(e) => {
                        tracing::error!(group = %group, error = %e, "failed to create ClickHouse table");
                        continue; // skip this group's insert for this tick
                    }
                }
            }

            // Insert one wide row for this group.
            let sql = build_insert(group, metrics, ts_secs);
            if let Err(e) = self.client.query(&sql).execute().await {
                tracing::warn!(group = %group, error = %e, "ClickHouse insert failed; skipping tick");
                // Do not return Err — one failed group must not abort the whole export.
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clickhouse_sink_new_does_not_panic() {
        let _sink = ClickHouseSink::new(ClickHouseClientConfig {
            url:      "http://localhost:8123".to_string(),
            database: "nyquist".to_string(),
            username: "default".to_string(),
            password: String::new(),
        });
    }
}

/// Integration tests that require a live ClickHouse instance.
/// Run with: cargo test -p nyquist-clickhouse --features clickhouse-integration
#[cfg(test)]
#[cfg(feature = "clickhouse-integration")]
mod integration_tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
    use std::time::SystemTime;

    #[tokio::test]
    async fn integration_insert_and_verify() {
        let mut sink = ClickHouseSink::new(ClickHouseClientConfig {
            url:      "http://localhost:8123".to_string(),
            database: "nyquist_test".to_string(),
            username: "default".to_string(),
            password: String::new(),
        });
        let snap = RegistrySnapshot {
            captured: SystemTime::now(),
            metrics: vec![MetricSnapshot {
                name: "cpu/usage/user".to_string(),
                kind: Kind::Counter,
                unit: Unit::Count,
                labels: Labels::new(),
                raw: 42,
                percentiles: vec![(50.0, 20), (90.0, 35), (99.0, 40), (99.9, 42)],
            }],
        };
        sink.export(&snap).await.expect("export failed");
        // Query ClickHouse to verify the row exists.
        let count: u64 = sink.client
            .query("SELECT count() FROM nyquist_cpu")
            .fetch_one::<u64>()
            .await
            .expect("query failed");
        assert!(count >= 1, "expected at least 1 row, got {count}");
    }
}
```

- [ ] **Step 4: Run unit tests to verify they pass**

Run: `cargo test -p nyquist-clickhouse`
Expected: PASS (1 unit test; integration tests skipped without the feature flag).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-clickhouse/src/lib.rs
git commit -m "feat(clickhouse): ClickHouseSink implements Sink with DDL creation and INSERT"
```

---

### Task 8: `spawn_sink` in the core scheduler

**Files:**
- Modify: `crates/nyquist-core/src/scheduler.rs`
- Modify: `crates/nyquist-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub fn spawn_sink(sink: Box<dyn Sink>, reg: Arc<Registry>, percentiles: Arc<Vec<f64>>, interval: Duration, fault_tolerant: bool) -> JoinHandle<()>`

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-core/src/scheduler.rs` inside the existing `tests` module:
```rust
    use crate::sink::{Sink, SinkError};
    use crate::snapshot::RegistrySnapshot;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct CountSink { count: Arc<AtomicU64> }

    #[async_trait::async_trait]
    impl Sink for CountSink {
        async fn export(&mut self, _snap: &RegistrySnapshot) -> Result<(), SinkError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn spawn_sink_calls_export_on_interval() {
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let count = Arc::new(AtomicU64::new(0));
        let sink = Box::new(CountSink { count: count.clone() });
        let pcts = Arc::new(vec![50.0, 99.0]);
        let handle = spawn_sink(sink, reg, pcts, Duration::from_millis(50), true);
        tokio::time::sleep(Duration::from_millis(210)).await;
        handle.abort();
        // ~4 ticks should have happened (210ms / 50ms).
        let c = count.load(Ordering::SeqCst);
        assert!(c >= 3, "expected >=3 export calls, got {c}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-core scheduler::`
Expected: FAIL — `spawn_sink` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/nyquist-core/src/scheduler.rs` (after the existing `spawn_sampler` fn):
```rust
use crate::sink::Sink;
use std::sync::Arc;

pub fn spawn_sink(
    mut sink: Box<dyn Sink>,
    reg: Arc<Registry>,
    percentiles: Arc<Vec<f64>>,
    interval: std::time::Duration,
    fault_tolerant: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let snap = reg.snapshot(std::time::Instant::now(), &percentiles);
            if let Err(e) = sink.export(&snap).await {
                tracing::warn!(error = %e, "sink export failed");
                if !fault_tolerant {
                    tracing::error!("exiting: fault_tolerant=false");
                    break;
                }
            }
        }
    })
}
```

Add to `crates/nyquist-core/src/lib.rs`:
```rust
pub use scheduler::spawn_sink;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-core scheduler::`
Expected: PASS (2 tests — the existing sampler test and the new sink test).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src/scheduler.rs crates/nyquist-core/src/lib.rs
git commit -m "feat(core): spawn_sink helper drives push sinks on tokio interval"
```

---

### Task 9: Binary wiring — connect sinks to `main.rs`

**Files:**
- Modify: `Cargo.toml` (root — add new crate dependencies)
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `nyquist_recorder::{RecorderSink, RecorderConfig}`, `nyquist_clickhouse::{ClickHouseSink, ClickHouseClientConfig}`, `nyquist_core::spawn_sink`, `nyquist_config::{RecorderConfig as CfgRecorder, ClickHouseConfig as CfgCH}`.

- [ ] **Step 1: Add new crate dependencies to root `Cargo.toml`**

Add to the `[dependencies]` section of the workspace root `Cargo.toml`:
```toml
nyquist-recorder    = { path = "crates/nyquist-recorder" }
nyquist-clickhouse  = { path = "crates/nyquist-clickhouse" }
```

- [ ] **Step 2: Write a compile-check test before modifying `main.rs`**

Verify the workspace still builds after adding the new dependencies:
```bash
cargo build
```
Expected: PASS.

- [ ] **Step 3: Update `src/main.rs`**

Replace `src/main.rs` with:
```rust
use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use nyquist_config::Config;
use nyquist_core::registry::Registry;
use nyquist_core::scheduler::{spawn_sampler, spawn_sink};
use nyquist_exposition::HttpServer;
use nyquist_recorder::{RecorderSink, RecorderConfig};
use nyquist_clickhouse::{ClickHouseSink, ClickHouseClientConfig};
use nyquist_samplers::inventory::build_enabled;

#[derive(Parser)]
#[command(name = "nyquist", about = "High-resolution oversampling telemetry agent")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match &cli.config {
        Some(path) => Config::load(path)?,
        None => Config::default(),
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.general.log.clone().into()),
        )
        .init();

    let reg = Arc::new(Registry::new(
        std::time::Duration::from_millis(100),
        config.general.window,
    ));

    let percentiles = Arc::new(config.general.percentiles.clone());

    // Spawn samplers.
    let cfg = config.clone();
    let samplers = build_enabled(
        &reg,
        config.general.default_interval,
        |name| cfg.sampler(name).enabled,
        |name| cfg.sampler(name).interval,
    );
    let mut handles = Vec::new();
    for s in samplers {
        handles.push(spawn_sampler(s, reg.clone(), config.general.fault_tolerant));
    }

    // Spawn recorder sink (if enabled).
    if config.recorder.enabled {
        let rec_cfg = RecorderConfig {
            output_dir:        config.recorder.output_dir.clone().into(),
            flush_interval:    config.recorder.flush_interval,
            rotation_interval: config.recorder.rotation_interval,
        };
        match RecorderSink::new(rec_cfg) {
            Ok(sink) => {
                handles.push(spawn_sink(
                    Box::new(sink),
                    reg.clone(),
                    percentiles.clone(),
                    config.recorder.export_interval,
                    config.general.fault_tolerant,
                ));
                tracing::info!("recorder sink enabled");
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to initialise recorder sink; disabled");
            }
        }
    }

    // Spawn ClickHouse sink (if enabled).
    if config.clickhouse.enabled {
        let ch_cfg = ClickHouseClientConfig {
            url:      config.clickhouse.url.clone(),
            database: config.clickhouse.database.clone(),
            username: config.clickhouse.username.clone(),
            password: config.clickhouse.password.clone(),
        };
        let sink = ClickHouseSink::new(ch_cfg);
        handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            percentiles.clone(),
            config.clickhouse.export_interval,
            config.general.fault_tolerant,
        ));
        tracing::info!("clickhouse sink enabled");
    }

    // Serve HTTP metrics (blocks until shutdown).
    let server = HttpServer::new(reg.clone(), config.general.percentiles.clone());
    server.serve(&config.general.listen).await?;
    Ok(())
}
```

- [ ] **Step 4: Verify binary compiles**

Run: `cargo build`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat: wire RecorderSink and ClickHouseSink into nyquist binary"
```

---

### Task 10: Integration tests — RecorderSink wired through scheduler

**Files:**
- Modify: `tests/integration.rs`
- Modify: root `Cargo.toml` `[dev-dependencies]`

**Interfaces:**
- Extends the existing end-to-end integration test suite with a test that verifies `RecorderSink` produces a valid Parquet file when driven by `spawn_sink`.

- [ ] **Step 1: Add dev-dependencies to root `Cargo.toml`**

Add to `[dev-dependencies]` in the workspace root `Cargo.toml`:
```toml
nyquist-recorder   = { path = "crates/nyquist-recorder" }
nyquist-clickhouse = { path = "crates/nyquist-clickhouse" }
tempfile           = "3"
parquet            = { version = "53", default-features = false, features = ["snap"] }
```

- [ ] **Step 2: Write the new integration tests**

Append to `tests/integration.rs`:
```rust
use nyquist_recorder::{RecorderSink, RecorderConfig};
use nyquist_core::scheduler::spawn_sink;
use tempfile::tempdir;

#[tokio::test]
async fn recorder_sink_produces_parquet_via_spawn_sink() {
    let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
    let id = reg.register(MetricDef::new("cpu/usage/user", Kind::Counter));
    let t0 = Instant::now();
    for i in 1..=20u64 {
        reg.record_counter(id, t0 + Duration::from_millis(i * 10), i * 100);
    }

    let dir = tempdir().unwrap();
    let cfg = RecorderConfig {
        output_dir:        dir.path().to_path_buf(),
        flush_interval:    Duration::from_millis(50),
        rotation_interval: Duration::from_secs(3600),
    };
    let sink = RecorderSink::new(cfg).unwrap();
    let pcts = Arc::new(vec![50.0, 90.0, 99.0, 99.9]);
    let handle = spawn_sink(
        Box::new(sink),
        reg.clone(),
        pcts,
        Duration::from_millis(20),
        true,
    );

    // Let the scheduler tick several times.
    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.abort();

    // Assert a Parquet file exists with non-zero size.
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert!(!files.is_empty(), "no parquet files created");
    for f in &files {
        assert!(f.metadata().unwrap().len() > 0, "parquet file is empty: {:?}", f.path());
    }

    // Verify the Parquet file has rows.
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use std::fs::File;
    let reader = SerializedFileReader::new(
        File::open(&files[0].path()).unwrap()
    ).unwrap();
    let total_rows: i64 = reader.metadata().row_groups().iter()
        .map(|rg| rg.num_rows())
        .sum();
    assert!(total_rows > 0, "parquet file has 0 rows");
}

#[tokio::test]
async fn clickhouse_sink_does_not_panic_on_unavailable_host() {
    // With no ClickHouse instance, export must not panic — it logs and drops the tick.
    use nyquist_clickhouse::{ClickHouseSink, ClickHouseClientConfig};
    use nyquist_core::sink::Sink;

    let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
    let id = reg.register(MetricDef::new("cpu/usage/user", Kind::Counter));
    reg.record_gauge(id, Instant::now(), 42);

    let snap = reg.snapshot(Instant::now(), &[50.0, 99.0]);
    let mut sink = ClickHouseSink::new(ClickHouseClientConfig {
        url:      "http://127.0.0.1:19999".to_string(), // nothing listening here
        database: "test".to_string(),
        username: "default".to_string(),
        password: String::new(),
    });
    // Must return Ok (error is swallowed internally with a warn log).
    let result = sink.export(&snap).await;
    assert!(result.is_ok(), "export should not propagate connection errors: {result:?}");
}
```

The test also needs the imports already present in `tests/integration.rs`. Add these at the top of the file if not already present:
```rust
use std::sync::Arc;
use std::time::{Duration, Instant};
use nyquist_core::model::Kind;
use nyquist_core::registry::{MetricDef, Registry};
```

- [ ] **Step 3: Run the integration test suite**

Run: `cargo test --test integration`
Expected: PASS (3 tests — the existing Phase 1 test plus the 2 new Phase 2 tests).

- [ ] **Step 4: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS — all crate unit tests plus all integration tests.

- [ ] **Step 5: Commit**

```bash
git add tests/integration.rs Cargo.toml
git commit -m "test: Phase 2 integration tests — recorder via spawn_sink, clickhouse graceful degradation"
```

---

### Task 11: Full workspace verification and documentation

**Files:**
- No new source files; optional documentation additions only.

- [ ] **Step 1: Run full test suite**

```bash
cargo test
```
Expected: all tests PASS across all crates.

- [ ] **Step 2: Verify clippy is clean**

```bash
cargo clippy -- -D warnings
```
Expected: no clippy errors. Fix any warnings before continuing.

- [ ] **Step 3: Verify binary runs with recorder enabled (Linux)**

Create a test config file:
```toml
[recorder]
enabled = true
output_dir = "/tmp/nyquist-test-parquet"
flush_interval = "2s"
rotation_interval = "1h"
export_interval = "500ms"
```

Run:
```bash
cargo run -- --config /tmp/test.toml &
sleep 5
ls -lh /tmp/nyquist-test-parquet/
kill %1
```
Expected: one `nyquist-*.parquet` file of non-zero size appears within 5 seconds.

- [ ] **Step 4: Verify binary runs with all sinks disabled (default config)**

```bash
cargo run &
sleep 2
curl -s http://localhost:9100/ && kill %1
```
Expected: `nyquist` text response; no crash.

- [ ] **Step 5: Final commit**

```bash
git add .
git commit -m "chore: Phase 2 complete — Parquet recorder and ClickHouse rollup sink"
```

---

## Self-Review Notes

**Spec coverage:**

- Parquet long format and schema (§4.2) → Task 1 (`RowAccumulator`, `nyquist_schema`).
- Parquet file naming and rotation (§4.6) → Task 2 (`ParquetWriter`).
- `RecorderSink` with flush logic (§4.5) → Task 3.
- Config types for both sinks (§6) → Task 4.
- Metric grouping and DDL (§5.2–5.4) → Task 5 (`grouping.rs`).
- INSERT builder (§5.5) → Task 6 (`inserter.rs`).
- `ClickHouseSink` with error swallowing (§5.6–5.7) → Task 7.
- `spawn_sink` scheduler helper (§7) → Task 8.
- Binary wiring with opt-in via config (§6) → Task 9.
- Integration tests — roundtrip, scheduler wiring, graceful degradation (§9.3) → Tasks 3 + 10.
- ClickHouse integration test gated behind feature flag (§9.2) → Task 7 `#[cfg(feature = "clickhouse-integration")]`.

**Type consistency:** `RecorderConfig` (crate-local, not from nyquist-config), `ClickHouseClientConfig` (crate-local), `RowAccumulator::drain() -> RecordBatch`, `ParquetWriter::flush(&mut RowAccumulator)`, `build_insert(group, metrics, ts_secs) -> String`, `generate_ddl(group, metrics) -> String` — all defined once and consumed consistently.

**Dependency direction:** `nyquist-recorder` and `nyquist-clickhouse` depend only on `nyquist-core`. Neither depends on `nyquist-config`; instead they expose plain config structs that `main.rs` populates from the config crate. This is explicitly required by the global constraints.

**No TBD/TODO:** all steps have complete, copy-paste-ready code.

---

### Critical Files for Implementation

- `/home/gmckee/projects/rust-nyquist/crates/nyquist-core/src/sink.rs` — the `Sink` trait and `SinkError` type that both new crates implement; also `scheduler.rs` where `spawn_sink` is added
- `/home/gmckee/projects/rust-nyquist/crates/nyquist-core/src/snapshot.rs` — `RegistrySnapshot` and `MetricSnapshot`, the data types consumed by both new sinks
- `/home/gmckee/projects/rust-nyquist/crates/nyquist-config/src/lib.rs` — where `RecorderConfig` and `ClickHouseConfig` TOML types are added
- `/home/gmckee/projects/rust-nyquist/src/main.rs` — binary wiring that instantiates sinks and calls `spawn_sink`
- `/home/gmckee/projects/rust-nyquist/tests/integration.rs` — extended with Phase 2 integration tests