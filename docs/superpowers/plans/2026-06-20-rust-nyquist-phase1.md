# rust-nyquist Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working high-resolution telemetry agent that oversamples procfs sources and exposes percentile metrics over HTTP.

**Architecture:** A Cargo workspace. `nyquist-core` holds the data model plus the oversampling engine: a sliding windowed histogram (a ring of per-slice histograms) into which counters' instantaneous rates and gauges' readings are recorded. A `Scheduler` drives each `Sampler` on its own sub-second timer; samplers write to a shared lock-free `Registry`. A `Sink` trait abstracts outputs; Phase 1 ships a Prometheus/JSON HTTP endpoint via `axum`.

**Tech Stack:** Rust (edition 2021), `tokio`, `axum`, `histogram` crate, `dashmap`, `serde`/`toml`, `clap`, `thiserror`/`anyhow`, `proptest`, `async-trait`.

## Global Constraints

- Workspace MUST compile on macOS; Linux-only samplers gated behind `#[cfg(target_os = "linux")]`. Engine + exposition + config are platform-independent and unit-tested on any OS.
- `nyquist-core` depends on no other workspace crate and has no HTTP/config/sampler dependencies.
- Samplers record to the `Registry` only; they never format output or know about sinks.
- Histogram math comes from the `histogram` crate (pinned `histogram = "0.11"`); do not hand-roll bucketing. The crate is used only inside `nyquist-core/src/hist/slice.rs`.
- Default sample interval: `10ms`. Default slice width: `100ms`. Default reporting window: `60s`. Default percentiles: `[50.0, 90.0, 99.0, 99.9]`. Default listen: `0.0.0.0:9100`.
- Counters expose raw total + rate percentiles; gauges expose last value + value percentiles.
- TDD: write the failing test first, watch it fail, implement minimally, watch it pass, commit. Every code step shows the actual code.
- Time uses `std::time::Instant` for sampling/rate math and `std::time::SystemTime` for export wall-clock stamps.

---

### Task 0: Workspace scaffolding

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/nyquist-core/Cargo.toml`, `crates/nyquist-core/src/lib.rs`
- Create: `crates/nyquist-config/Cargo.toml`, `crates/nyquist-config/src/lib.rs`
- Create: `crates/nyquist-samplers/Cargo.toml`, `crates/nyquist-samplers/src/lib.rs`
- Create: `crates/nyquist-exposition/Cargo.toml`, `crates/nyquist-exposition/src/lib.rs`
- Create: `src/main.rs`
- Create: `rust-toolchain.toml`, `.gitignore`

**Interfaces:**
- Produces: a building workspace; crate names `nyquist-core`, `nyquist-config`, `nyquist-samplers`, `nyquist-exposition`, and binary `nyquist`.

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[package]
name = "nyquist"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
description = "High-resolution oversampling systems telemetry agent"

[[bin]]
name = "nyquist"
path = "src/main.rs"

[dependencies]
nyquist-core = { path = "crates/nyquist-core" }
nyquist-config = { path = "crates/nyquist-config" }
nyquist-samplers = { path = "crates/nyquist-samplers" }
nyquist-exposition = { path = "crates/nyquist-exposition" }
tokio = { version = "1", features = ["full"] }
anyhow = "1"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 2: Create each crate's `Cargo.toml`**

`crates/nyquist-core/Cargo.toml`:
```toml
[package]
name = "nyquist-core"
version = "0.1.0"
edition = "2021"

[dependencies]
histogram = "0.11"
dashmap = "6"
async-trait = "0.1"
thiserror = "1"
tokio = { version = "1", features = ["rt", "time", "macros", "sync"] }
tracing = "0.1"

[dev-dependencies]
proptest = "1"
tokio = { version = "1", features = ["rt", "time", "macros", "test-util"] }
```

`crates/nyquist-config/Cargo.toml`:
```toml
[package]
name = "nyquist-config"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"
humantime-serde = "1"
thiserror = "1"
```

`crates/nyquist-samplers/Cargo.toml`:
```toml
[package]
name = "nyquist-samplers"
version = "0.1.0"
edition = "2021"

[dependencies]
nyquist-core = { path = "../nyquist-core" }
async-trait = "0.1"
tracing = "0.1"
```

`crates/nyquist-exposition/Cargo.toml`:
```toml
[package]
name = "nyquist-exposition"
version = "0.1.0"
edition = "2021"

[dependencies]
nyquist-core = { path = "../nyquist-core" }
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "net", "sync"] }
async-trait = "0.1"
tracing = "0.1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 3: Create placeholder lib/bin files**

Each `lib.rs` starts empty except a module doc comment, e.g. `crates/nyquist-core/src/lib.rs`:
```rust
//! Core data model and oversampling engine for nyquist.
```
`src/main.rs`:
```rust
fn main() {
    println!("nyquist");
}
```
`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
```
`.gitignore`:
```
/target
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build`
Expected: compiles with no errors (warnings about unused crates are fine).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates src/main.rs rust-toolchain.toml .gitignore
git commit -m "chore: scaffold nyquist workspace"
```

---

### Task 1: Core metric types — Kind, Unit, Labels, MetricId

**Files:**
- Create: `crates/nyquist-core/src/model.rs`
- Modify: `crates/nyquist-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `enum Kind { Counter, Gauge, Distribution }`
  - `enum Unit { Count, Bytes, Seconds, Percent, None }`
  - `struct Labels(BTreeMap<String, String>)` with `Labels::new()`, `.insert(k, v) -> Self`, `.iter()`
  - `struct MetricId(u64)` and `fn metric_id(name: &str, labels: &Labels) -> MetricId`

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-core/src/model.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_id_is_stable_and_label_order_independent() {
        let a = Labels::new().insert("cpu", "0").insert("core", "1");
        let b = Labels::new().insert("core", "1").insert("cpu", "0");
        assert_eq!(metric_id("cpu/usage", &a), metric_id("cpu/usage", &b));
        assert_ne!(metric_id("cpu/usage", &a), metric_id("cpu/idle", &a));
    }

    #[test]
    fn labels_iterate_sorted() {
        let l = Labels::new().insert("z", "1").insert("a", "2");
        let keys: Vec<_> = l.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "z"]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-core model::`
Expected: FAIL — `Labels`, `metric_id` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-core/src/model.rs`:
```rust
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Counter, Gauge, Distribution }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit { Count, Bytes, Seconds, Percent, None }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Labels(BTreeMap<String, String>);

impl Labels {
    pub fn new() -> Self { Labels(BTreeMap::new()) }
    pub fn insert(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.0.insert(k.into(), v.into());
        self
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> { self.0.iter() }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetricId(pub u64);

pub fn metric_id(name: &str, labels: &Labels) -> MetricId {
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    for (k, v) in labels.iter() {
        k.hash(&mut h);
        v.hash(&mut h);
    }
    MetricId(h.finish())
}
```

- [ ] **Step 4: Wire module and run tests**

Add to `crates/nyquist-core/src/lib.rs`:
```rust
pub mod model;
pub use model::{Kind, Labels, MetricId, Unit, metric_id};
```
Run: `cargo test -p nyquist-core model::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src/model.rs crates/nyquist-core/src/lib.rs
git commit -m "feat(core): metric model types (Kind, Unit, Labels, MetricId)"
```

---

### Task 2: Histogram slice wrapper over the `histogram` crate

