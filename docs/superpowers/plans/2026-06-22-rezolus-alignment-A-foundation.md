# Rezolus Alignment — Plan A: Non-BPF Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the consumer-driven, bucket-array data model and `linkme` sampler registration from the Rezolus Alignment design (`docs/superpowers/specs/2026-06-22-rezolus-alignment-design.md`), plus the coverage-parity governance — all of which is pure Rust and builds/tests on macOS. The eBPF toolchain + TCP reference slice are Plan B.

**Architecture:** Histograms flow downstream as bucket arrays; percentile choice moves out of `Registry` into a shared sink-side helper (design §3.4). `MetricSnapshot` carries a sparse `(upper_bound, count)` bucket array instead of pre-computed percentiles. Sampler registration switches from the manual `inventory.rs` factory to a `linkme` distributed slice `SAMPLERS` declared in `nyquist-core` (design §3.3) so that Plan B's BPF samplers can register into the same slice. A golden coverage baseline + parity-gate test (design §6) guards against metric regressions.

**Tech Stack:** Rust (stable, edition 2021), `histogram = "0.11"`, `linkme`, `dashmap`, `tokio`, `axum`, `async-trait`.

## Global Constraints

- **Builds on macOS and Linux.** Every task in this plan compiles and tests on the dev Mac; no `/proc`, BPF, or clang required. (Design §2 "Acknowledged consequence": the windowed path survives for procfs/perf samplers.)
- **Zero metric regressions.** The set of emitted `(metric name, label-key set, kind, unit)` tuples must never shrink (design §6.3). Any deliberate rename is recorded as an explicit mapping, never a silent drop.
- **H2 grouping power stays 7 for the windowed path.** `crates/nyquist-core/src/hist/slice.rs` already uses `GROUPING_POWER = 7`, `MAX_VALUE_POWER = 39`. Do not change these in Plan A. (Plan B introduces `grouping_power = 3` for the in-kernel BPF histogram only.)
- **`histogram` 0.11 bucket-iterator API is unverified offline.** The crate is not in the local cargo cache. Task A1 Step 1 pins the exact names (`(&Histogram).into_iter()` yielding `Bucket`, `Bucket::count()`, `Bucket::end()`) via `cargo doc`; if they differ, adjust at the single call site in `sliding.rs::bucket_counts` and nowhere else.
- **Frequent commits.** One commit per task minimum; each task ends green (`cargo test --workspace`).
- **Percentile config is a consumer concern.** After this plan, each sink/exposition holds its own percentile list and computes percentiles from bucket arrays at read time. The collection path (Registry) no longer knows about percentiles.

---

### Task A1: Bucket-array snapshot + sink-side percentile helper (nyquist-core)

This is the load-bearing data-model change. It removes pre-computed percentiles from `MetricSnapshot` and replaces them with a sparse H2 bucket array, and adds the shared helper that consumers call to recover percentiles. Everything downstream (Task A2) depends on it.

**Files:**
- Modify: `crates/nyquist-core/src/hist/slice.rs` (expose grouping consts; no behavior change)
- Modify: `crates/nyquist-core/src/hist/sliding.rs:60-71` (add `bucket_counts`)
- Modify: `crates/nyquist-core/src/hist/mod.rs` (re-export, if it gates the module)
- Create: `crates/nyquist-core/src/percentiles.rs`
- Modify: `crates/nyquist-core/src/lib.rs` (add `pub mod percentiles;`)
- Modify: `crates/nyquist-core/src/snapshot.rs:4-12` (replace `percentiles` field with `buckets`)
- Modify: `crates/nyquist-core/src/registry.rs:100-120` (snapshot drops the `percentiles` arg, fills `buckets`)

**Interfaces:**
- Produces:
  - `nyquist_core::hist::HIST_GROUPING_POWER: u8` (= 7) and `HIST_MAX_VALUE_POWER: u8` (= 39)
  - `SlidingHistogram::bucket_counts(&mut self, now: Instant) -> Vec<(u64, u64)>` — sparse `(upper_bound, count)` for non-empty buckets, ascending.
  - `nyquist_core::percentiles::percentiles_from_buckets(buckets: &[(u64, u64)], ps: &[f64]) -> Vec<(f64, u64)>`
  - `MetricSnapshot.buckets: Vec<(u64, u64)>` (replaces `MetricSnapshot.percentiles`)
  - `Registry::snapshot(&self, now: Instant) -> RegistrySnapshot` (signature loses `percentiles: &[f64]`)
- Consumed by: Task A2 (all sinks + exposition), Plan B (BPF samplers populate `buckets` directly via a new Registry path).

- [ ] **Step 1: Verify the `histogram` 0.11 bucket-iterator API**

Run:
```bash
cargo doc -p histogram --no-deps 2>/dev/null; \
find target/doc/histogram -name "*.html" | head; \
grep -rno "fn end\|fn count\|fn start\|impl IntoIterator" target/doc/histogram/*.html 2>/dev/null | head
```
Expected: confirm `Bucket` exposes `count()` and `end()`, and that a `&Histogram` (or `Histogram`) is iterable yielding `Bucket`. If offline and `cargo doc` cannot fetch, open https://docs.rs/histogram/0.11.5 and confirm the same. Record the exact spelling; the only call site is `bucket_counts` below.

- [ ] **Step 2: Expose grouping-power constants in `slice.rs`**

