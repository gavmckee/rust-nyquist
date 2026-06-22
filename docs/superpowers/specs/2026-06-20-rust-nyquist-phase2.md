# rust-nyquist Phase 2 — Design Specification

**Date:** 2026-06-20
**Status:** Approved for implementation
**Builds on:** `docs/superpowers/specs/2026-06-20-rust-nyquist-design.md` (Phase 1 spec)

---

## 1. Vision

Phase 2 adds two push sinks that consume the existing `RegistrySnapshot` and `Sink` trait seam built in Phase 1. The addition of these sinks does not change `nyquist-core`, `nyquist-samplers`, or `nyquist-exposition` at all — the `Sink` abstraction was designed precisely to allow this.

- **`nyquist-recorder`** writes a Parquet file to local disk at full resolution (one row per metric per export tick), creating a durable high-resolution archive for offline analysis and replay.
- **`nyquist-clickhouse`** rolls up windowed percentiles into a wide analytical table per sampler group and inserts them into ClickHouse on a configurable interval, ideal for dashboards.

Both crates depend only on `nyquist-core`.

---

## 2. Scope

This spec fully designs Phase 2. Phases 3–5 (perf_events, eBPF, remote-write) are unaffected.

**In scope:**
- `crates/nyquist-recorder` — Parquet sink
- `crates/nyquist-clickhouse` — ClickHouse rollup sink
- Config additions to `nyquist-config`
- Binary wiring in `src/main.rs`
- Testing strategy for both crates

**Out of scope:** Parquet replay tooling, ClickHouse query dashboards, Kafka/remote-write (Phase 5).

---

## 3. Architecture

### 3.1 Workspace additions

```
crates/
  nyquist-recorder/
    Cargo.toml
    src/lib.rs           # RecorderSink (Sink impl), re-exports
    src/schema.rs        # Arrow schema constant, RowAccumulator (column buffers)
    src/writer.rs        # ParquetWriter: file handle, row group flushing, rotation
  nyquist-clickhouse/
    Cargo.toml
    src/lib.rs           # ClickHouseSink (Sink impl), re-exports
    src/grouping.rs      # metric grouping by prefix, sanitise(), DDL generation
    src/inserter.rs      # async insert logic over the clickhouse HTTP client
```

Both new crates appear in the workspace `members = ["crates/*"]` already (the glob picks them up). They must be added to the root `Cargo.toml` `[dependencies]` and to `src/main.rs`.

### 3.2 Data flow

```
Registry ──snapshot()──► RegistrySnapshot
                              │
               ┌──────────────┼───────────────────┐
               │              │                   │
        PrometheusHttpSink  RecorderSink    ClickHouseSink
        (pull, on-scrape)   (push, 1s)     (push, 60s)
               │              │                   │
          axum handler    Parquet file        ClickHouse
                          on disk             INSERT
```

### 3.3 Push sink scheduling

Phase 1's binary currently only pushes the scheduler for `Sampler` tasks. Phase 2 adds a `spawn_sink` helper (analogous to `spawn_sampler`) that drives push sinks on their own timer. This helper lives in `nyquist-core::scheduler` and uses the same `tokio::time::interval` pattern.

---

## 4. `nyquist-recorder` — Parquet high-res recorder

### 4.1 Crate dependencies

```toml
[dependencies]
nyquist-core  = { path = "../nyquist-core" }
arrow-array   = "53"
arrow-schema  = "53"
parquet       = "53"
async-trait   = "0.1"
thiserror     = "1"
tracing       = "0.1"

[dev-dependencies]
tempfile      = "3"
```

The `arrow-array`, `arrow-schema`, and `parquet` crates are all from the Apache `arrow-rs` repository and must share the same major version (`53`). The `parquet` crate at version 53 re-exports `arrow_schema` and `arrow_array` compatibility helpers; pinning all three to `"53"` avoids type-mismatch compile errors.

### 4.2 Parquet schema — long format

Each snapshot export produces one row per `MetricSnapshot`. Long format is chosen over wide because:

- New metrics appear automatically without schema evolution.
- The schema is fixed at crate compile time.
- Consumers filter by `name` column; columnar storage makes this cheap.

**Schema (Arrow `Schema`):**

| Column | Arrow type | Notes |
|---|---|---|
| `ts_unix_ms` | `Int64` | Milliseconds since Unix epoch (`SystemTime` → `UNIX_EPOCH.elapsed()`) |
| `name` | `Utf8` | Metric name as registered, e.g. `"cpu/usage/user"` |
| `labels_json` | `Utf8` | JSON object `{"cpu":"0","core":"1"}` or `"{}"` for no labels |
| `kind` | `Utf8` | `"counter"`, `"gauge"`, or `"distribution"` |
| `unit` | `Utf8` | `"bytes"`, `"count"`, `"seconds"`, `"percent"`, or `"none"` |
| `raw` | `UInt64` | `MetricSnapshot::raw` |
| `p50` | `UInt64` | 50th percentile value |
| `p90` | `UInt64` | 90th percentile value |
| `p99` | `UInt64` | 99th percentile value |
| `p99_9` | `UInt64` | 99.9th percentile value |