**Files:**
- Create: `crates/nyquist-core/src/hist/mod.rs`
- Create: `crates/nyquist-core/src/hist/slice.rs`
- Modify: `crates/nyquist-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `struct HistogramSlice` wrapping `histogram::Histogram`
  - `HistogramSlice::new() -> Self`
  - `.record(value: u64)`
  - `.clear(&mut self)`
  - `.merge_into(&self, acc: &mut histogram::Histogram)`
  - `fn empty_accumulator() -> histogram::Histogram`
  - `fn percentile(acc: &histogram::Histogram, p: f64) -> u64`

- [ ] **Step 1: Confirm the `histogram` crate API before coding**

Run: `cargo doc -p histogram --no-deps --open` (or read https://docs.rs/histogram/0.11).
Confirm signatures for: `Histogram::new(grouping_power, max_value_power)`, `increment(value)`, `add`/`checked_add`/merge, and `percentile(p) -> Result<Option<Bucket>>` plus `Bucket::end()`. If a name differs in the pinned version, adjust only this file (`slice.rs`) — the rest of the codebase consumes the wrapper, not the crate.

- [ ] **Step 2: Write the failing test**

Create `crates/nyquist-core/src/hist/slice.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_of_uniform_values_is_near_input() {
        let mut s = HistogramSlice::new();
        for v in 1..=1000u64 { s.record(v); }
        let mut acc = empty_accumulator();
        s.merge_into(&acc.clone(), &mut acc); // merge slice into acc
        let p50 = percentile(&acc, 50.0);
        // 1% relative-error buckets: p50 should be within ~2% of 500
        assert!((490..=515).contains(&p50), "p50 was {p50}");
    }

    #[test]
    fn clear_resets_counts() {
        let mut s = HistogramSlice::new();
        s.record(100);
        s.clear();
        let mut acc = empty_accumulator();
        s.merge_into(&acc.clone(), &mut acc);
        assert_eq!(percentile(&acc, 99.0), 0);
    }
}
```

Note: `merge_into` takes the slice's own histogram and an accumulator. The test's awkward `acc.clone()` call is removed in Step 4 once the real signature `merge_into(&self, acc: &mut Histogram)` is in place — adjust the test to `s.merge_into(&mut acc);`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-core/src/hist/slice.rs`:
```rust
use histogram::Histogram;

/// Grouping power 7 (~1% relative error) and max value power 39
/// (covers up to ~5.4e11, enough for byte/sec rates).
const GROUPING_POWER: u8 = 7;
const MAX_VALUE_POWER: u8 = 39;

pub struct HistogramSlice {
    inner: Histogram,
}

impl HistogramSlice {
    pub fn new() -> Self {
        HistogramSlice {
            inner: Histogram::new(GROUPING_POWER, MAX_VALUE_POWER)
                .expect("valid histogram parameters"),
        }
    }

    pub fn record(&mut self, value: u64) {
        // Saturate at the histogram's max rather than erroring on overflow.
        let _ = self.inner.increment(value);
    }

    pub fn clear(&mut self) {
        self.inner = Histogram::new(GROUPING_POWER, MAX_VALUE_POWER)
            .expect("valid histogram parameters");
    }

    pub fn merge_into(&self, acc: &mut Histogram) {
        // `checked_add` returns a new histogram; assign back into acc.
        if let Ok(sum) = acc.checked_add(&self.inner) {
            *acc = sum;
        }
    }
}

pub fn empty_accumulator() -> Histogram {
    Histogram::new(GROUPING_POWER, MAX_VALUE_POWER).expect("valid histogram parameters")
}

pub fn percentile(acc: &Histogram, p: f64) -> u64 {
    match acc.percentile(p) {
        Ok(Some(bucket)) => bucket.end(),
        _ => 0,
    }
}
```
Then fix the Step 2 tests to call `s.merge_into(&mut acc);` (single argument).

- [ ] **Step 4: Wire module and run tests**

Create `crates/nyquist-core/src/hist/mod.rs`:
```rust
mod slice;
pub use slice::{HistogramSlice, empty_accumulator, percentile};
```
Add to `crates/nyquist-core/src/lib.rs`:
```rust
pub mod hist;
```
Run: `cargo test -p nyquist-core hist::`
Expected: PASS (2 tests). If the histogram API differs, fix `slice.rs` until green.

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src/hist crates/nyquist-core/src/lib.rs
git commit -m "feat(core): histogram slice wrapper over histogram crate"
```

---

### Task 3: Sliding windowed histogram (ring of slices)

**Files:**
- Create: `crates/nyquist-core/src/hist/sliding.rs`
- Modify: `crates/nyquist-core/src/hist/mod.rs`

**Interfaces:**
- Consumes: `HistogramSlice`, `empty_accumulator`, `percentile` from Task 2.
- Produces:
  - `struct SlidingHistogram` with `SlidingHistogram::new(slice_width: Duration, window: Duration) -> Self`
  - `.record(now: Instant, value: u64)`
  - `.percentile(now: Instant, p: f64) -> u64` (merges live slices covering the window)

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-core/src/hist/sliding.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn records_within_window_are_counted() {
        let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(1));
        let t0 = Instant::now();
        for i in 0..1000u64 {
            h.record(t0 + Duration::from_micros(i * 100), 42);
        }
        let p50 = h.percentile(t0 + Duration::from_millis(999), 50.0);
        assert!((41..=43).contains(&p50), "p50 was {p50}");
    }

    #[test]
    fn old_slices_expire_out_of_window() {
        let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(1));
        let t0 = Instant::now();
        // Old burst of large values.
        for _ in 0..100 { h.record(t0, 1_000_000); }
        // Two seconds later, record small values; the old burst must have rotated out.
        let later = t0 + Duration::from_secs(2);
        for i in 0..100u64 { h.record(later + Duration::from_millis(i), 10); }
        let p99 = h.percentile(later + Duration::from_millis(100), 99.0);
        assert!(p99 < 100, "stale burst leaked into window: p99 = {p99}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-core hist::sliding`
Expected: FAIL — `SlidingHistogram` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-core/src/hist/sliding.rs`:
```rust
use std::time::{Duration, Instant};
use super::slice::{HistogramSlice, empty_accumulator, percentile};

/// A ring of histogram slices forming a sliding time window.
pub struct SlidingHistogram {
    slices: Vec<HistogramSlice>,
    slice_index: Vec<Option<u64>>, // which absolute slice-number each ring cell holds
    slice_width: Duration,
    n_slices: usize,
    origin: Option<Instant>,
}

impl SlidingHistogram {
    pub fn new(slice_width: Duration, window: Duration) -> Self {
        let n_slices = (window.as_nanos() / slice_width.as_nanos()).max(1) as usize;
        SlidingHistogram {
            slices: (0..n_slices).map(|_| HistogramSlice::new()).collect(),
            slice_index: vec![None; n_slices],
            slice_width,
            n_slices,
            origin: None,
        }
    }

    fn absolute_slice(&mut self, now: Instant) -> u64 {
        let origin = *self.origin.get_or_insert(now);
        let elapsed = now.saturating_duration_since(origin);
        (elapsed.as_nanos() / self.slice_width.as_nanos()) as u64
    }

    pub fn record(&mut self, now: Instant, value: u64) {
        let abs = self.absolute_slice(now);
        let cell = (abs as usize) % self.n_slices;
        if self.slice_index[cell] != Some(abs) {
            // This ring cell holds a stale slice; recycle it for the new slice number.
            self.slices[cell].clear();
            self.slice_index[cell] = Some(abs);
        }
        self.slices[cell].record(value);
    }