In `crates/nyquist-core/src/hist/slice.rs`, change the two private consts to public and re-export them. Replace lines 3-6:
```rust
/// Grouping power 7 (~1% relative error) and max value power 39
/// (covers up to ~5.4e11, enough for byte/sec rates).
pub const HIST_GROUPING_POWER: u8 = 7;
pub const HIST_MAX_VALUE_POWER: u8 = 39;
```
Then update the internal references in this file from `GROUPING_POWER`/`MAX_VALUE_POWER` to `HIST_GROUPING_POWER`/`HIST_MAX_VALUE_POWER` (4 occurrences: lines 15, 25, 37, plus the test at 53 uses `HistogramSlice::new()` so no change there).

- [ ] **Step 3: Re-export the consts from the hist module**

In `crates/nyquist-core/src/hist/mod.rs`, ensure the consts are reachable as `nyquist_core::hist::HIST_GROUPING_POWER`. Add to the existing re-exports:
```rust
pub use slice::{HIST_GROUPING_POWER, HIST_MAX_VALUE_POWER};
```
(If `mod.rs` already does `pub use slice::*;` this is redundant — skip. Read the file first.)

- [ ] **Step 4: Write the failing test for `bucket_counts`**

Add to the `tests` module in `crates/nyquist-core/src/hist/sliding.rs`:
```rust
    #[test]
    fn bucket_counts_are_sparse_and_ascending() {
        let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(1));
        let t0 = Instant::now();
        for _ in 0..10 { h.record(t0, 5); }
        for _ in 0..3 { h.record(t0, 1000); }
        let buckets = h.bucket_counts(t0 + Duration::from_millis(50));
        // Non-empty buckets only.
        assert!(buckets.iter().all(|&(_, c)| c > 0), "empty bucket leaked: {buckets:?}");
        // Ascending by upper bound.
        let bounds: Vec<u64> = buckets.iter().map(|&(b, _)| b).collect();
        let mut sorted = bounds.clone();
        sorted.sort_unstable();
        assert_eq!(bounds, sorted, "buckets not ascending: {buckets:?}");
        // Total count is preserved.
        let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
        assert_eq!(total, 13, "total count wrong: {buckets:?}");
    }
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p nyquist-core bucket_counts_are_sparse_and_ascending`
Expected: FAIL — `no method named bucket_counts`.

- [ ] **Step 6: Implement `bucket_counts`**

Add to `impl SlidingHistogram` in `crates/nyquist-core/src/hist/sliding.rs` (after `percentile_batch`, around line 70):
```rust
    /// Merge the window and return the non-empty H2 buckets as
    /// `(upper_bound, count)` pairs in ascending order. This is the
    /// downstream bucket-array representation (design §3.4); consumers
    /// compute percentiles from it via `percentiles_from_buckets`.
    pub fn bucket_counts(&mut self, now: Instant) -> Vec<(u64, u64)> {
        let acc = self.merge_window(now);
        (&acc)
            .into_iter()
            .filter(|b| b.count() > 0)
            .map(|b| (b.end(), b.count()))
            .collect()
    }
```
(If Step 1 found different method names, adjust `b.count()` / `b.end()` / `(&acc).into_iter()` here only.)

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p nyquist-core bucket_counts_are_sparse_and_ascending`
Expected: PASS.

- [ ] **Step 8: Write the failing test for `percentiles_from_buckets`**

Create `crates/nyquist-core/src/percentiles.rs`:
```rust
//! Consumer-side percentile computation from H2 bucket arrays (design §3.4).
//! The collection path emits full bucket arrays; the percentile set is a
//! consumer concern, computed here at read time.

/// Compute percentiles from a sparse H2 bucket array.
///
/// `buckets` is `(upper_bound, count)` pairs sorted ascending by upper bound
/// (as produced by `SlidingHistogram::bucket_counts`). `ps` are percentiles in
/// `0.0..=100.0`. Returns `(percentile, value)` pairs in the same order as `ps`.
/// The returned value is the upper bound of the bucket containing the rank,
/// matching the `histogram` crate's percentile semantics.
pub fn percentiles_from_buckets(buckets: &[(u64, u64)], ps: &[f64]) -> Vec<(f64, u64)> {
    let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
    if total == 0 {
        return ps.iter().map(|&p| (p, 0)).collect();
    }
    ps.iter()
        .map(|&p| {
            let rank = ((p / 100.0) * total as f64).ceil() as u64;
            let rank = rank.clamp(1, total);
            let mut cum = 0u64;
            let mut val = 0u64;
            for &(end, count) in buckets {
                cum += count;
                if cum >= rank {
                    val = end;
                    break;
                }
            }
            (p, val)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buckets_yield_zero() {
        let out = percentiles_from_buckets(&[], &[50.0, 99.0]);
        assert_eq!(out, vec![(50.0, 0), (99.0, 0)]);
    }

    #[test]
    fn single_bucket_returns_its_bound() {
        let out = percentiles_from_buckets(&[(42, 100)], &[50.0, 99.9]);
        assert_eq!(out, vec![(50.0, 42), (99.9, 42)]);
    }

    #[test]
    fn percentiles_pick_the_rank_bucket() {
        // 90 samples at bound 10, 10 samples at bound 1000.
        let buckets = [(10u64, 90u64), (1000u64, 10u64)];
        let out = percentiles_from_buckets(&buckets, &[50.0, 90.0, 99.0]);
        assert_eq!(out[0], (50.0, 10));   // rank 50 -> first bucket
        assert_eq!(out[1], (90.0, 10));   // rank 90 -> still first bucket
        assert_eq!(out[2], (99.0, 1000)); // rank 99 -> second bucket
    }

    #[test]
    fn percentiles_are_monotonic() {
        let buckets = [(1u64, 5u64), (10u64, 5u64), (100u64, 5u64), (1000u64, 5u64)];
        let out = percentiles_from_buckets(&buckets, &[50.0, 90.0, 99.0, 99.9]);
        let vals: Vec<u64> = out.iter().map(|&(_, v)| v).collect();
        let mut sorted = vals.clone();
        sorted.sort_unstable();
        assert_eq!(vals, sorted, "percentiles not monotonic: {vals:?}");
    }
}
```

- [ ] **Step 9: Wire the module and run the failing test**

Add to `crates/nyquist-core/src/lib.rs` (alongside the other `pub mod` lines):
```rust
pub mod percentiles;
```
Run: `cargo test -p nyquist-core percentiles::`
Expected: PASS (the implementation is included in Step 8; this step confirms it compiles and passes).

- [ ] **Step 10: Change `MetricSnapshot` to carry buckets instead of percentiles**

In `crates/nyquist-core/src/snapshot.rs`, replace lines 4-12:
```rust
#[derive(Clone, Debug)]
pub struct MetricSnapshot {
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    pub labels: Labels,
    pub raw: u64,
    /// Sparse H2 bucket array `(upper_bound, count)`, ascending. Consumers
    /// compute percentiles from this via `percentiles::percentiles_from_buckets`.
    pub buckets: Vec<(u64, u64)>,
}
```
Update the test at lines 27-40 to assert on buckets instead of percentiles:
```rust
    #[test]
    fn snapshot_contains_raw_and_buckets() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("g", Kind::Gauge));
        let t0 = Instant::now();
        for i in 0..50u64 { reg.record_gauge(id, t0 + Duration::from_millis(i * 10), 7); }
        let snap = reg.snapshot(t0 + Duration::from_millis(500));
        let m = snap.metrics.iter().find(|m| m.name == "g").unwrap();
        assert_eq!(m.raw, 7);
        let total: u64 = m.buckets.iter().map(|&(_, c)| c).sum();
        assert!(total > 0, "no buckets recorded");
        // Recover p50 via the consumer helper; value 7 fits a small bucket.
        let pcts = nyquist_core::percentiles::percentiles_from_buckets(&m.buckets, &[50.0]);
        assert!((7..=8).contains(&pcts[0].1), "p50 was {}", pcts[0].1);
    }
