# rust-nyquist Phase 3 — Design Specification

**Date:** 2026-06-20
**Status:** Approved for implementation
**Builds on:** Phase 2 spec — Parquet recorder + ClickHouse sink

---

## 1. Vision

Phase 3 adds a `nyquist-perf` crate that wraps Linux's `perf_event_open(2)` syscall to collect hardware and software performance counters at sub-second resolution. These metrics are invisible to procfs: CPU cycle counts, retired instructions, cache miss rates, and branch mispredicts capture microarchitectural behaviour that `/proc/stat` cannot see.

The sampler slots directly into the existing `Sampler` trait with no changes to `nyquist-core`. Like procfs samplers, it records counters into the shared `Registry`; the rest of the pipeline (sliding histogram, Prometheus/Parquet/ClickHouse exposition) is unchanged.

No eBPF is used in this phase — that is Phase 4 (Aya).

---

## 2. Scope

**In scope:**
- New crate `crates/nyquist-perf`
- `HardwareSampler`: per-CPU hardware counters (cycles, instructions, cache, branch)
- `SoftwareSampler`: per-CPU software counters (context switches, page faults, CPU migrations)
- `build_perf_enabled()` inventory function (analogous to Phase 1's `build_enabled`)
- Config additions: `[perf]` section
- Binary wiring in `src/main.rs`
- Graceful permission degradation (CAP_PERFMON / perf_event_paranoid handling)

**Out of scope:** Cache-detail events (L1/LLC breakdown), hardware tracepoints, PMU raw events, per-process monitoring, eBPF (Phase 4), sampling mode (record vs. count).

---

## 3. Architecture

### 3.1 New crate

```
crates/nyquist-perf/
  Cargo.toml
  src/lib.rs          # re-exports + build_perf_enabled()
  src/events.rs       # PerfCounter: per-CPU counter wrapper; CPU detection
  src/hw.rs           # HardwareSampler
  src/sw.rs           # SoftwareSampler
```

Dependency rule: `nyquist-perf` depends only on `nyquist-core`. The `perf-event` crate is a platform-conditional dependency (`[target.'cfg(target_os = "linux")'.dependencies]`), so the crate compiles on macOS with stubs.

### 3.2 macOS compatibility

On macOS, `build_perf_enabled()` returns an empty `Vec`. The sampler types themselves are defined only under `#[cfg(target_os = "linux")]`. The workspace continues to compile everywhere.

### 3.3 Data flow

```
perf_event_open (one fd per CPU per event)
        │
        ▼
 PerfCounter::read()  ─►  Δvalue / Δt  ─►  Registry::record_counter()
                                                     │
                                             SlidingHistogram
                                                     │
                                    Prometheus / Parquet / ClickHouse
```

---

## 4. `nyquist-perf` crate

### 4.1 Dependencies

```toml
[dependencies]
nyquist-core = { path = "../nyquist-core" }
async-trait  = "0.1"
tracing      = "0.1"
thiserror    = "1"

[target.'cfg(target_os = "linux")'.dependencies]
perf-event = "0.4"
```

### 4.2 `PerfCounter` — per-CPU counter abstraction

```rust
#[cfg(target_os = "linux")]
pub struct PerfCounter {
    counter: perf_event::Counter,
}

#[cfg(target_os = "linux")]
impl PerfCounter {
    pub fn open(cpu: usize, kind: impl perf_event::events::Event) -> Result<Self, PerfError>
    pub fn read(&mut self) -> Result<u64, PerfError>
}
```

`open()` calls `Builder::new().kind(kind).observe_cpu(cpu as i32).observe_pid(-1).build()`. If the syscall fails with `EPERM` or `EACCES`, it returns `Err(PerfError::Permission)` — a distinct variant so callers can detect it and disable cleanly.

CPU detection: `num_cpus()` reads `/sys/devices/system/cpu/present` (e.g. `"0-191"`) and returns the integer count. Fallback: `std::thread::available_parallelism()`.

### 4.3 `HardwareSampler`

Metric names and corresponding `perf_event::events::Hardware` variants:

| Metric name | Event |
|---|---|
| `perf/hw/cpu_cycles` | `CPU_CYCLES` |
| `perf/hw/instructions` | `INSTRUCTIONS` |
| `perf/hw/cache_references` | `CACHE_REFERENCES` |
| `perf/hw/cache_misses` | `CACHE_MISSES` |
| `perf/hw/branch_instructions` | `BRANCH_INSTRUCTIONS` |
| `perf/hw/branch_misses` | `BRANCH_MISSES` |

All are `Kind::Counter`, labeled `cpu="cpu0"` … `cpu="cpuN"`. Rate percentiles expose bursts of IPC spikes.

### 4.4 `SoftwareSampler`

| Metric name | Event |
|---|---|
| `perf/sw/context_switches` | `CONTEXT_SWITCHES` |
| `perf/sw/page_faults` | `PAGE_FAULTS` |
| `perf/sw/cpu_migrations` | `CPU_MIGRATIONS` |

Same per-CPU label approach.

### 4.5 Permission handling

The `sample()` method tries to open counters lazily on the **first tick**. If any open returns `PerfError::Permission`:

1. Emit a single `tracing::warn!` explaining the requirement (CAP_PERFMON, or `sysctl kernel.perf_event_paranoid=1`).
2. Set an internal `disabled: bool` flag.
3. On subsequent ticks, return `Ok(())` immediately — no log spam, no panic.

This is consistent with Phase 1's `fault_tolerant` behaviour: one bad sampler never kills the agent.

### 4.6 File-descriptor budget

Each `HardwareSampler` opens 6 fds per CPU; each `SoftwareSampler` opens 3 fds per CPU. On a 192-CPU host: 6 × 192 = 1152, 3 × 192 = 576. The combined total (1728) exceeds the default per-process limit of 1024. The default config therefore sets `max_cpus = 32`; users on smaller hosts get per-CPU resolution, users on larger hosts can raise it (or rely on the system ulimit).

```toml
[perf]
enabled         = false
max_cpus        = 32      # limit CPUs monitored; 0 = all
hw_enabled      = true
sw_enabled      = true
```

`max_cpus = 0` means "all CPUs"; the sampler computes `min(detected_cpus, max_cpus_effective)` where `max_cpus_effective = max_cpus` if `max_cpus > 0`, else `detected_cpus`.

### 4.7 `build_perf_enabled()`

```rust
pub fn build_perf_enabled(
    reg: &Registry,
    default_interval: Duration,
    cfg: &PerfConfig,
) -> Vec<Box<dyn Sampler>>
```

Returns up to two samplers (hw, sw) if `cfg.enabled` and the respective sub-flag is set.

---

## 5. Configuration

New section in `nyquist-config`:

```toml
[perf]
enabled   = false     # master switch; perf_event_open is not attempted if false
max_cpus  = 32        # CPUs to monitor; 0 = all
hw_enabled = true
sw_enabled = true
```

All fields default-safe; missing `[perf]` section behaves as `enabled = false`.

---

## 6. Testing strategy

- **Unit tests (no kernel access needed):** the `ingest(&[(name, value)])` function is extracted and tested with synthetic values. No `perf_event_open` call occurs.
- **Compile test on macOS:** `cargo build` must succeed; stubs ensure no Linux headers are needed.
- **Permission-degradation test:** unit test that calling `sample()` with a sampler pre-configured with `disabled = true` returns `Ok(())` immediately.
- **Live integration test** (`#[cfg(target_os = "linux")]`, skipped in CI unless running as root or with `CAP_PERFMON`): opens real counters, runs for 100 ms, asserts values increase.

---

## 7. Phase dependencies

Phase 4 (`nyquist-bpf`) will introduce a `Distribution` kind and `record_distribution()` on the registry. Phase 3 uses only `Counter` and the existing recording API — no core changes are needed.