The percentile columns are always present. If a snapshot is taken with a different `percentiles` config, values default to `0` for absent percentile levels. This keeps the schema stable independent of config.

### 4.3 `RowAccumulator` (in `schema.rs`)

An in-memory columnar buffer. Stores one `Vec<T>` per column. The `push()` method accepts a `&MetricSnapshot` and the snapshot's `ts_unix_ms`. On `drain()` it produces Arrow arrays suitable for writing a Parquet row group, then clears the vecs.

```rust
pub struct RowAccumulator {
    ts_unix_ms: Vec<i64>,
    name: Vec<String>,
    labels_json: Vec<String>,
    kind: Vec<String>,
    unit: Vec<String>,
    raw: Vec<u64>,
    p50: Vec<u64>,
    p90: Vec<u64>,
    p99: Vec<u64>,
    p99_9: Vec<u64>,
}
```

`push()` extracts p50/p90/p99/p99_9 from `MetricSnapshot::percentiles` by matching the `f64` key (50.0, 90.0, 99.0, 99.9), defaulting missing levels to `0`.

`is_empty() -> bool` and `len() -> usize` are provided for flush-decision logic.

`labels_json(labels: &Labels) -> String` is a standalone helper that serializes the `BTreeMap` behind `Labels` as a compact JSON object without pulling in `serde_json`.

### 4.4 `ParquetWriter` (in `writer.rs`)

Owns the file handle and rotation state. Internally holds:
- `current_file: Option<ArrowWriter<File>>` — the Arrow/Parquet columnar writer wrapping the open file.
- `rotation_due: Instant` — wall time when the next file rotation occurs.
- `output_dir: PathBuf` and `rotation_interval: Duration`.

**`open_new_file()`** creates a file named `nyquist-{unix_seconds}.parquet` in `output_dir` and sets `rotation_due = now + rotation_interval`. Calls `ArrowWriter::try_new(file, schema, Some(props))` where `props` uses Snappy compression (default in `parquet::file::properties`).

**`flush(&mut self, acc: &mut RowAccumulator)`** — if `acc` is non-empty: drain the accumulator into Arrow arrays, call `writer.write(&RecordBatch)`, call `writer.flush()`. This produces a valid readable row group in the file without closing it.

**`maybe_rotate(&mut self)`** — if `now >= rotation_due`: close the current writer (which finalises the file footer) and open a new file. Called before each flush.

**`close()`** — close the current writer cleanly (called on agent shutdown, future work).

### 4.5 `RecorderSink` (in `lib.rs`)

Implements `Sink`. Owns a `RowAccumulator` and a `ParquetWriter`. On `export()`:

1. Convert `snap.captured` (SystemTime) to `ts_unix_ms`.
2. For each `MetricSnapshot` in the snapshot, call `acc.push(ts, metric)`.
3. If it is time to flush (tracked by a `next_flush: Instant` field), call `writer.maybe_rotate()` then `writer.flush(&mut acc)` then advance `next_flush`.

The flush decision is time-based (`flush_interval`, default 10s), not count-based, so files are always readable within 10s of any data arriving.

`RecorderSink::new(cfg: RecorderConfig) -> Result<Self, RecorderError>` creates the output directory if missing and opens the first Parquet file immediately.

### 4.6 File naming and rotation

File names: `nyquist-{unix_timestamp_seconds}.parquet` where the timestamp is taken at the moment the file is opened. Example: `nyquist-1750425600.parquet`.

A new file is opened on startup and on rotation. The rotation interval is configurable (`rotation_interval`, default `1h`). On rotation, the current file is closed (footer written, file is now valid and self-contained) before the new file is opened.

### 4.7 Error handling

`RecorderError` (thiserror) covers:
- `Io(std::io::Error)` — directory creation, file open, file write.
- `Arrow(parquet::errors::ParquetError)` — row group write failure.

On flush error, the sink logs a warning and skips that flush (data for that interval is lost). On file-open error, the sink logs an error and disables itself for that rotation period. This matches the Phase 1 philosophy: one failing sink must not crash the agent.

---

## 5. `nyquist-clickhouse` — ClickHouse rollup sink

### 5.1 Crate dependencies

```toml
[dependencies]
nyquist-core  = { path = "../nyquist-core" }
clickhouse    = "0.13"
async-trait   = "0.1"
thiserror     = "1"
tracing       = "0.1"
tokio         = { version = "1", features = ["rt", "time"] }

[dev-dependencies]
tokio         = { version = "1", features = ["macros", "rt-multi-thread"] }

[features]
clickhouse-integration = []
```