```

- [ ] **Step 11: Update `Registry::snapshot` to drop percentiles and fill buckets**

In `crates/nyquist-core/src/registry.rs`, replace `snapshot` (lines 100-120):
```rust
    pub fn snapshot(&self, now: std::time::Instant) -> crate::snapshot::RegistrySnapshot {
        let mut metrics = Vec::new();
        for entry in self.metrics.iter() {
            let mut s = entry.value().lock().unwrap();
            let buckets = s.window.bucket_counts(now);
            metrics.push(crate::snapshot::MetricSnapshot {
                name: s.def.name.clone(),
                kind: s.def.kind,
                unit: s.def.unit,
                labels: s.def.labels.clone(),
                raw: s.raw,
                buckets,
            });
        }
        crate::snapshot::RegistrySnapshot { metrics, captured: std::time::SystemTime::now() }
    }
```

- [ ] **Step 12: Run the whole core test suite**

Run: `cargo test -p nyquist-core`
Expected: PASS. (The `registry.rs` percentile unit tests at lines 129-166 still use `reg.percentile(...)`, which is unchanged — `percentile`/`percentile_batch` on the window stay for now. Only `snapshot` changed.)

- [ ] **Step 13: Commit**

```bash
git add crates/nyquist-core/src/hist/slice.rs crates/nyquist-core/src/hist/sliding.rs \
        crates/nyquist-core/src/hist/mod.rs crates/nyquist-core/src/percentiles.rs \
        crates/nyquist-core/src/lib.rs crates/nyquist-core/src/snapshot.rs \
        crates/nyquist-core/src/registry.rs