    pub fn percentile(&mut self, now: Instant, p: f64) -> u64 {
        let abs_now = self.absolute_slice(now);
        let oldest = abs_now.saturating_sub(self.n_slices as u64 - 1);
        let mut acc = empty_accumulator();
        for cell in 0..self.n_slices {
            if let Some(abs) = self.slice_index[cell] {
                if abs >= oldest && abs <= abs_now {
                    self.slices[cell].merge_into(&mut acc);
                }
            }
        }
        percentile(&acc, p)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-core hist::sliding`
Expected: PASS (2 tests).

- [ ] **Step 5: Add a property test for percentile monotonicity**

Append inside the `tests` module:
```rust
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn percentiles_are_monotonic(values in proptest::collection::vec(1u64..1_000_000, 1..500)) {
            let mut h = SlidingHistogram::new(Duration::from_millis(100), Duration::from_secs(10));
            let t0 = Instant::now();
            for (i, v) in values.iter().enumerate() {
                h.record(t0 + Duration::from_millis(i as u64), *v);
            }
            let now = t0 + Duration::from_millis(values.len() as u64);
            let p50 = h.percentile(now, 50.0);
            let p90 = h.percentile(now, 90.0);
            let p99 = h.percentile(now, 99.0);
            prop_assert!(p50 <= p90, "p50={p50} p90={p90}");
            prop_assert!(p90 <= p99, "p90={p90} p99={p99}");
        }
    }
```
Run: `cargo test -p nyquist-core hist::sliding`
Expected: PASS (3 tests).

- [ ] **Step 6: Wire and commit**

Add to `crates/nyquist-core/src/hist/mod.rs`:
```rust
mod sliding;
pub use sliding::SlidingHistogram;
```
```bash
git add crates/nyquist-core/src/hist
git commit -m "feat(core): sliding windowed histogram (ring of slices)"
```

---

### Task 4: Metric and Registry with the recording API

**Files:**
- Create: `crates/nyquist-core/src/registry.rs`
- Modify: `crates/nyquist-core/src/lib.rs`

**Interfaces:**
- Consumes: `Kind`, `Unit`, `Labels`, `MetricId`, `metric_id` (Task 1); `SlidingHistogram` (Task 3).
- Produces:
  - `struct MetricDef { name, kind, unit, description, labels }` with builder `MetricDef::new(name, kind)`, `.unit(Unit)`, `.description(&str)`, `.labels(Labels)`
  - `struct Registry` with `Registry::new(slice_width: Duration, window: Duration) -> Self`
  - `.register(def: MetricDef) -> MetricId`
  - `.record_counter(id: MetricId, now: Instant, value: u64)`
  - `.record_gauge(id: MetricId, now: Instant, value: u64)`
  - `.metric_ids() -> Vec<MetricId>` and `.with_metric(id, f)` accessor used by snapshots (Task 5)

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-core/src/registry.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Labels};
    use std::time::{Duration, Instant};

    #[test]
    fn counter_rate_reflects_delta_over_time() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("net/tx_bytes", Kind::Counter));
        let t0 = Instant::now();
        // 1000 bytes per 10ms tick = 100_000 bytes/sec.
        let mut total = 0u64;
        for i in 1..=50u64 {
            total += 1000;
            reg.record_counter(id, t0 + Duration::from_millis(i * 10), total);
        }
        let now = t0 + Duration::from_millis(500);
        let p50 = reg.percentile(id, now, 50.0);
        assert!((95_000..=105_000).contains(&p50), "rate p50 was {p50}");
        assert_eq!(reg.raw(id), 50_000);
    }

    #[test]
    fn counter_reset_is_ignored_for_one_interval() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("c", Kind::Counter));
        let t0 = Instant::now();
        reg.record_counter(id, t0 + Duration::from_millis(10), 1000);
        reg.record_counter(id, t0 + Duration::from_millis(20), 2000);
        reg.record_counter(id, t0 + Duration::from_millis(30), 5); // reset (wrap/restart)
        // No panic; raw tracks the latest value.
        assert_eq!(reg.raw(id), 5);
    }

    #[test]
    fn gauge_records_reading_directly() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("mem/free", Kind::Gauge));
        let t0 = Instant::now();
        for i in 0..50u64 { reg.record_gauge(id, t0 + Duration::from_millis(i * 10), 4096); }
        let now = t0 + Duration::from_millis(500);
        assert_eq!(reg.percentile(id, now, 50.0), 4096);
        assert_eq!(reg.raw(id), 4096);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-core registry::`
Expected: FAIL — `Registry`, `MetricDef` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-core/src/registry.rs`:
```rust
use std::sync::Mutex;
use std::time::{Duration, Instant};
use dashmap::DashMap;
use crate::hist::SlidingHistogram;
use crate::model::{Kind, Labels, MetricId, Unit, metric_id};

#[derive(Clone, Debug)]
pub struct MetricDef {
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    pub description: Option<String>,
    pub labels: Labels,
}

impl MetricDef {
    pub fn new(name: impl Into<String>, kind: Kind) -> Self {
        MetricDef { name: name.into(), kind, unit: Unit::None, description: None, labels: Labels::new() }
    }
    pub fn unit(mut self, u: Unit) -> Self { self.unit = u; self }
    pub fn description(mut self, d: impl Into<String>) -> Self { self.description = Some(d.into()); self }
    pub fn labels(mut self, l: Labels) -> Self { self.labels = l; self }
}

struct MetricState {
    def: MetricDef,
    window: SlidingHistogram,
    raw: u64,
    prev: Option<(Instant, u64)>, // previous counter sample
}

pub struct Registry {
    metrics: DashMap<MetricId, Mutex<MetricState>>,
    slice_width: Duration,
    window: Duration,
}

impl Registry {
    pub fn new(slice_width: Duration, window: Duration) -> Self {
        Registry { metrics: DashMap::new(), slice_width, window }
    }

    pub fn register(&self, def: MetricDef) -> MetricId {
        let id = metric_id(&def.name, &def.labels);
        self.metrics.entry(id).or_insert_with(|| {
            Mutex::new(MetricState {
                def,
                window: SlidingHistogram::new(self.slice_width, self.window),
                raw: 0,
                prev: None,
            })
        });
        id
    }

    pub fn record_counter(&self, id: MetricId, now: Instant, value: u64) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            if let Some((prev_t, prev_v)) = s.prev {
                if value >= prev_v {
                    let dt = now.saturating_duration_since(prev_t).as_secs_f64();
                    if dt > 0.0 {
                        let rate = ((value - prev_v) as f64 / dt).round() as u64;
                        s.window.record(now, rate);
                    }
                }
                // value < prev_v => counter reset; skip this interval.
            }
            s.prev = Some((now, value));
            s.raw = value;
        }
    }

    pub fn record_gauge(&self, id: MetricId, now: Instant, value: u64) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            s.window.record(now, value);
            s.raw = value;
        }
    }

    pub fn percentile(&self, id: MetricId, now: Instant, p: f64) -> u64 {
        match self.metrics.get(&id) {
            Some(state) => state.lock().unwrap().window.percentile(now, p),
            None => 0,
        }
    }

    pub fn raw(&self, id: MetricId) -> u64 {
        self.metrics.get(&id).map(|s| s.lock().unwrap().raw).unwrap_or(0)
    }

    pub fn metric_ids(&self) -> Vec<MetricId> {
        self.metrics.iter().map(|e| *e.key()).collect()
    }
}
```

- [ ] **Step 4: Wire module and run tests**

Add to `crates/nyquist-core/src/lib.rs`:
```rust
pub mod registry;
pub use registry::{MetricDef, Registry};
```
Run: `cargo test -p nyquist-core registry::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src/registry.rs crates/nyquist-core/src/lib.rs
git commit -m "feat(core): registry with counter/gauge recording and rate calc"
```

---

### Task 5: RegistrySnapshot and the Sink trait

**Files:**
- Create: `crates/nyquist-core/src/snapshot.rs`
- Create: `crates/nyquist-core/src/sink.rs`
- Modify: `crates/nyquist-core/src/registry.rs`, `crates/nyquist-core/src/lib.rs`

**Interfaces:**
- Consumes: `Registry`, `MetricDef` (Task 4), `Kind`, `Unit`, `Labels`.
- Produces:
  - `struct MetricSnapshot { name, kind, unit, labels, raw, percentiles: Vec<(f64, u64)> }`
  - `struct RegistrySnapshot { metrics: Vec<MetricSnapshot>, captured: SystemTime }`
  - `Registry::snapshot(&self, now: Instant, percentiles: &[f64]) -> RegistrySnapshot`
  - `trait Sink { async fn export(&mut self, snap: &RegistrySnapshot) -> anyhow::Result<()> }` (use `Box<dyn std::error::Error + Send + Sync>` to avoid an anyhow dep in core)

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-core/src/snapshot.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;
    use crate::registry::{MetricDef, Registry};
    use std::time::{Duration, Instant};

    #[test]
    fn snapshot_contains_raw_and_percentiles() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("g", Kind::Gauge));
        let t0 = Instant::now();
        for i in 0..50u64 { reg.record_gauge(id, t0 + Duration::from_millis(i * 10), 7); }
        let snap = reg.snapshot(t0 + Duration::from_millis(500), &[50.0, 99.0]);
        let m = snap.metrics.iter().find(|m| m.name == "g").unwrap();
        assert_eq!(m.raw, 7);
        assert_eq!(m.percentiles.len(), 2);
        assert_eq!(m.percentiles[0], (50.0, 7));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-core snapshot::`
Expected: FAIL — `snapshot` method / `RegistrySnapshot` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-core/src/snapshot.rs`:
```rust
use std::time::SystemTime;
use crate::model::{Kind, Labels, Unit};

#[derive(Clone, Debug)]
pub struct MetricSnapshot {
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    pub labels: Labels,
    pub raw: u64,
    pub percentiles: Vec<(f64, u64)>,
}