The `clickhouse = "0.13"` crate provides an async ClickHouse HTTP client with row-derive macros. Phase 2 does not use the derive macros (schema is dynamic), but the crate is used for its connection management and query execution.

### 5.2 Metric grouping

Metrics are grouped by the **first path component** of their name. Examples:

| Metric name | Group |
|---|---|
| `cpu/usage/user` | `cpu` |
| `memory/memfree` | `memory` |
| `network/receive/bytes` | `network` |
| `disk/read/bytes` | `disk` |
| `tcp/in_segs` | `tcp` |
| `udp/in_datagrams` | `udp` |

The `group_metrics(snap: &RegistrySnapshot) -> BTreeMap<String, Vec<&MetricSnapshot>>` function in `grouping.rs` splits on `/` and uses the first component, or the whole name if there is no `/`.

### 5.3 Column name sanitisation

Column names in ClickHouse SQL must be valid identifiers. `sanitise(name: &str, labels: &Labels) -> String` produces a column-name string by:

1. Replace `/` and `-` with `_`.
2. Replace `.` with `_`.
3. Append label values sorted by label key, joined with `_`, if labels are non-empty. Example: `network/receive/bytes` with `iface=eth0` → `network_receive_bytes_eth0`.
4. The result is the **base identifier**; individual percentile/raw columns append `_raw`, `_p50`, `_p90`, `_p99`, `_p99_9`.

### 5.4 Wide-row schema and DDL generation

Each group gets a ClickHouse table named `nyquist_{group}`. The DDL is:

```sql
CREATE TABLE IF NOT EXISTS nyquist_{group}
(
    ts DateTime,
    {per_metric_columns}
)
ENGINE = MergeTree()
ORDER BY ts
```

Where `{per_metric_columns}` is generated from the first non-empty snapshot for that group:

```
{sanitised_name}_raw   UInt64,
{sanitised_name}_p50   UInt64,
{sanitised_name}_p90   UInt64,
{sanitised_name}_p99   UInt64,
{sanitised_name}_p99_9 UInt64,
```

One DDL block per `MetricSnapshot` in the group, sorted by sanitised name for determinism.

`generate_ddl(group: &str, metrics: &[&MetricSnapshot]) -> String` in `grouping.rs` produces this string. DDL is executed with `CREATE TABLE IF NOT EXISTS`, so re-running it is idempotent.

### 5.5 Insert logic (in `inserter.rs`)

Since the schema is dynamic, inserts use raw SQL rather than the typed derive-macro path.

