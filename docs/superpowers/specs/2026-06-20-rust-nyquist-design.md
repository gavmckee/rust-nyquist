# rust-nyquist — Design Specification

**Date:** 2026-06-20
**Status:** Phase 1 design approved (pending written-spec review)
**Inspiration:** [rezolus](https://github.com/iopsystems/rezolus) — specifically its *oversampling → windowed histogram → percentile* telemetry model.

---

## 1. Vision

`rust-nyquist` is a high-resolution systems performance telemetry agent. Its
defining feature — inherited from rezolus and the reason for the name — is
**oversampling**: every telemetry source is sampled far more often than it is
reported, so that instead of a single point-in-time value we build a
**histogram across a time window** and expose its **percentiles**.

### Why oversampling (the Nyquist insight)

A counter sampled once per second reports the *average* rate over that second.
A burst — say 900 MB/s for 50 ms inside an otherwise-quiet second — disappears
into a ~50 MB/s average. To observe events at timescale *t*, you must sample
faster than *t/2* (Nyquist–Shannon). So we sample counters at a sub-second
interval, compute the instantaneous rate at each step, and keep the
**distribution** of those rates. The burst then appears as a fat upper
percentile (p99 / p99.9) that naive gauges never reveal.

This is the single most important property of the system. Every design decision
below serves it.

---

## 2. Scope

**Goal:** a full rezolus-class tool, built in modern Rust, decomposed into
phases. This document fully specifies **Phase 1** and sketches the roadmap.

### Phase roadmap

| Phase | Sub-project | Notes |
|---|---|---|
| **1 (this spec)** | Core engine + sampler framework + procfs samplers + Prometheus/JSON exposition + `Sink` seam | A working agent that oversamples CPU/memory/network/disk/tcp/udp from procfs and serves percentile metrics. |
| 2 | Push sinks: Parquet high-res recorder + ClickHouse rollup sink | Both consume the same `RegistrySnapshot`. |
| 3 | perf_events samplers | Userspace `perf_event_open`; hardware/software counters. No eBPF. |
| 4 | eBPF (Aya) samplers | BPF abstraction trait + in-kernel distribution (latency) samplers. |
| 5 | Breadth + remote outputs | Remaining samplers, remote-write/Kafka, polish. |

Each phase gets its own spec → plan → implementation cycle.

### Confirmed technology decisions

- **eBPF toolchain (Phase 4): Aya** — pure-Rust userspace + kernel code, CO-RE
  portability, no runtime libbcc/LLVM dependency. Abstracted behind a trait so
  `nyquist-core` never depends on it directly.
- **Histogram math: the `histogram` crate** (log-linear, bounded relative
  error, mergeable, atomic/windowed variants). We do *not* hand-roll bucketing.
- **Windowing: sliding window** built from a ring of mergeable histogram slices.
- **Output scope: live exposition + high-res recording** (recording is Phase 2).
- **ClickHouse sink: windowed percentile rollups** (Phase 2), schema derived
  from metric metadata — samplers write no SQL.
- **HTTP server: `axum`**. **Async runtime: `tokio`** (multi-threaded).
- **Config: TOML + `clap`**. **Errors: `thiserror` (core) + `anyhow` (edges)**.

---

## 3. Architecture

### 3.1 Workspace layout

A Cargo workspace keeps boundaries clean and the engine reusable as a library.

```
rust-nyquist/
├── crates/
│   ├── nyquist-core/        # data model, oversampling engine, Sampler trait,
│   │                        #   scheduler, Registry, Sink trait, snapshots
│   ├── nyquist-samplers/    # concrete procfs samplers (cpu, memory, …)
│   ├── nyquist-exposition/  # Prometheus/OpenMetrics + JSON, axum server (a Sink)
│   └── nyquist-config/      # TOML config types + loading
├── src/main.rs              # bin `nyquist`: clap CLI; wires config → registry
│                            #   → samplers → sinks
```

Dependency direction: `nyquist-core` depends on nothing project-specific.
Samplers, exposition, and config depend on core. The binary depends on all.
Later phases add `nyquist-recorder`, `nyquist-clickhouse` (Phase 2) and
`nyquist-bpf` (Phase 4) without changing core's public API.

### 3.2 Concurrency model

- `tokio` multi-threaded runtime.
- Each enabled sampler runs as its own `tokio::spawn` task driven by its own
  `interval` timer.
- Samplers write into a shared `Arc<Registry>` using lock-free atomics
  (`DashMap` for the metric map, atomics within each metric). No inter-sampler
  messaging.
- Sinks read snapshots from the registry. The Prometheus sink reads on-scrape;
  push sinks (Phase 2) read on their own timer.

---

## 4. The oversampling engine (`nyquist-core`)

### 4.1 Metric kinds and pipelines

```rust
enum Kind { Counter, Gauge, Distribution }
```

| Kind | Example | Pipeline |
|---|---|---|
| **Counter** | bytes sent, ctx switches | each tick: `rate = (v − v_prev) / Δt` → insert `rate` into the windowed histogram. Expose raw total **and** rate percentiles. Handle counter reset/wrap (if `v < v_prev`, treat as reset: skip one interval). |
| **Gauge** | queue depth, temperature | each tick: insert reading into windowed histogram. Expose last value **and** percentiles. |
| **Distribution** | eBPF latencies (Phase 4) | histogram produced at source, merged into the window. Expose percentiles. |

### 4.2 Core types

```rust
struct Metric {
    name: String,
    kind: Kind,
    unit: Unit,                 // for exposition + derived schemas
    description: Option<String>,
    labels: Labels,
    window: SlidingHistogram,   // built on the `histogram` crate
    raw: AtomicU64,             // last total (counter) / last value (gauge)
    prev: AtomicU64,            // previous counter sample (for rate)
    prev_ts: AtomicInstant,
}

struct Registry { metrics: DashMap<MetricId, Arc<Metric>> }
```

### 4.3 Recording API

Samplers only record; they never format or expose:

```rust
impl Registry {
    fn record_counter(&self, id: MetricId, now: Instant, value: u64);
    fn record_gauge(&self, id: MetricId, now: Instant, value: u64);
    fn record_distribution(&self, id: MetricId, now: Instant, h: &Histogram); // Phase 4
}
```

### 4.4 Sliding windowed histogram

The load-bearing component.

- Time is divided into fixed **slices** (default 100 ms). Each slice is its own
  histogram. A ring buffer holds enough slices to cover the configured window
  (e.g. 60 s ÷ 100 ms = 600 slices).
- Each sample lands in the current slice. On snapshot, the slices spanning the
  requested window are **merged**, and percentiles are computed from the merge.
- Old slices are recycled (cleared and reused) as the ring advances — bounded
  memory, true sliding recency, no tumbling-window boundary artifacts.

Rationale: tumbling windows double-count or drop samples at edges; a
free-running histogram never forgets. The ring-of-slices gives accurate sliding
percentiles with fixed memory. This mirrors rezolus's approach.

---

## 5. Sampler framework

### 5.1 The trait

```rust
#[async_trait]
trait Sampler: Send {
    fn name(&self) -> &str;
    fn interval(&self) -> Duration;  // per-sampler; overrides global default
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<()>;
}
```

Deliberately minimal — a sampler's one job is to produce readings into the
registry.

### 5.2 Scheduler

Each enabled sampler gets a tokio task with its own `interval` timer; on each
tick it calls `sample()`. A sampler returning `Err` is logged; with
`fault_tolerant = true` (default) the task continues next tick, otherwise the
agent exits non-zero. One failing sampler never kills the agent.

### 5.3 Registration

Adding a sampler = implement the trait + add it to a **static inventory list**
in `nyquist-samplers` (e.g. via a registration function or `inventory`-style
list). No edits scattered through `main.rs` (an explicit improvement over the
original rezolus, which spawns each sampler by hand in `main`).

### 5.4 Sampling interval default

Oversampling requires a **sub-second default sample interval** — the entire
point. Default **10 ms** sample interval, configurable per sampler. The
**reporting window** is separate (default 1 s for live, larger for rollups).
Accepts modestly higher CPU in exchange for genuine burst capture.

### 5.5 Phase 1 concrete samplers (procfs/sysfs)

| Sampler | Source | Exercises |
|---|---|---|
| `cpu` | `/proc/stat` | counters (per-CPU usage) |
| `memory` | `/proc/meminfo`, `/proc/vmstat` | gauges + counters |
| `network` | `/proc/net/dev` | counters (bytes/packets/errors) |
| `disk` | `/proc/diskstats` | counters (IOPs/bandwidth) |
| `tcp` / `udp` | `/proc/net/snmp` | protocol counters |

This breadth exercises counters, gauges, and multiple parse formats.

---

## 6. Outputs

### 6.1 The `Sink` seam (Phase 1)

All output destinations are consumers of a registry snapshot:

```rust
struct RegistrySnapshot { /* per-metric: raw value + merged window percentiles */ }

#[async_trait]
trait Sink: Send {
    async fn export(&mut self, snap: &RegistrySnapshot, now: SystemTime) -> Result<()>;
}
```

Prometheus exposition (Phase 1), the Parquet recorder (Phase 2), and the
ClickHouse rollup sink (Phase 2) are all `Sink` implementations. The pull-based
Prometheus endpoint computes its snapshot on-scrape; push sinks run on a timer.

### 6.2 Prometheus / JSON exposition (Phase 1, `nyquist-exposition`)

`axum` HTTP server:

- `GET /metrics` — Prometheus/OpenMetrics text. Each metric emits the raw
  total/value **plus** percentile series, e.g.
  `network_transmit_bytes_rate{percentile="99"}`. Percentile set configurable
  (`p50/p90/p99/p99.9` by default).
- `GET /metrics.json` — same data, JSON.
- `GET /` — health/landing.

Snapshots are computed on-scrape (merge window slices → percentiles),
rate-limited with a short cache (~100 ms) so concurrent scrapes are cheap.

### 6.3 ClickHouse rollup sink (Phase 2, design-anticipated)

- Mode: **windowed percentile rollups** — once per reporting window, write one
  row per sampler with raw totals + percentile columns (`p50/p90/p99/p99.9`).
  Modest volume, batched inserts, ideal for dashboards/analytics.
- **Schema is derived from metric metadata**, not hand-written per sampler. The
  sink reads the metrics a sampler registered (name, kind, unit, labels) and
  generates a wide analytical table per sampler (e.g. a `cpu` table with columns
  for each usage metric and its percentiles). Samplers write no SQL and stay
  output-agnostic. The optional `unit`/`description` fields on `Metric` exist to
  make these generated schemas good.

### 6.4 Parquet high-res recorder (Phase 2, design-anticipated)

Records full-resolution data to disk for later replay/analysis — the second
half of "live + recording." Shares the snapshot mechanism. Detailed in the
Phase 2 spec.

---

## 7. Configuration

TOML file (`--config`) with sensible built-in defaults so it runs with none.

```toml
[general]
listen = "0.0.0.0:9100"
default_interval = "10ms"     # oversampling rate
window = "1m"                 # reporting window for percentiles
percentiles = [50.0, 90.0, 99.0, 99.9]
fault_tolerant = true
log = "info"

[samplers.cpu]
enabled = true

[samplers.network]
enabled = true
interval = "5ms"              # per-sampler override
```

CLI via `clap`: `nyquist --config nyquist.toml`, `-v`/`-vv` for verbosity.

---

## 8. Error handling

- `thiserror` for typed errors in `nyquist-core`; `anyhow` at binary/edge
  boundaries.
- Sampler errors: logged; tolerated when `fault_tolerant = true`, else
  non-zero exit.
- Missing or permission-denied procfs files degrade gracefully: skip that
  metric, warn once (no log spam).

---

## 9. Testing strategy

Test-driven throughout (the `test-driven-development` skill drives
implementation).

- **`nyquist-core`:** property tests on the sliding window — percentile
  accuracy vs. a brute-force oracle; ring recycling correctness; counter
  reset/wrap handling. Heaviest coverage; this is where bugs hurt.
- **Samplers:** parse committed `/proc/*` fixture files → assert parsed
  metrics. No live `/proc` dependence in unit tests.
- **Exposition:** golden-output tests for Prometheus/JSON formatting.
- **Integration:** run the binary on Linux CI, scrape `/metrics`, assert
  well-formed output.

---

## 10. Platform

Linux-only at runtime (procfs / perf_events / eBPF). The workspace must still
**compile** on macOS for local development: Linux-specific samplers sit behind
`#[cfg(target_os = "linux")]`, with a mock/synthetic source available on other
platforms so the engine and exposition can be developed and tested anywhere.

---

## 11. Open items deferred to later phases

- Parquet recorder file format and replay tooling (Phase 2 spec).
- ClickHouse connection/batching/retry details and generated-DDL specifics
  (Phase 2 spec).
- perf_events event selection and per-CPU attach model (Phase 3 spec).
- Aya BPF abstraction trait shape and the first distribution samplers
  (Phase 4 spec).
- Remote-write / Kafka output (Phase 5 spec).