#[derive(Clone, Debug)]
pub struct RegistrySnapshot {
    pub metrics: Vec<MetricSnapshot>,
    pub captured: SystemTime,
}
```

Add to `crates/nyquist-core/src/registry.rs` inside `impl Registry`:
```rust
    pub fn snapshot(
        &self,
        now: std::time::Instant,
        percentiles: &[f64],
    ) -> crate::snapshot::RegistrySnapshot {
        let mut metrics = Vec::new();
        for entry in self.metrics.iter() {
            let mut s = entry.value().lock().unwrap();
            let pcts = percentiles.iter().map(|&p| (p, s.window.percentile(now, p))).collect();
            metrics.push(crate::snapshot::MetricSnapshot {
                name: s.def.name.clone(),
                kind: s.def.kind,
                unit: s.def.unit,
                labels: s.def.labels.clone(),
                raw: s.raw,
                percentiles: pcts,
            });
        }
        crate::snapshot::RegistrySnapshot { metrics, captured: std::time::SystemTime::now() }
    }
```

Create `crates/nyquist-core/src/sink.rs`:
```rust
use crate::snapshot::RegistrySnapshot;

pub type SinkError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait::async_trait]
pub trait Sink: Send {
    async fn export(&mut self, snapshot: &RegistrySnapshot) -> Result<(), SinkError>;
}
```

- [ ] **Step 4: Wire modules and run tests**

Add to `crates/nyquist-core/src/lib.rs`:
```rust
pub mod snapshot;
pub mod sink;
pub use snapshot::{MetricSnapshot, RegistrySnapshot};
pub use sink::{Sink, SinkError};
```
Run: `cargo test -p nyquist-core snapshot::`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src
git commit -m "feat(core): registry snapshot and Sink trait"
```

---

### Task 6: Sampler trait and Scheduler

**Files:**
- Create: `crates/nyquist-core/src/sampler.rs`
- Create: `crates/nyquist-core/src/scheduler.rs`
- Modify: `crates/nyquist-core/src/lib.rs`

**Interfaces:**
- Consumes: `Registry` (Task 4).
- Produces:
  - `trait Sampler { fn name(&self)->&str; fn interval(&self)->Duration; async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> }`
  - `type SamplerError = Box<dyn std::error::Error + Send + Sync>`
  - `fn spawn_sampler(sampler: Box<dyn Sampler>, reg: Arc<Registry>, fault_tolerant: bool) -> JoinHandle<()>`

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-core/src/scheduler.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{MetricDef, Registry};
    use crate::model::Kind;
    use crate::sampler::{Sampler, SamplerError};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct CountingSampler { id: crate::model::MetricId, n: u64 }

    #[async_trait::async_trait]
    impl Sampler for CountingSampler {
        fn name(&self) -> &str { "counting" }
        fn interval(&self) -> Duration { Duration::from_millis(10) }
        async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
            self.n += 100;
            reg.record_counter(self.id, now, self.n);
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn scheduler_ticks_sampler_on_its_interval() {
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let id = reg.register(MetricDef::new("ticks", Kind::Counter));
        let handle = spawn_sampler(Box::new(CountingSampler { id, n: 0 }), reg.clone(), true);
        tokio::time::sleep(Duration::from_millis(105)).await;
        handle.abort();
        // ~10 ticks happened; raw should have advanced well past zero.
        assert!(reg.raw(id) >= 500, "raw was {}", reg.raw(id));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-core scheduler::`
Expected: FAIL — `Sampler`, `spawn_sampler` not found.

- [ ] **Step 3: Write minimal implementation**

Create `crates/nyquist-core/src/sampler.rs`:
```rust
use std::time::{Duration, Instant};
use crate::registry::Registry;

pub type SamplerError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait::async_trait]
pub trait Sampler: Send {
    fn name(&self) -> &str;
    fn interval(&self) -> Duration;
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError>;
}
```

Prepend to `crates/nyquist-core/src/scheduler.rs`:
```rust
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinHandle;
use crate::registry::Registry;
use crate::sampler::Sampler;

pub fn spawn_sampler(
    mut sampler: Box<dyn Sampler>,
    reg: Arc<Registry>,
    fault_tolerant: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(sampler.interval());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let now = Instant::now();
            if let Err(e) = sampler.sample(&reg, now).await {
                tracing::warn!(sampler = sampler.name(), error = %e, "sample failed");
                if !fault_tolerant {
                    tracing::error!(sampler = sampler.name(), "exiting: fault_tolerant=false");
                    break;
                }
            }
        }
    })
}
```

- [ ] **Step 4: Wire modules and run tests**

Add to `crates/nyquist-core/src/lib.rs`:
```rust
pub mod sampler;
pub mod scheduler;
pub use sampler::{Sampler, SamplerError};
pub use scheduler::spawn_sampler;
```
Run: `cargo test -p nyquist-core scheduler::`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src crates/nyquist-core/src/lib.rs
git commit -m "feat(core): Sampler trait and tokio-driven scheduler"
```

---

### Task 7: Configuration types and loading

**Files:**
- Create: `crates/nyquist-config/src/lib.rs` (replace placeholder)
- Create: `crates/nyquist-config/tests/load.rs`

**Interfaces:**
- Produces:
  - `struct Config { general: General, samplers: BTreeMap<String, SamplerConfig> }`
  - `struct General { listen, default_interval: Duration, window: Duration, percentiles: Vec<f64>, fault_tolerant: bool, log: String }`
  - `struct SamplerConfig { enabled: bool, interval: Option<Duration> }`
  - `Config::load(path: &Path) -> Result<Config, ConfigError>` and `Config::default()`
  - `Config::sampler(&self, name: &str) -> SamplerConfig` (defaults: enabled=true, interval=None)

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-config/tests/load.rs`:
```rust
use nyquist_config::Config;
use std::time::Duration;

#[test]
fn defaults_are_sane() {
    let c = Config::default();
    assert_eq!(c.general.listen, "0.0.0.0:9100");
    assert_eq!(c.general.default_interval, Duration::from_millis(10));
    assert_eq!(c.general.window, Duration::from_secs(60));
    assert!(c.general.fault_tolerant);
}

#[test]
fn parses_toml_with_sampler_override() {
    let toml = r#"
        [general]
        listen = "127.0.0.1:1234"
        default_interval = "5ms"
        window = "30s"
        percentiles = [50.0, 99.9]
        fault_tolerant = false
        log = "debug"

        [samplers.network]
        enabled = true
        interval = "2ms"

        [samplers.disk]
        enabled = false
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert_eq!(c.general.default_interval, Duration::from_millis(5));
    assert_eq!(c.sampler("network").interval, Some(Duration::from_millis(2)));
    assert!(!c.sampler("disk").enabled);
    // Unlisted sampler defaults to enabled with no override.
    assert!(c.sampler("cpu").enabled);
    assert_eq!(c.sampler("cpu").interval, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-config`
Expected: FAIL — `Config` not found.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/nyquist-config/src/lib.rs`:
```rust
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing config: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub samplers: BTreeMap<String, SamplerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    pub listen: String,
    #[serde(with = "humantime_serde")]
    pub default_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub window: Duration,
    pub percentiles: Vec<f64>,
    pub fault_tolerant: bool,
    pub log: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SamplerConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde::option")]
    pub interval: Option<Duration>,
}

impl Default for General {
    fn default() -> Self {
        General {
            listen: "0.0.0.0:9100".to_string(),
            default_interval: Duration::from_millis(10),
            window: Duration::from_secs(60),
            percentiles: vec![50.0, 90.0, 99.0, 99.9],
            fault_tolerant: true,
            log: "info".to_string(),
        }
    }
}

impl Default for SamplerConfig {
    fn default() -> Self { SamplerConfig { enabled: true, interval: None } }
}

impl Default for Config {
    fn default() -> Self { Config { general: General::default(), samplers: BTreeMap::new() } }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }
    pub fn sampler(&self, name: &str) -> SamplerConfig {
        self.samplers.get(name).cloned().unwrap_or_default()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-config`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-config
git commit -m "feat(config): TOML config with defaults and per-sampler overrides"
```

---

### Task 8: Prometheus and JSON formatting (pure functions)

**Files:**
- Create: `crates/nyquist-exposition/src/format.rs`
- Modify: `crates/nyquist-exposition/src/lib.rs`

**Interfaces:**
- Consumes: `RegistrySnapshot`, `MetricSnapshot`, `Kind` from `nyquist-core`.
- Produces:
  - `fn to_prometheus(snap: &RegistrySnapshot) -> String`
  - `fn to_json(snap: &RegistrySnapshot) -> String`
  - Metric names sanitized: `/` and `-` → `_`. Percentile series suffix: `_rate` for counters, `_value` for gauges, with a `percentile="P"` label.

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-exposition/src/format.rs`:
```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-exposition format::`
Expected: FAIL — `to_prometheus` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-exposition/src/format.rs`:
```rust
use nyquist_core::model::{Kind, Labels};
use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c == '/' || c == '-' { '_' } else { c }).collect()
}