**Insert format** (ClickHouse VALUES format via the `clickhouse` crate's `query()` API):

```sql
INSERT INTO nyquist_{group} (ts, col1_raw, col1_p50, ...) VALUES (...)
```

`build_insert(group: &str, metrics: &[&MetricSnapshot], ts: DateTime) -> String` constructs the full `INSERT ... VALUES (...)` statement as a `String`, then the `Inserter` executes it with `client.query(&sql).execute().await`.

**Batching:** All groups' DDL (on first export) and all groups' inserts are issued sequentially in one export tick. There is no transaction across groups (ClickHouse does not have multi-table transactions), but each INSERT is atomic at the ClickHouse level.

**DDL caching:** `ClickHouseSink` tracks `tables_created: BTreeSet<String>`. On the first export that includes a group, it issues the DDL and adds the group to the set. Subsequent exports skip DDL.

### 5.6 `ClickHouseSink` (in `lib.rs`)

Implements `Sink`. Owns a `clickhouse::Client` and the `tables_created` set.

`ClickHouseSink::new(cfg: ClickHouseConfig) -> Self` builds the client from `cfg.url`, `cfg.database`, `cfg.username`, `cfg.password`.

On `export()`:
1. Group metrics with `group_metrics`.
2. For each group: if not in `tables_created`, execute DDL, insert into set.
3. For each group: call `build_insert`, execute query.
4. On any error: log warning, skip that group for this tick. Never retry infinitely — the next tick retries automatically.

### 5.7 Error handling

`ClickHouseError` covers:
- `Connect(clickhouse::error::Error)` — client error on DDL or insert.
- The sink itself returns `Ok(())` from `export()` even on insert failure (it logs and drops the tick). This is intentional: a ClickHouse outage must not cascade to stop the agent.

---

## 6. Configuration additions

New sections added to `nyquist-config`:

```toml
[recorder]
enabled           = false
output_dir        = "/var/lib/nyquist/parquet"
flush_interval    = "10s"
rotation_interval = "1h"
export_interval   = "1s"

[clickhouse]
enabled         = false
url             = "http://localhost:8123"
database        = "nyquist"
username        = "default"
password        = ""
export_interval = "60s"
```

**Rust types in `nyquist-config`:**

```rust
#[derive(Debug, Clone, Deserialize, Default)]
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

#[derive(Debug, Clone, Deserialize, Default)]
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
```

Both have `Default` implementations (disabled by default). The defaults for durations use non-zero sentinels to avoid divide-by-zero in scheduler math; the `Default` impl hardcodes `flush_interval = 10s`, `rotation_interval = 3600s`, `export_interval_recorder = 1s`, `export_interval_clickhouse = 60s`.

`Config` gains two new fields: `pub recorder: RecorderConfig` and `pub clickhouse: ClickHouseConfig`.

---

## 7. Push sink scheduler

`nyquist-core::scheduler` gains:

```rust
pub fn spawn_sink(
    mut sink: Box<dyn Sink>,
    reg: Arc<Registry>,
    percentiles: Arc<Vec<f64>>,
    interval: Duration,
    fault_tolerant: bool,
) -> JoinHandle<()>
```

This spawns a tokio task that calls `reg.snapshot(Instant::now(), &percentiles)` on each tick and passes the snapshot to `sink.export()`. Errors are logged. When `fault_tolerant` is false, the task breaks on error (consistent with sampler behaviour).

---

## 8. Error handling

- `RecorderError` and `ClickHouseError` use `thiserror`. They are not exposed through the `Sink` trait — `export()` returns `Box<dyn std::error::Error + Send + Sync>` (the existing `SinkError` alias).
- Sink errors logged at `WARN` level; never panic.
- Missing `output_dir` is created by `RecorderSink::new()` with `std::fs::create_dir_all`.
- ClickHouse connection failures on the first export log at `ERROR` and skip the tick; the sink remains alive for the next tick.

---

## 9. Testing strategy

### 9.1 `nyquist-recorder`

**Unit — `RowAccumulator`** (`crates/nyquist-recorder/src/schema.rs`):
- Given a `RegistrySnapshot` with known values, `push()` fills the column vecs correctly.
- `labels_json` serialises `Labels` to compact JSON with sorted keys.
- `drain()` produces Arrow arrays of the correct length and values.

**Unit — `ParquetWriter`** (`crates/nyquist-recorder/src/writer.rs`):
- Rotation triggers when `now >= rotation_due`.
- `flush()` with an empty accumulator is a no-op (no panic, no file written).

**Integration — round-trip** (`crates/nyquist-recorder/tests/roundtrip.rs`):
- Create a `RecorderSink` with `output_dir = tempdir.path()` and `flush_interval = 0s` (force flush every export).
- Call `export()` twice with two snapshots.
- Open the resulting `.parquet` file with `parquet::file::reader::SerializedFileReader`.
- Assert row count == 2 × (number of metrics), and that `ts_unix_ms`, `name`, `raw` have the expected values.

### 9.2 `nyquist-clickhouse`

**Unit — `grouping.rs`** (no network):
- `group_metrics` correctly separates a mixed snapshot into `cpu`, `memory`, `network` groups.
- `sanitise("network/receive/bytes", labels_eth0)` returns `"network_receive_bytes_eth0"`.
- `generate_ddl("cpu", metrics)` produces a string containing `CREATE TABLE IF NOT EXISTS nyquist_cpu` and `ts DateTime`.
- `build_insert("cpu", metrics, ts)` produces a string containing `INSERT INTO nyquist_cpu` and the correct number of value placeholders.

**Unit — `ClickHouseSink::new`** with a mock URL does not panic (it only builds the client struct; no connection is attempted until the first query).

**Integration** — gated behind `#[cfg(feature = "clickhouse-integration")]`. Not run in CI unless a ClickHouse instance is available. The integration test would create the sink pointing at `http://localhost:8123`, export a snapshot, and query ClickHouse to assert the row exists.

### 9.3 Binary wiring integration test

Extend `tests/integration.rs` with a test that:
1. Creates a `RecorderSink` with a `tempdir` output.
2. Calls `export()` with a `RegistrySnapshot` containing two metrics.
3. Asserts that a `.parquet` file exists in the tempdir and has non-zero size.

This tests the wiring path without requiring the full scheduler.

---

## 10. Platform notes

`nyquist-recorder` is platform-independent (pure Rust file I/O). `nyquist-clickhouse` is platform-independent (HTTP client). Both compile on macOS. The only Linux-specific code remains in `nyquist-samplers` as in Phase 1.

---

## 11. Open items deferred to later phases

- Parquet replay tooling and schema evolution (append new columns) — later Phase 2 addendum or Phase 5.
- ClickHouse `ReplacingMergeTree` / TTL configuration — Phase 5.
- Remote-write / Kafka output — Phase 5.
- perf_events samplers — Phase 3.
- eBPF (Aya) samplers — Phase 4.

---

## Phase 2 Implementation Plan

`docs/superpowers/plans/2026-06-20-rust-nyquist-phase2.md`

---