git commit -m "feat(core): expose H2 bucket arrays in snapshot; move percentiles to consumer helper"
```

---

### Task A2: Move percentile computation into the sinks/exposition

`Registry::snapshot` no longer computes percentiles. Each consumer (Prometheus text, JSON, ClickHouse, Parquet, VictoriaMetrics push) now computes its own percentile set from `MetricSnapshot.buckets` via the helper. The `Sink::export` path and `spawn_sink` lose the percentile argument; instead each consumer owns its percentile list.

**Files:**
- Modify: `crates/nyquist-core/src/scheduler.rs:32-54` (`spawn_sink` drops `percentiles` from the `snapshot` call)
- Modify: `crates/nyquist-exposition/src/format.rs:18-48` (`to_prometheus`/`to_json` take `percentiles: &[f64]`)
- Modify: `crates/nyquist-exposition/src/server.rs` (HttpServer passes its percentiles to formatters)
- Modify: `crates/nyquist-exposition/src/push.rs` (VictoriaMetrics sink stores + uses percentiles)
- Modify: `crates/nyquist-clickhouse/src/inserter.rs:22-34` (`from_snapshot` takes percentiles)
- Modify: `crates/nyquist-clickhouse/src/sink.rs` (sink stores percentiles)
- Modify: `crates/nyquist-recorder/src/sink.rs` + `crates/nyquist-recorder/src/*` row accumulator (compute percentiles from buckets)
- Modify: `src/main.rs:74-132` (pass `config.general.percentiles` into each sink constructor instead of `spawn_sink`)

**Interfaces:**
- Consumes: `MetricSnapshot.buckets`, `percentiles::percentiles_from_buckets` (Task A1).
- Produces: `to_prometheus(snap, percentiles)`, `to_json(snap, percentiles)`; `spawn_sink(sink, reg, export_interval, fault_tolerant)` (no percentiles param).

- [ ] **Step 1: Update `to_prometheus` / `to_json` signatures (failing test)**

In `crates/nyquist-exposition/src/format.rs`, update the two tests first to the new signatures and bucket-based fixture. Replace `sample_snapshot` (lines 57-69) and the two tests:
```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p nyquist-exposition --lib format::`
Expected: FAIL — `to_prometheus`/`to_json` take 1 arg, not 2; `MetricSnapshot` has no `percentiles` field.

- [ ] **Step 3: Implement the new formatter signatures**

In `crates/nyquist-exposition/src/format.rs`, replace `to_prometheus` and `to_json` (lines 18-48):
```rust
use nyquist_core::percentiles::percentiles_from_buckets;

pub fn to_prometheus(snap: &RegistrySnapshot, percentiles: &[f64]) -> String {
    let mut out = String::new();
    for m in &snap.metrics {
        let base = sanitize(&m.name);
        let kind = match m.kind { Kind::Counter => "counter", Kind::Gauge => "gauge", Kind::Distribution => "summary" };
        out.push_str(&format!("# TYPE {base} {kind}\n"));
        out.push_str(&format!("{base}{} {}\n", label_str(&m.labels, None), m.raw));
        let suffix = match m.kind { Kind::Gauge => "value", _ => "rate" };
        for (p, v) in percentiles_from_buckets(&m.buckets, percentiles) {
            out.push_str(&format!(
                "{base}_{suffix}{} {}\n",
                label_str(&m.labels, Some(("percentile", &fmt_pct(p)))),
                v
            ));
        }
    }
    out
}

pub fn to_json(snap: &RegistrySnapshot, percentiles: &[f64]) -> String {
    let mut items = Vec::new();
    for m in &snap.metrics {
        let pcts: Vec<String> = percentiles_from_buckets(&m.buckets, percentiles)
            .iter()
            .map(|(p, v)| format!("\"{}\":{}", fmt_pct(*p), v))
            .collect();
        items.push(format!(
            "{{\"name\":\"{}\",\"raw\":{},\"percentiles\":{{{}}}}}",
            m.name, m.raw, pcts.join(",")
        ));
    }
    format!("[{}]", items.join(","))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p nyquist-exposition --lib format::`
Expected: PASS.

- [ ] **Step 5: Update HttpServer call sites**

In `crates/nyquist-exposition/src/server.rs`, the `metrics` / `metrics_json` handlers call `to_prometheus(&snap)` / `to_json(&snap)`. The server already holds `percentiles` (constructed via `HttpServer::new(reg, percentiles)`). Thread them through:
- In the handler, the snapshot is produced from state; change `s.snapshot().await` if it passes percentiles to `reg.snapshot` — update to `reg.snapshot(Instant::now())` (no percentiles).
- Change `to_prometheus(&snap)` → `to_prometheus(&snap, &s.percentiles)` and `to_json(&snap)` → `to_json(&snap, &s.percentiles)`.

Read `server.rs` fully first; the `AppState` carries `reg` + `percentiles`. Ensure `percentiles` is stored on `AppState` (it is, since `HttpServer::new` takes it). Adjust field access to match the actual struct.

- [ ] **Step 6: Update the VictoriaMetrics push sink**

In `crates/nyquist-exposition/src/push.rs`, the sink calls `to_prometheus(snapshot)`. Add a `percentiles: Vec<f64>` field to `VictoriaMetricsSink`, set it in `new`, and call `to_prometheus(snapshot, &self.percentiles)`:
```rust
pub struct VictoriaMetricsSink {
    url: String,
    client: reqwest::Client,
    percentiles: Vec<f64>,
}

impl VictoriaMetricsSink {
    pub fn new(url: &str, percentiles: Vec<f64>) -> Self {
        VictoriaMetricsSink { url: url.to_string(), client: reqwest::Client::new(), percentiles }
    }
}
```
In `export`, change `let body = to_prometheus(snapshot);` → `let body = to_prometheus(snapshot, &self.percentiles);`.

- [ ] **Step 7: Update the ClickHouse sink + inserter**

In `crates/nyquist-clickhouse/src/inserter.rs`, `from_snapshot` currently calls `pct(&m.percentiles, 50.0)` etc. Change it to compute from buckets:
```rust
use nyquist_core::percentiles::percentiles_from_buckets;

pub fn from_snapshot(ts_ms: i64, m: &MetricSnapshot) -> Self {
    let p = percentiles_from_buckets(&m.buckets, &[50.0, 90.0, 99.0, 99.9]);
    let get = |target: f64| p.iter().find(|(pp, _)| *pp == target).map(|(_, v)| *v).unwrap_or(0);
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
```
The ClickHouse schema (`p50/p90/p99/p999`) is fixed columns, so the percentile set here stays `[50,90,99,99.9]` (design non-goal: no schema redesign). Remove the now-unused `pct(&m.percentiles, ...)` helper if it referenced the deleted field. Check `crates/nyquist-clickhouse/src/sink.rs` for any `snapshot(now, &percentiles)` call and change to `snapshot(now)`.

- [ ] **Step 8: Update the Parquet recorder**

In `crates/nyquist-recorder/src/`, the row accumulator writes `p50/p90/p99/p99_9` columns (schema at `schema.rs:8-21`). Find where it reads `m.percentiles` (likely `RowAccumulator::push` or an inserter) and switch to:
```rust
let p = nyquist_core::percentiles::percentiles_from_buckets(&m.buckets, &[50.0, 90.0, 99.0, 99.9]);
```
mapping each to the fixed columns as in Step 7. Read the recorder source to locate the exact call; the schema columns are fixed, so keep `[50,90,99,99.9]`.

- [ ] **Step 9: Update `spawn_sink` to drop percentiles**

In `crates/nyquist-core/src/scheduler.rs`, replace `spawn_sink` (lines 32-54):
```rust
/// Spawn a task that calls `sink.export()` every `export_interval`,
/// passing a fresh bucket-array snapshot from `reg`. Percentile selection
/// is a consumer concern owned by each sink (design §3.4).
pub fn spawn_sink(
    mut sink: Box<dyn Sink>,
    reg: Arc<Registry>,
    export_interval: Duration,
    fault_tolerant: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(export_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let snapshot = reg.snapshot(Instant::now());
            if let Err(e) = sink.export(&snapshot).await {
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

- [ ] **Step 10: Update `main.rs` wiring**

In `src/main.rs`, update each `spawn_sink(...)` call to drop the `config.general.percentiles.clone()` argument, and pass percentiles into the sink constructors that now need them:
- ClickHouse/Recorder constructors: percentiles are baked into the fixed columns (Steps 7–8), so no constructor change unless you chose to parameterize.
- VictoriaMetrics: `VictoriaMetricsSink::new(&config.victoria_metrics.url, config.general.percentiles.clone())` (Step 6).
- Each `spawn_sink(Box::new(sink), reg.clone(), interval, config.general.fault_tolerant)` — 3 call sites (recorder, clickhouse, victoria), drop the percentiles arg.

- [ ] **Step 11: Build and test the whole workspace**

Run: `cargo test --workspace`
Expected: PASS. Fix any remaining `m.percentiles` references the compiler flags (the field no longer exists — the compiler will point to every stale site).

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "refactor: compute percentiles in sinks from bucket arrays; drop percentiles from snapshot path"
```

---

### Task A3: `linkme` SAMPLERS registration (nyquist-core + nyquist-samplers)

Replace the manual `inventory.rs` factory with a `linkme` distributed slice declared in `nyquist-core` so Plan B's BPF crate can register into the same slice (design §3.3). Each procfs sampler contributes a `SamplerEntry { name, init }`.

**Files:**
- Modify: `crates/nyquist-core/Cargo.toml` (add `linkme = "0.3"`)
- Create: `crates/nyquist-core/src/registration.rs` (`SamplerEntry`, `SAMPLERS` slice, `build_enabled` iterator)
- Modify: `crates/nyquist-core/src/lib.rs` (`pub mod registration;`)
- Modify: `crates/nyquist-samplers/Cargo.toml` (add `linkme = "0.3"`)
- Modify: `crates/nyquist-samplers/src/inventory.rs` (delete the manual factory; re-export from core; register each sampler)
- Modify: each sampler file under `crates/nyquist-samplers/src/*.rs` (add a `#[distributed_slice(SAMPLERS)]` entry)
- Modify: `src/main.rs:46-51` (call the new `build_enabled`)

**Interfaces:**
- Produces:
  - `nyquist_core::registration::SamplerEntry { pub name: &'static str, pub init: fn(&Registry, Duration) -> Box<dyn Sampler> }`
  - `#[distributed_slice] pub static SAMPLERS: [SamplerEntry]`
  - `nyquist_core::registration::build_enabled(reg, default_interval, is_enabled, interval_for) -> Vec<Box<dyn Sampler>>`
  - `nyquist_core::registration::all_sampler_names() -> Vec<&'static str>`
- Consumed by: `src/main.rs`, Plan B (each BPF sampler adds its own `#[distributed_slice(SAMPLERS)]`).

- [ ] **Step 1: Add `linkme` to nyquist-core and define the slice**

Add to `crates/nyquist-core/Cargo.toml` `[dependencies]`:
```toml
linkme = "0.3"
```
Create `crates/nyquist-core/src/registration.rs`:
```rust
use std::time::Duration;
use linkme::distributed_slice;
use crate::registry::Registry;
use crate::sampler::Sampler;

/// A registered sampler: its stable config name plus a constructor.
/// Mirrors rezolus's `SamplerEntry` (design §3.3), adapted to nyquist's
/// `Sampler` trait (`name`/`interval`/`sample`).
pub struct SamplerEntry {
    pub name: &'static str,
    pub init: fn(reg: &Registry, interval: Duration) -> Box<dyn Sampler>,
}

/// Every sampler registers into this slice via `#[distributed_slice(SAMPLERS)]`.
#[distributed_slice]
pub static SAMPLERS: [SamplerEntry] = [..];

/// Names of all registered samplers (config knows these by name).
pub fn all_sampler_names() -> Vec<&'static str> {
    SAMPLERS.iter().map(|e| e.name).collect()
}

/// Build the enabled samplers by iterating the distributed slice.
pub fn build_enabled(
    reg: &Registry,
    default_interval: Duration,
    is_enabled: impl Fn(&str) -> bool,
    interval_for: impl Fn(&str) -> Option<Duration>,
) -> Vec<Box<dyn Sampler>> {
    let mut out: Vec<Box<dyn Sampler>> = Vec::new();
    for entry in SAMPLERS {
        if !is_enabled(entry.name) { continue; }
        let iv = interval_for(entry.name).unwrap_or(default_interval);
        out.push((entry.init)(reg, iv));
    }
    out
}
```
Add `pub mod registration;` to `crates/nyquist-core/src/lib.rs`.

- [ ] **Step 2: Build core**

Run: `cargo build -p nyquist-core`
Expected: PASS (empty slice compiles).

- [ ] **Step 3: Register one sampler and prove the slice collects it (failing test)**

Add a `#[distributed_slice(SAMPLERS)]` entry to `crates/nyquist-samplers/src/cpu.rs`. First add `linkme = "0.3"` to `crates/nyquist-samplers/Cargo.toml`. Then at the bottom of `cpu.rs`:
```rust
use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};

#[distributed_slice(SAMPLERS)]
static CPU_ENTRY: SamplerEntry = SamplerEntry {
    name: "cpu",
    init: |reg, iv| Box::new(CpuSampler::new(reg, iv)),
};
```
Add a test in `crates/nyquist-samplers/src/inventory.rs` tests module:
```rust
    #[test]
    fn slice_contains_registered_samplers() {
        let names = nyquist_core::registration::all_sampler_names();
        assert!(names.contains(&"cpu"), "cpu not registered: {names:?}");
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p nyquist-samplers slice_contains_registered_samplers`
Expected: PASS. (If it fails with an empty slice, the cpu module is being dead-stripped — see Step 7's linker note; ensure `cpu` is `pub mod` and referenced.)

- [ ] **Step 5: Register the remaining samplers**

Add an analogous `#[distributed_slice(SAMPLERS)]` entry to each sampler file, matching the names and constructors from the old factory (`inventory.rs:31-46`):
- `memory.rs` → `"memory"` / `MemorySampler::new`
- `network.rs` → `"network"` / `NetworkSampler::new`
- `disk.rs` → `"disk"` / `DiskSampler::new`
- `snmp.rs` → three entries: `"ip"` / `IpSampler::new`, `"tcp"` / `TcpSampler::new`, `"udp"` / `UdpSampler::new`
- `loadavg.rs` → `"loadavg"` / `LoadAvgSampler::new`
- `psi.rs` → `"psi"` / `PsiSampler::new`
- `sockstat.rs` → `"sockstat"` / `SockstatSampler::new`
- `netstat.rs` → `"netstat"` / `NetstatSampler::new`
- `softirqs.rs` → `"softirqs"` / `SoftirqSampler::new`
- `tcpinfo.rs` → `"tcpinfo"` / `TcpInfoSampler::new`
- `nic_stats.rs` → `"nic_stats"` / `NicStatsSampler::new`

Use the exact same `use linkme::distributed_slice; use nyquist_core::registration::{SamplerEntry, SAMPLERS};` preamble in each file (or a single `pub(crate) use` in `lib.rs`).

- [ ] **Step 6: Replace the manual factory in `inventory.rs`**

Replace the whole body of `crates/nyquist-samplers/src/inventory.rs` (keep the file as a thin re-export so `main.rs` import path stays stable, or update `main.rs`). Recommended — re-export from core:
```rust
//! Sampler registration now lives in `nyquist_core::registration` via a
//! `linkme` distributed slice. The procfs samplers register themselves in
//! their own modules (`#[distributed_slice(SAMPLERS)]`). This module re-exports
//! the iterator entry points for back-compat.
pub use nyquist_core::registration::{all_sampler_names, build_enabled};

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use nyquist_core::registry::Registry;

    #[test]
    fn respects_enabled_flag() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let samplers = build_enabled(
            &reg,
            Duration::from_millis(10),
            |name| name != "network",
            |_| None,
        );
        let names: Vec<_> = samplers.iter().map(|s| s.name().to_string()).collect();
        assert!(!names.iter().any(|n| n == "network"), "network should be disabled: {names:?}");
        assert!(names.iter().any(|n| n == "cpu"), "cpu should be enabled: {names:?}");
    }
}
```
Note: the old test asserted an exact ordered Vec. Slice iteration order is link-order, not the old hardcoded order, so assert membership, not order.

- [ ] **Step 7: Ensure sampler modules are not dead-stripped (linker note)**

`linkme` entries only appear if the defining crate is linked into the final binary. `main.rs` already calls `build_enabled` (now re-exported), which references `nyquist_samplers`, keeping the crate linked, and each `pub mod` sampler is reachable. Confirm `crates/nyquist-samplers/src/lib.rs` declares every sampler as `pub mod <name>;`. If any sampler's entry is missing at runtime, add `pub use crate::<name>::*;` or keep the module `pub`. Verify with the test in Step 8.

- [ ] **Step 8: Full registration test**

Add to `inventory.rs` tests:
```rust
    #[test]
    fn all_expected_samplers_registered() {
        let mut names = nyquist_core::registration::all_sampler_names();
        names.sort_unstable();
        for expected in ["cpu","disk","ip","loadavg","memory","netstat","network",
                         "nic_stats","psi","sockstat","softirqs","tcp","tcpinfo","udp"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }
```
Run: `cargo test -p nyquist-samplers`
Expected: PASS — all 14 names present.

- [ ] **Step 9: Update `main.rs` import**

`src/main.rs:8` imports `use nyquist_samplers::inventory::build_enabled;` — this still works via the re-export. No change needed unless you removed the `inventory` module. Confirm `cargo build` passes.

- [ ] **Step 10: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "refactor(samplers): replace manual inventory factory with linkme SAMPLERS slice"
```

---

### Task A4: Golden coverage baseline + parity-gate test

Capture the canonical emitted-tuple set and assert it can only grow (design §6). The baseline fixture is captured once on a Linux agent and committed; the test reduces a snapshot to canonical tuples and asserts the baseline is a subset.

**Files:**
- Create: `crates/nyquist-core/src/coverage.rs` (canonical tuple type + reducer)
- Modify: `crates/nyquist-core/src/lib.rs` (`pub mod coverage;`)
- Create: `crates/nyquist-exposition/tests/fixtures/golden_metrics.txt` (committed Prometheus capture; placeholder seeded from fixtures, replaced by the real Linux capture)
- Create: `crates/nyquist-exposition/tests/coverage_parity.rs` (the gate)
- Create: `scripts/capture-golden-baseline.sh` (the documented Linux capture procedure)

**Interfaces:**
- Produces:
  - `nyquist_core::coverage::Tuple { name: String, label_keys: Vec<String>, kind: Kind, unit: Unit }` (`Ord`, `Eq`)
  - `nyquist_core::coverage::canonical_tuples(&RegistrySnapshot) -> std::collections::BTreeSet<Tuple>`
  - `nyquist_core::coverage::parse_prometheus_tuples(&str) -> BTreeSet<(String, Vec<String>)>` (name + sorted label keys, for diffing a captured text file)

- [ ] **Step 1: Write the canonical-tuple reducer (failing test)**

Create `crates/nyquist-core/src/coverage.rs`:
```rust
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
}
```
`Kind`/`Unit` need `PartialOrd, Ord` for `Tuple`'s derive. In `crates/nyquist-core/src/model.rs` lines 5-9, add `PartialOrd, Ord` to both enums' derives:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind { Counter, Gauge, Distribution }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unit { Count, Bytes, Seconds, Percent, None }
```
Add `pub mod coverage;` to `lib.rs`.

- [ ] **Step 2: Run to verify pass**

Run: `cargo test -p nyquist-core coverage::`
Expected: PASS.

- [ ] **Step 3: Add the Prometheus-text tuple parser (for the committed fixture)**

Append to `crates/nyquist-core/src/coverage.rs`:
```rust
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
            Some(x) => x, None => continue,
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
```
Add a unit test parsing a 3-line sample and asserting the parsed set.

- [ ] **Step 4: Write the capture script**

Create `scripts/capture-golden-baseline.sh` (executable):
```bash
#!/usr/bin/env bash
# Capture the golden coverage baseline from a live agent (run on Linux).
# Usage: scripts/capture-golden-baseline.sh > crates/nyquist-exposition/tests/fixtures/golden_metrics.txt
set -euo pipefail
ADDR="${1:-127.0.0.1:9100}"
# Assumes a nyquist agent is running and has been up > window seconds so all
# dynamic families (cpu/* per-CPU, memory/* per meminfo field, psi/*, softirq/*)
# have emitted at least once.
curl -fsS "http://${ADDR}/metrics"
```
Commit a real capture here when running on the Linux box. For now, seed `crates/nyquist-exposition/tests/fixtures/golden_metrics.txt` with a minimal hand-authored set of the statically-known base names (e.g. `cpu_usage`, `memory_MemFree`, `tcp_rtt_us`, `ebpf_tcp_rtt_us`, `ebpf_tcp_retransmits`, ...) so the gate runs on macOS. Mark the file's first line `# GOLDEN BASELINE — replace with a live Linux /metrics capture (scripts/capture-golden-baseline.sh)`.

- [ ] **Step 5: Write the parity-gate test**

Create `crates/nyquist-exposition/tests/coverage_parity.rs`:
```rust
//! Parity gate (design §6.5): the live emitted base-metric set must remain a
//! superset of the committed golden baseline. Migrations may ADD tuples; the
//! baseline set must never drop.
use nyquist_core::coverage::parse_prometheus_tuples;

const GOLDEN: &str = include_str!("fixtures/golden_metrics.txt");

#[test]
fn baseline_is_a_subset_of_itself() {
    // Sanity: the parser is deterministic and the fixture is non-empty.
    let golden = parse_prometheus_tuples(GOLDEN);
    assert!(!golden.is_empty(), "golden baseline fixture is empty");
}

/// When a live capture is available (env CAPTURE points to a /metrics dump),
/// assert the baseline is a subset of the live set. Skipped if unset so the
/// test runs on macOS without a live agent.
#[test]
fn golden_baseline_never_drops() {
    let live_path = match std::env::var("NYQUIST_LIVE_METRICS") {
        Ok(p) => p,
        Err(_) => return, // no live capture available in this environment
    };
    let live = std::fs::read_to_string(&live_path).expect("read live metrics");
    let golden = parse_prometheus_tuples(GOLDEN);
    let live = parse_prometheus_tuples(&live);
    let dropped: Vec<_> = golden.difference(&live).collect();
    assert!(dropped.is_empty(), "coverage regression — dropped tuples: {dropped:?}");
}
```

- [ ] **Step 6: Run the gate**

Run: `cargo test -p nyquist-exposition --test coverage_parity`
Expected: PASS (`golden_baseline_never_drops` returns early without `NYQUIST_LIVE_METRICS`).

- [ ] **Step 7: Commit**

```bash
git add crates/nyquist-core/src/coverage.rs crates/nyquist-core/src/model.rs \
        crates/nyquist-core/src/lib.rs crates/nyquist-exposition/tests/coverage_parity.rs \
        crates/nyquist-exposition/tests/fixtures/golden_metrics.txt \
        scripts/capture-golden-baseline.sh
chmod +x scripts/capture-golden-baseline.sh
git commit -m "test: add golden coverage baseline fixture + parity-gate (design §6)"
```

> **Linux follow-up (do on the Linux box, tracked in Plan B's environment):** run a real agent, capture `/metrics` via the script, commit it over the seed fixture, and run the gate with `NYQUIST_LIVE_METRICS=<dump>` to lock the true baseline before any procfs sampler is retired.

---

### Task A5: rust-nyquist `docs/principles.md`

Author rust-nyquist's `docs/principles.md`, derived from rezolus's (`/Users/gmckee/projects/rezolus/docs/principles.md`), annotated with met / pending / deliberate-deviation status (design §9.6). This is the review checklist for every follow-on sampler migration.

**Files:**
- Create: `docs/principles.md`

- [ ] **Step 1: Write `docs/principles.md`**

Create `docs/principles.md` with the rezolus principles adapted to rust-nyquist, each tagged `[MET]`, `[PENDING]`, or `[DEVIATION]`. Required sections (one short paragraph each), derived from rezolus principles 1–15:
1. Aggregate in the kernel — `[PENDING]` (realized for TCP in Plan B; procfs samplers still windowed).
2. mmap-direct, zero-syscall reads — `[PENDING]` (Plan B).
3. Consumers drive cadence — `[MET]` (Plan A: percentiles + reads are consumer-side; no percentile clock in collection).
4. Distributions over summaries — `[MET]` (Plan A: bucket arrays flow downstream; percentiles computed by consumers).
5. No per-event streaming for measurement — `[PENDING]` (Plan B deletes the TCP ring buffers + 1-in-128 sampling).
6. CO-RE on vanilla kernels, checked-in per-arch `vmlinux.h` — `[PENDING]` (Plan B).
7. Bounded constant work per probe; relaxed atomics — `[PENDING]` (Plan B).
8. Arrays over hashmaps; pointer-key HASH exception documented — `[PENDING]` (Plan B `tcp/packet_latency`).
9. H2 histograms, bounded relative error — `[MET]` for the windowed path (`grouping_power=7`); `[PENDING]` for in-kernel `grouping_power=3` (Plan B).
10. Tolerate benign races for monotone values — `[PENDING]` (Plan B `array_set_if_larger`).
11. Shared BPF infrastructure headers — `[PENDING]` (Plan B ports `histogram.h`/`helpers.h`).
12. Userspace overhead is part of the budget — `[MET]` (sparse bucket arrays; O(active metrics) snapshot).
13. Prefer BPF probes over parsing procfs in the hot path — `[DEVIATION, migrating]` (dual model §4; procfs retained until per-sampler parity per §6).
14. Coverage parity — never drop a metric tuple — `[MET]` (Plan A parity gate §6.5).
15. `linkme` distributed-slice registration — `[MET]` (Plan A Task A3).

Include a short "Operational checklist" subsection (a literal pass a reviewer runs over each follow-on sampler migration), and a "Known deviations" subsection naming the dual model and the retained-procfs families from design §6.4.

- [ ] **Step 2: Commit**

```bash
git add docs/principles.md
git commit -m "docs: add rust-nyquist principles.md derived from rezolus, with status tags"
```

---

### Task A6: Expand the §6.4 coverage map with dynamic families

Expand the design's coverage table into a committed migration contract that enumerates **every** current metric family — including the dynamically generated ones a source grep can't see (design §6.1, §6.4).

**Files:**
- Create: `docs/coverage-map.md`

- [ ] **Step 1: Enumerate the dynamic families**

For each sampler, list the concrete emitted families. Derive them from the sampler source (read each file) — especially the dynamic ones:
- `cpu` (`/proc/stat`): `cpu/usage/{user,nice,system,idle,iowait,irq,softirq}` **per CPU** (label `cpu=N`).
- `memory` (`/proc/meminfo`): `memory/<Field>` **per meminfo field** (MemTotal, MemFree, Buffers, Cached, … — enumerate from a real `/proc/meminfo`).
- `psi`: `psi/{cpu,memory,io}/{some,full}_avg{10,60,300}` (full absent for cpu).
- `softirqs`: `softirq/<TYPE>` **per softirq type** (HI, TIMER, NET_TX, NET_RX, BLOCK, IRQ_POLL, TASKLET, SCHED, HRTIMER, RCU).
- `network` (`/proc/net/dev`): `network/{receive,transmit}/{bytes,errors,dropped}` **per interface**.
- `disk`: `disk/{read,write}/bytes` **per device**.
- `nic_stats`: `nic/*`, `nic/queue/*` **per interface, ethtool-discovered**.
- `snmp`/`netstat`/`sockstat`/`loadavg`: as in design §6.4.
- eBPF (Plan B target): `ebpf/tcp/rtt_us` (→ `tcp/packet_latency`), `ebpf/tcp/retransmits` (→ `tcp/retransmit`).

- [ ] **Step 2: Write `docs/coverage-map.md`**

Reproduce the design §6.4 table, expanded: one row per family with columns `Family | Dynamic? | Representative tuples | Current source | Target source | Status`. Mark TCP rows `In progress (Plan B)`; all others `Retained (procfs/perf)`. Add a header note that this file is the migration contract referenced by every follow-on spec, and that a row is realized (procfs retired) only on verified tuple parity (§6.3).

- [ ] **Step 3: Commit**

```bash
git add docs/coverage-map.md
git commit -m "docs: expand coverage map with dynamic metric families (design §6.4)"
```

---

## Self-Review Notes (for the implementer)

- **Spec coverage:** Plan A covers design §9 deliverables 2 (linkme), 3 (bucket-array snapshot + sink percentile helper + the snapshot seam), 5 (golden baseline + parity gate), 6 (principles.md), 7 (coverage map). Deliverables 1 (libbpf toolchain) and 4 (TCP slice) are Plan B.
- **The dual-model seam (design §4):** Plan A establishes the unified snapshot interface — `RegistrySnapshot` of `(raw, buckets)` per metric, percentiles computed by consumers. Plan B plugs BPF metrics into the *same* `RegistrySnapshot` via a new direct-bucket Registry path, so the snapshot interface does not change as samplers migrate.
- **Type consistency:** `buckets: Vec<(u64,u64)>` (upper_bound, count) is used identically in `snapshot.rs`, `sliding.rs::bucket_counts`, `percentiles_from_buckets`, and every sink. `Registry::snapshot(now)` (no percentiles) is the single signature used by `spawn_sink` and `HttpServer`.
- **Known risk:** the `histogram` 0.11 bucket-iterator method names (Task A1 Step 1). Localized to one call site.