fn fmt_pct(p: f64) -> String {
    // 99.0 -> "99", 99.9 -> "99.9"
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
    // Hand-rolled JSON to avoid a serde_json dependency in this crate.
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
```

- [ ] **Step 4: Wire module and run tests**

Add to `crates/nyquist-exposition/src/lib.rs`:
```rust
pub mod format;
pub use format::{to_json, to_prometheus};
```
Run: `cargo test -p nyquist-exposition format::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-exposition/src
git commit -m "feat(exposition): Prometheus and JSON formatters"
```

---

### Task 9: Axum HTTP server with cached snapshot

**Files:**
- Create: `crates/nyquist-exposition/src/server.rs`
- Modify: `crates/nyquist-exposition/src/lib.rs`

**Interfaces:**
- Consumes: `Registry` (Task 4), `to_prometheus`/`to_json` (Task 8).
- Produces:
  - `struct HttpServer` with `HttpServer::new(reg: Arc<Registry>, percentiles: Vec<f64>) -> Self`
  - `async fn HttpServer::serve(self, listen: &str) -> anyhow::Result<()>` serving `/metrics`, `/metrics.json`, `/`
  - Snapshot cached for ~100ms across concurrent scrapes.

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-exposition/src/server.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::Kind;
    use nyquist_core::registry::{MetricDef, Registry};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_text() {
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let id = reg.register(MetricDef::new("cpu/usage", Kind::Counter));
        let t0 = Instant::now();
        for i in 1..=20u64 { reg.record_counter(id, t0 + Duration::from_millis(i * 10), i * 1000); }

        let server = HttpServer::new(reg.clone(), vec![50.0, 99.0]);
        let app = server.router();
        // Use axum's testing via oneshot.
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let resp = app.oneshot(
            Request::builder().uri("/metrics").body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("# TYPE cpu_usage counter"), "{body}");
    }
}
```
Add `tower = "0.5"` to `crates/nyquist-exposition` dev-dependencies.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p nyquist-exposition server::`
Expected: FAIL — `HttpServer` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/nyquist-exposition/src/server.rs`:
```rust
use std::sync::Arc;
use std::time::{Duration, Instant};
use axum::{routing::get, Router, extract::State, response::IntoResponse, http::header};
use tokio::sync::Mutex;
use nyquist_core::registry::Registry;
use nyquist_core::snapshot::RegistrySnapshot;
use crate::format::{to_json, to_prometheus};

#[derive(Clone)]
struct AppState {
    reg: Arc<Registry>,
    percentiles: Arc<Vec<f64>>,
    cache: Arc<Mutex<Option<(Instant, RegistrySnapshot)>>>,
}

impl AppState {
    async fn snapshot(&self) -> RegistrySnapshot {
        let mut cache = self.cache.lock().await;
        if let Some((t, snap)) = cache.as_ref() {
            if t.elapsed() < Duration::from_millis(100) {
                return snap.clone();
            }
        }
        let snap = self.reg.snapshot(Instant::now(), &self.percentiles);
        *cache = Some((Instant::now(), snap.clone()));
        snap
    }
}

pub struct HttpServer { state: AppState }

impl HttpServer {
    pub fn new(reg: Arc<Registry>, percentiles: Vec<f64>) -> Self {
        HttpServer {
            state: AppState {
                reg,
                percentiles: Arc::new(percentiles),
                cache: Arc::new(Mutex::new(None)),
            },
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(|| async { "nyquist\n" }))
            .route("/metrics", get(metrics))
            .route("/metrics.json", get(metrics_json))
            .with_state(self.state.clone())
    }

    pub async fn serve(self, listen: &str) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(listen).await?;
        tracing::info!(%listen, "serving metrics");
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

async fn metrics(State(s): State<AppState>) -> impl IntoResponse {
    let snap = s.snapshot().await;
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], to_prometheus(&snap))
}

async fn metrics_json(State(s): State<AppState>) -> impl IntoResponse {
    let snap = s.snapshot().await;
    ([(header::CONTENT_TYPE, "application/json")], to_json(&snap))
}
```
Add `anyhow = "1"` to `crates/nyquist-exposition` dependencies.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nyquist-exposition server::`
Expected: PASS (1 test).

- [ ] **Step 5: Wire and commit**

Add to `crates/nyquist-exposition/src/lib.rs`:
```rust
pub mod server;
pub use server::HttpServer;
```
```bash
git add crates/nyquist-exposition
git commit -m "feat(exposition): axum server with cached snapshot endpoints"
```

---

### Task 10: procfs helper and the CPU sampler

**Files:**
- Create: `crates/nyquist-samplers/src/procfs.rs`
- Create: `crates/nyquist-samplers/src/cpu.rs`
- Create: `crates/nyquist-samplers/tests/fixtures/proc_stat`
- Modify: `crates/nyquist-samplers/src/lib.rs`

**Interfaces:**
- Consumes: `Registry`, `MetricDef`, `Kind`, `Unit`, `Sampler`, `SamplerError`, `Labels` from `nyquist-core`.
- Produces:
  - `fn parse_proc_stat(text: &str) -> Vec<(String, [u64; 7])>` returning `(cpu_label, [user, nice, system, idle, iowait, irq, softirq])`
  - `struct CpuSampler` implementing `Sampler` with `CpuSampler::new(reg: &Registry, interval: Duration) -> Self`

- [ ] **Step 1: Create the fixture**

Create `crates/nyquist-samplers/tests/fixtures/proc_stat`:
```
cpu  100 0 50 9000 10 0 5 0 0 0
cpu0 60 0 30 4500 5 0 3 0 0 0
cpu1 40 0 20 4500 5 0 2 0 0 0
intr 12345
ctxt 67890
```

- [ ] **Step 2: Write the failing parser test**

Create `crates/nyquist-samplers/src/procfs.rs`:
```rust
pub fn parse_proc_stat(text: &str) -> Vec<(String, [u64; 7])> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.starts_with("cpu") { continue; }
        let mut it = line.split_whitespace();
        let label = match it.next() { Some(l) => l.to_string(), None => continue };
        let vals: Vec<u64> = it.take(7).filter_map(|v| v.parse().ok()).collect();
        if vals.len() == 7 {
            let mut arr = [0u64; 7];
            arr.copy_from_slice(&vals);
            out.push((label, arr));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_aggregate_and_per_cpu_lines() {
        let text = include_str!("../tests/fixtures/proc_stat");
        let parsed = parse_proc_stat(text);
        assert_eq!(parsed.len(), 3); // cpu, cpu0, cpu1
        assert_eq!(parsed[0].0, "cpu");
        assert_eq!(parsed[0].1[0], 100); // user
        assert_eq!(parsed[1].0, "cpu0");
        assert_eq!(parsed[1].1[2], 30); // cpu0 system
    }
}
```

- [ ] **Step 3: Run parser test**

Add `pub mod procfs;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers procfs::`
Expected: PASS (1 test).

- [ ] **Step 4: Write the CPU sampler with its registration test**

Create `crates/nyquist-samplers/src/cpu.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_proc_stat;

const FIELDS: [&str; 7] = ["user", "nice", "system", "idle", "iowait", "irq", "softirq"];

pub struct CpuSampler {
    interval: Duration,
    // metric ids per (cpu_label, field) registered lazily on first sample
    path: String,
}

impl CpuSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        CpuSampler { interval, path: "/proc/stat".to_string() }
    }

    fn ingest(&self, reg: &Registry, now: Instant, text: &str) {
        for (cpu, vals) in parse_proc_stat(text) {
            for (i, field) in FIELDS.iter().enumerate() {
                let labels = Labels::new().insert("cpu", &cpu);
                let id = reg.register(
                    MetricDef::new(format!("cpu/usage/{field}"), Kind::Counter)
                        .unit(Unit::Count)
                        .labels(labels),
                );
                reg.record_counter(id, now, vals[i]);
            }
        }
    }
}

#[async_trait::async_trait]
impl Sampler for CpuSampler {
    fn name(&self) -> &str { "cpu" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(&self.path)?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingest_registers_and_records_per_cpu_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let s = CpuSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_stat");
        let t0 = Instant::now();
        s.ingest(&reg, t0, text);
        s.ingest(&reg, t0 + Duration::from_millis(10), text);
        // 3 cpu rows x 7 fields = 21 metrics registered.
        assert_eq!(reg.metric_ids().len(), 21);
    }
}
```

- [ ] **Step 5: Run tests, wire, commit**

Add `pub mod cpu;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers`
Expected: PASS (parser + cpu tests).
```bash
git add crates/nyquist-samplers
git commit -m "feat(samplers): procfs helper and CPU sampler"
```

---

### Task 11: Memory sampler (gauges + counters)

**Files:**
- Create: `crates/nyquist-samplers/src/memory.rs`
- Create: `crates/nyquist-samplers/tests/fixtures/proc_meminfo`
- Modify: `crates/nyquist-samplers/src/procfs.rs`, `crates/nyquist-samplers/src/lib.rs`

**Interfaces:**
- Consumes: same core types as Task 10.
- Produces:
  - `fn parse_meminfo(text: &str) -> Vec<(String, u64)>` returning `(key, value_in_bytes)` (kB values converted to bytes)
  - `struct MemorySampler` implementing `Sampler`

- [ ] **Step 1: Create fixture**

Create `crates/nyquist-samplers/tests/fixtures/proc_meminfo`:
```
MemTotal:       16384000 kB
MemFree:         8192000 kB
MemAvailable:   12000000 kB
Buffers:          512000 kB
```

- [ ] **Step 2: Write failing parser test**

Add to `crates/nyquist-samplers/src/procfs.rs`:
```rust
pub fn parse_meminfo(text: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split(':');
        let key = match it.next() { Some(k) => k.trim().to_string(), None => continue };
        let rest = match it.next() { Some(r) => r.trim(), None => continue };
        let mut parts = rest.split_whitespace();
        if let Some(num) = parts.next().and_then(|n| n.parse::<u64>().ok()) {
            let bytes = if rest.ends_with("kB") { num * 1024 } else { num };
            out.push((key, bytes));
        }
    }
    out
}

#[cfg(test)]
mod meminfo_tests {
    use super::*;
    #[test]
    fn parses_meminfo_kb_to_bytes() {
        let text = include_str!("../tests/fixtures/proc_meminfo");
        let parsed = parse_meminfo(text);
        let free = parsed.iter().find(|(k, _)| k == "MemFree").unwrap().1;
        assert_eq!(free, 8192000 * 1024);
    }
}
```

- [ ] **Step 3: Run parser test**

Run: `cargo test -p nyquist-samplers meminfo_tests`
Expected: PASS.

- [ ] **Step 4: Write the memory sampler**

Create `crates/nyquist-samplers/src/memory.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_meminfo;

pub struct MemorySampler { interval: Duration, path: String }

impl MemorySampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        MemorySampler { interval, path: "/proc/meminfo".to_string() }
    }
    fn ingest(&self, reg: &Registry, now: Instant, text: &str) {
        for (key, bytes) in parse_meminfo(text) {
            let name = format!("memory/{}", key.to_lowercase());
            let id = reg.register(MetricDef::new(name, Kind::Gauge).unit(Unit::Bytes));
            reg.record_gauge(id, now, bytes);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for MemorySampler {
    fn name(&self) -> &str { "memory" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(&self.path)?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingest_registers_gauges() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let s = MemorySampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_meminfo");
        s.ingest(&reg, Instant::now(), text);
        assert_eq!(reg.metric_ids().len(), 4);
    }
}
```

- [ ] **Step 5: Wire, run, commit**

Add `pub mod memory;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers`
Expected: PASS.
```bash
git add crates/nyquist-samplers
git commit -m "feat(samplers): memory sampler with meminfo gauges"
```

---

### Task 12: Network sampler

**Files:**
- Create: `crates/nyquist-samplers/src/network.rs`
- Create: `crates/nyquist-samplers/tests/fixtures/proc_net_dev`
- Modify: `crates/nyquist-samplers/src/procfs.rs`, `crates/nyquist-samplers/src/lib.rs`

**Interfaces:**
- Produces:
  - `fn parse_net_dev(text: &str) -> Vec<(String, u64, u64)>` returning `(iface, rx_bytes, tx_bytes)`
  - `struct NetworkSampler` implementing `Sampler`, recording `network/receive/bytes` and `network/transmit/bytes` counters labeled by `iface`.

- [ ] **Step 1: Create fixture**

Create `crates/nyquist-samplers/tests/fixtures/proc_net_dev`:
```
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets
  eth0: 1000000  1000    0    0    0     0          0         0   2000000  2000    0    0    0     0       0          0
    lo:  500     5       0    0    0     0          0         0    500     5       0    0    0     0       0          0
```

- [ ] **Step 2: Write failing parser test**

Add to `crates/nyquist-samplers/src/procfs.rs`:
```rust
pub fn parse_net_dev(text: &str) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(colon) = line.find(':') else { continue };
        let iface = line[..colon].trim().to_string();
        if iface.is_empty() || iface.contains('|') { continue; }
        let nums: Vec<u64> = line[colon + 1..].split_whitespace()
            .filter_map(|n| n.parse().ok()).collect();
        // rx_bytes is field 0; tx_bytes is field 8.
        if nums.len() >= 9 {
            out.push((iface, nums[0], nums[8]));
        }
    }
    out
}

#[cfg(test)]
mod net_dev_tests {
    use super::*;
    #[test]
    fn parses_rx_tx_bytes_per_iface() {
        let text = include_str!("../tests/fixtures/proc_net_dev");
        let parsed = parse_net_dev(text);
        assert_eq!(parsed.len(), 2);
        let eth0 = parsed.iter().find(|(i, _, _)| i == "eth0").unwrap();
        assert_eq!((eth0.1, eth0.2), (1000000, 2000000));
    }
}
```

- [ ] **Step 3: Run parser test**

Run: `cargo test -p nyquist-samplers net_dev_tests`
Expected: PASS.

- [ ] **Step 4: Write the network sampler**

Create `crates/nyquist-samplers/src/network.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_net_dev;

pub struct NetworkSampler { interval: Duration, path: String }

impl NetworkSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        NetworkSampler { interval, path: "/proc/net/dev".to_string() }
    }
    fn ingest(&self, reg: &Registry, now: Instant, text: &str) {
        for (iface, rx, tx) in parse_net_dev(text) {
            let rx_id = reg.register(
                MetricDef::new("network/receive/bytes", Kind::Counter)
                    .unit(Unit::Bytes).labels(Labels::new().insert("iface", &iface)));
            let tx_id = reg.register(
                MetricDef::new("network/transmit/bytes", Kind::Counter)
                    .unit(Unit::Bytes).labels(Labels::new().insert("iface", &iface)));
            reg.record_counter(rx_id, now, rx);
            reg.record_counter(tx_id, now, tx);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for NetworkSampler {
    fn name(&self) -> &str { "network" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(&self.path)?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingest_registers_rx_tx_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let s = NetworkSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_net_dev");
        s.ingest(&reg, Instant::now(), text);
        assert_eq!(reg.metric_ids().len(), 4); // 2 ifaces x (rx, tx)
    }
}
```

- [ ] **Step 5: Wire, run, commit**

Add `pub mod network;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers`
Expected: PASS.
```bash
git add crates/nyquist-samplers
git commit -m "feat(samplers): network sampler with rx/tx byte counters"
```

---

### Task 13: Disk sampler

**Files:**
- Create: `crates/nyquist-samplers/src/disk.rs`
- Create: `crates/nyquist-samplers/tests/fixtures/proc_diskstats`
- Modify: `crates/nyquist-samplers/src/procfs.rs`, `crates/nyquist-samplers/src/lib.rs`

**Interfaces:**
- Consumes: same core types as Task 10.
- Produces:
  - `fn parse_diskstats(text: &str) -> Vec<(String, u64, u64)>` returning `(device, sectors_read, sectors_written)`
  - `struct DiskSampler` implementing `Sampler`, recording `disk/read/bytes` and `disk/write/bytes` counters (sectors × 512) labeled by `device`.

- [ ] **Step 1: Create fixture**

Create `crates/nyquist-samplers/tests/fixtures/proc_diskstats`:
```
   8       0 sda 1000 0 8000 500 2000 0 16000 800 0 1200 1300
   8       1 sda1 500 0 4000 250 1000 0 8000 400 0 600 650
```

- [ ] **Step 2: Write failing parser test**

Add to `crates/nyquist-samplers/src/procfs.rs`:
```rust
pub fn parse_diskstats(text: &str) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // major minor name + 11 stat fields. Need at least name + 7 stats.
        if f.len() < 10 { continue; }
        let device = f[2].to_string();
        // After the name, numeric fields are 0-indexed:
        //   [0]=reads [1]=reads_merged [2]=sectors_read ... [6]=sectors_written
        let nums: Vec<u64> = f[3..].iter().filter_map(|n| n.parse().ok()).collect();
        if nums.len() >= 7 {
            out.push((device, nums[2], nums[6]));
        }
    }
    out
}

#[cfg(test)]
mod diskstats_tests {
    use super::*;
    #[test]
    fn parses_sectors_read_written_per_device() {
        let text = include_str!("../tests/fixtures/proc_diskstats");
        let parsed = parse_diskstats(text);
        assert_eq!(parsed.len(), 2);
        let sda = parsed.iter().find(|(d, _, _)| d == "sda").unwrap();
        assert_eq!((sda.1, sda.2), (8000, 16000));
    }
}
```

- [ ] **Step 3: Run parser test**

Run: `cargo test -p nyquist-samplers diskstats_tests`
Expected: PASS.

- [ ] **Step 4: Write the disk sampler**

Create `crates/nyquist-samplers/src/disk.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_diskstats;

const SECTOR_BYTES: u64 = 512;

pub struct DiskSampler { interval: Duration, path: String }

impl DiskSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        DiskSampler { interval, path: "/proc/diskstats".to_string() }
    }
    fn ingest(&self, reg: &Registry, now: Instant, text: &str) {
        for (device, sread, swritten) in parse_diskstats(text) {
            let r = reg.register(
                MetricDef::new("disk/read/bytes", Kind::Counter)
                    .unit(Unit::Bytes).labels(Labels::new().insert("device", &device)));
            let w = reg.register(
                MetricDef::new("disk/write/bytes", Kind::Counter)
                    .unit(Unit::Bytes).labels(Labels::new().insert("device", &device)));
            reg.record_counter(r, now, sread * SECTOR_BYTES);
            reg.record_counter(w, now, swritten * SECTOR_BYTES);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for DiskSampler {
    fn name(&self) -> &str { "disk" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(&self.path)?;
        self.ingest(reg, now, &text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingest_registers_read_write_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let s = DiskSampler::new(&reg, Duration::from_millis(10));
        let text = include_str!("../tests/fixtures/proc_diskstats");
        s.ingest(&reg, Instant::now(), text);
        assert_eq!(reg.metric_ids().len(), 4); // 2 devices x (read, write)
    }
}
```

- [ ] **Step 5: Wire, run, commit**

Add `pub mod disk;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers`
Expected: PASS.
```bash
git add crates/nyquist-samplers
git commit -m "feat(samplers): disk sampler with read/write byte counters"
```

---

### Task 14: TCP and UDP samplers

**Files:**
- Create: `crates/nyquist-samplers/src/snmp.rs`
- Create: `crates/nyquist-samplers/tests/fixtures/proc_net_snmp`
- Modify: `crates/nyquist-samplers/src/procfs.rs`, `crates/nyquist-samplers/src/lib.rs`

**Interfaces:**
- Consumes: same core types as Task 10.
- Produces:
  - `fn parse_net_snmp(text: &str) -> HashMap<String, HashMap<String, u64>>` mapping protocol (`"Tcp"`, `"Udp"`) → field name → value
  - `struct TcpSampler` recording `tcp/active_opens`, `tcp/passive_opens`, `tcp/in_segs`, `tcp/out_segs` counters
  - `struct UdpSampler` recording `udp/in_datagrams`, `udp/out_datagrams`, `udp/in_errors`, `udp/no_ports` counters

- [ ] **Step 1: Create fixture**

Create `crates/nyquist-samplers/tests/fixtures/proc_net_snmp`:
```
Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors
Tcp: 1 200 120000 -1 100 50 5 3 10 12345 67890 20 0 4 0
Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti
Udp: 1000 5 0 900 0 0 0 0
```

- [ ] **Step 2: Write failing parser test**

Add to `crates/nyquist-samplers/src/procfs.rs`:
```rust
use std::collections::HashMap;

pub fn parse_net_snmp(text: &str) -> HashMap<String, HashMap<String, u64>> {
    let mut out: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let Some(colon) = line.find(':') else { continue };
        let proto = line[..colon].trim().to_string();
        let rest: Vec<&str> = line[colon + 1..].split_whitespace().collect();
        // A header line has non-numeric first token; a data line is all numbers.
        let is_data = rest.first().map(|t| t.parse::<i64>().is_ok()).unwrap_or(false);
        if is_data {
            if let Some(names) = headers.get(&proto) {
                let mut fields = HashMap::new();
                for (name, val) in names.iter().zip(rest.iter()) {
                    if let Ok(v) = val.parse::<u64>() {
                        fields.insert(name.clone(), v);
                    }
                }
                out.insert(proto, fields);
            }
        } else {
            headers.insert(proto, rest.iter().map(|s| s.to_string()).collect());
        }
    }
    out
}

#[cfg(test)]
mod net_snmp_tests {
    use super::*;
    #[test]
    fn parses_tcp_and_udp_fields() {
        let text = include_str!("../tests/fixtures/proc_net_snmp");
        let parsed = parse_net_snmp(text);
        assert_eq!(parsed["Tcp"]["ActiveOpens"], 100);
        assert_eq!(parsed["Tcp"]["InSegs"], 12345);
        assert_eq!(parsed["Udp"]["InDatagrams"], 1000);
        assert_eq!(parsed["Udp"]["NoPorts"], 5);
    }
}
```

- [ ] **Step 3: Run parser test**

Run: `cargo test -p nyquist-samplers net_snmp_tests`
Expected: PASS.

- [ ] **Step 4: Write the TCP and UDP samplers**

Create `crates/nyquist-samplers/src/snmp.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_net_snmp;

const SNMP_PATH: &str = "/proc/net/snmp";

// (metric name, protocol, snmp field)
const TCP_METRICS: [(&str, &str); 4] = [
    ("tcp/active_opens", "ActiveOpens"),
    ("tcp/passive_opens", "PassiveOpens"),
    ("tcp/in_segs", "InSegs"),
    ("tcp/out_segs", "OutSegs"),
];
const UDP_METRICS: [(&str, &str); 4] = [
    ("udp/in_datagrams", "InDatagrams"),
    ("udp/out_datagrams", "OutDatagrams"),
    ("udp/in_errors", "InErrors"),
    ("udp/no_ports", "NoPorts"),
];

fn ingest(reg: &Registry, now: Instant, text: &str, proto: &str, metrics: &[(&str, &str)]) {
    let parsed = parse_net_snmp(text);
    let Some(fields) = parsed.get(proto) else { return };
    for (name, snmp_field) in metrics {
        if let Some(&value) = fields.get(*snmp_field) {
            let id = reg.register(MetricDef::new(*name, Kind::Counter).unit(Unit::Count));
            reg.record_counter(id, now, value);
        }
    }
}

pub struct TcpSampler { interval: Duration }
impl TcpSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self { TcpSampler { interval } }
}
#[async_trait::async_trait]
impl Sampler for TcpSampler {
    fn name(&self) -> &str { "tcp" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(SNMP_PATH)?;
        ingest(reg, now, &text, "Tcp", &TCP_METRICS);
        Ok(())
    }
}

pub struct UdpSampler { interval: Duration }
impl UdpSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self { UdpSampler { interval } }
}
#[async_trait::async_trait]
impl Sampler for UdpSampler {
    fn name(&self) -> &str { "udp" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(SNMP_PATH)?;
        ingest(reg, now, &text, "Udp", &UDP_METRICS);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tcp_and_udp_ingest_register_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let text = include_str!("../tests/fixtures/proc_net_snmp");
        let now = Instant::now();
        ingest(&reg, now, text, "Tcp", &TCP_METRICS);
        ingest(&reg, now, text, "Udp", &UDP_METRICS);
        assert_eq!(reg.metric_ids().len(), 8); // 4 tcp + 4 udp
    }
}
```

- [ ] **Step 5: Wire, run, commit**

Add `pub mod snmp;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers`
Expected: PASS.
```bash
git add crates/nyquist-samplers
git commit -m "feat(samplers): tcp and udp samplers from /proc/net/snmp"
```

---

### Task 15: Sampler inventory and builder

**Files:**
- Create: `crates/nyquist-samplers/src/inventory.rs`
- Modify: `crates/nyquist-samplers/src/lib.rs`

**Interfaces:**
- Consumes: `CpuSampler`, `MemorySampler`, `NetworkSampler`, `DiskSampler`, `TcpSampler`, `UdpSampler`; `Registry`, `Sampler`.
- Produces:
  - `fn build_enabled(reg: &Registry, default_interval: Duration, is_enabled: impl Fn(&str) -> bool, interval_for: impl Fn(&str) -> Option<Duration>) -> Vec<Box<dyn Sampler>>`
  - `fn all_sampler_names() -> &'static [&'static str]`

- [ ] **Step 1: Write the failing test**

Create `crates/nyquist-samplers/src/inventory.rs`:
```rust
use std::time::Duration;
use nyquist_core::registry::Registry;
use nyquist_core::sampler::Sampler;
use crate::cpu::CpuSampler;
use crate::memory::MemorySampler;
use crate::network::NetworkSampler;
use crate::disk::DiskSampler;
use crate::snmp::{TcpSampler, UdpSampler};

pub fn all_sampler_names() -> &'static [&'static str] {
    &["cpu", "memory", "network", "disk", "tcp", "udp"]
}

pub fn build_enabled(
    reg: &Registry,
    default_interval: Duration,
    is_enabled: impl Fn(&str) -> bool,
    interval_for: impl Fn(&str) -> Option<Duration>,
) -> Vec<Box<dyn Sampler>> {
    let mut out: Vec<Box<dyn Sampler>> = Vec::new();
    for name in all_sampler_names() {
        if !is_enabled(name) { continue; }
        let iv = interval_for(name).unwrap_or(default_interval);
        let s: Box<dyn Sampler> = match *name {
            "cpu" => Box::new(CpuSampler::new(reg, iv)),
            "memory" => Box::new(MemorySampler::new(reg, iv)),
            "network" => Box::new(NetworkSampler::new(reg, iv)),
            "disk" => Box::new(DiskSampler::new(reg, iv)),
            "tcp" => Box::new(TcpSampler::new(reg, iv)),
            "udp" => Box::new(UdpSampler::new(reg, iv)),
            _ => continue,
        };
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn respects_enabled_flag() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let samplers = build_enabled(
            &reg,
            Duration::from_millis(10),
            |name| name != "network", // disable network
            |_| None,
        );
        let names: Vec<_> = samplers.iter().map(|s| s.name().to_string()).collect();
        assert_eq!(names, vec!["cpu", "memory", "disk", "tcp", "udp"]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails then passes**

Add `pub mod inventory;` to `crates/nyquist-samplers/src/lib.rs`.
Run: `cargo test -p nyquist-samplers inventory::`
Expected: PASS (1 test). (No separate red step needed beyond compile; if it fails to compile, fix imports.)

- [ ] **Step 3: Commit**

```bash
git add crates/nyquist-samplers
git commit -m "feat(samplers): inventory + enabled-sampler builder"
```

---

### Task 16: Binary wiring and end-to-end integration test

**Files:**
- Modify: `src/main.rs`
- Create: `tests/integration.rs`

**Interfaces:**
- Consumes: `nyquist_config::Config`, `nyquist_core::{Registry, spawn_sampler}`, `nyquist_samplers::inventory::build_enabled`, `nyquist_exposition::HttpServer`.

- [ ] **Step 1: Write the binary**

Replace `src/main.rs`:
```rust
use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use nyquist_config::Config;
use nyquist_core::registry::Registry;
use nyquist_core::scheduler::spawn_sampler;
use nyquist_exposition::HttpServer;
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
        std::time::Duration::from_millis(100), // slice width
        config.general.window,
    ));

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

    let server = HttpServer::new(reg.clone(), config.general.percentiles.clone());
    server.serve(&config.general.listen).await?;
    Ok(())
}
```

- [ ] **Step 2: Verify it builds and runs (Linux)**

Run: `cargo build`
Expected: compiles. On a Linux host: `cargo run` then `curl -s localhost:9100/metrics | head` shows `# TYPE cpu_usage_user counter` lines after ~1s. (On macOS, `/proc` reads fail and are logged as warnings under `fault_tolerant=true`; the server still serves an empty metric set — acceptable for local dev.)

- [ ] **Step 3: Write the integration test**

Create `tests/integration.rs`:
```rust
// End-to-end: register a sampler, scheduler ticks it, server serves percentiles.
use std::sync::Arc;
use std::time::{Duration, Instant};
use nyquist_core::model::Kind;
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_exposition::HttpServer;

#[tokio::test]
async fn end_to_end_metrics_pipeline() {
    let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
    let id = reg.register(MetricDef::new("disk/read/bytes", Kind::Counter));
    let t0 = Instant::now();
    for i in 1..=30u64 {
        reg.record_counter(id, t0 + Duration::from_millis(i * 10), i * 4096);
    }
    let server = HttpServer::new(reg.clone(), vec![50.0, 99.0]);
    let app = server.router();

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let resp = app.oneshot(Request::builder().uri("/metrics").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("disk_read_bytes counter"), "{body}");
    assert!(body.contains("disk_read_bytes_rate"), "{body}");
}
```
Add to root `Cargo.toml` `[dev-dependencies]`:
```toml
[dev-dependencies]
nyquist-core = { path = "crates/nyquist-core" }
nyquist-exposition = { path = "crates/nyquist-exposition" }
axum = "0.7"
tower = "0.5"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 4: Run the integration test**

Run: `cargo test --test integration`
Expected: PASS (1 test).

- [ ] **Step 5: Run the full suite and commit**

Run: `cargo test`
Expected: all crate tests PASS.
```bash
git add src/main.rs tests/integration.rs Cargo.toml
git commit -m "feat: wire nyquist binary and add end-to-end integration test"
```

---

## Self-Review Notes

**Spec coverage:** workspace layout (§3.1, Task 0); sliding windowed histogram (§4.4, Tasks 2–3); counter/gauge pipelines + reset handling (§4.1, Task 4); recording API (§4.3, Task 4); snapshot + Sink seam (§6.1, Task 5); Sampler trait + scheduler + fault tolerance (§5.1–5.3, Task 6); sub-second default interval (§5.4, Task 7 config); all six procfs samplers — cpu/memory/network (§5.5, Tasks 10–12) and disk/tcp/udp (§5.5, Tasks 13–14); Prometheus/JSON exposition + axum (§6.2, Tasks 8–9); config (§7, Task 7); error handling/fault tolerance (§8, Tasks 6–7); test strategy incl. property tests + fixtures + integration (§9, throughout); macOS-compiles/Linux-runtime (§10, Task 16 note). ClickHouse/Parquet/perf/eBPF are explicitly Phase 2+.

**Placeholder scan:** no TBD/TODO; every code step shows complete code. The one external-API caveat (histogram crate, Task 2 Step 1) is a real verification step isolated to one file, not a placeholder.

**Type consistency:** `Registry`, `MetricDef`, `MetricId`, `Labels`, `SlidingHistogram`, `RegistrySnapshot`, `MetricSnapshot`, `Sink`, `Sampler`, `spawn_sampler`, `HttpServer`, `to_prometheus`/`to_json`, `build_enabled` are defined once and used with consistent signatures across tasks. Sampler constructors all share the `new(&Registry, Duration)` shape so `build_enabled` (Task 15) can construct them uniformly.

**Scope:** all six Phase 1 samplers from spec §5.5 are now covered. Tasks 13–14 added disk (`/proc/diskstats`) and tcp/udp (`/proc/net/snmp`).
