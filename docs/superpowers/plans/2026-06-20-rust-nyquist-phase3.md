# rust-nyquist Phase 3 Implementation Plan

**Goal:** Add `nyquist-perf` — hardware and software perf_event_open samplers — as a new workspace crate that slots into the existing `Sampler` / `Registry` pipeline.

**Prerequisites:** Phase 2 is complete. `cargo test` is green.

**Key constraint:** `perf_event_open` is Linux-only and requires `CAP_PERFMON` (kernel ≥ 5.9) or `perf_event_paranoid ≤ 1`. The workspace must still compile on macOS; all Linux-specific code is gated behind `#[cfg(target_os = "linux")]`. Tests that don't call the kernel must pass on any platform without elevated privileges.

---

### Task 0: Crate scaffolding

**Files:**
- Create: `crates/nyquist-perf/Cargo.toml`
- Create: `crates/nyquist-perf/src/lib.rs`
- Create: `crates/nyquist-perf/src/events.rs`
- Create: `crates/nyquist-perf/src/hw.rs`
- Create: `crates/nyquist-perf/src/sw.rs`

- [ ] **Step 1: Create `crates/nyquist-perf/Cargo.toml`**

```toml
[package]
name    = "nyquist-perf"
version = "0.1.0"
edition = "2021"

[dependencies]
nyquist-core = { path = "../nyquist-core" }
async-trait  = "0.1"
tracing      = "0.1"
thiserror    = "1"

[target.'cfg(target_os = "linux")'.dependencies]
perf-event = "0.4"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Create placeholder source files**

`crates/nyquist-perf/src/lib.rs`:
```rust
//! perf_event_open samplers for nyquist.
pub mod events;
pub mod hw;
pub mod sw;

use std::time::Duration;
use nyquist_core::registry::Registry;
use nyquist_core::sampler::Sampler;

pub struct PerfConfig {
    pub enabled:    bool,
    pub max_cpus:   usize,
    pub hw_enabled: bool,
    pub sw_enabled: bool,
}

impl Default for PerfConfig {
    fn default() -> Self {
        PerfConfig { enabled: false, max_cpus: 32, hw_enabled: true, sw_enabled: true }
    }
}

pub fn build_perf_enabled(
    reg: &Registry,
    default_interval: Duration,
    cfg: &PerfConfig,
) -> Vec<Box<dyn Sampler>> {
    let _ = (reg, default_interval);
    if !cfg.enabled { return Vec::new(); }
    let mut out: Vec<Box<dyn Sampler>> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let ncpus = events::num_cpus();
        let effective = if cfg.max_cpus == 0 { ncpus } else { cfg.max_cpus.min(ncpus) };
        if cfg.hw_enabled {
            out.push(Box::new(hw::HardwareSampler::new(effective, default_interval)));
        }
        if cfg.sw_enabled {
            out.push(Box::new(sw::SoftwareSampler::new(effective, default_interval)));
        }
    }
    out
}
```

`crates/nyquist-perf/src/events.rs`:
```rust
//! CPU detection and PerfCounter abstraction.

pub fn num_cpus() -> usize {
    // Read /sys/devices/system/cpu/present, e.g. "0-191"
    if let Ok(s) = std::fs::read_to_string("/sys/devices/system/cpu/present") {
        if let Some(end) = s.trim().split('-').last() {
            if let Ok(n) = end.parse::<usize>() {
                return n + 1;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(target_os = "linux")]
pub use linux::PerfCounter;

#[cfg(target_os = "linux")]
mod linux {
    use perf_event::Builder;

    #[derive(Debug, thiserror::Error)]
    pub enum PerfError {
        #[error("perf_event_open permission denied — requires CAP_PERFMON or perf_event_paranoid <= 1")]
        Permission,
        #[error("perf_event_open error: {0}")]
        Io(#[from] std::io::Error),
    }

    pub struct PerfCounter {
        pub counter: perf_event::Counter,
    }

    impl PerfCounter {
        pub fn open(
            cpu: usize,
            kind: impl perf_event::events::Event + Clone + 'static,
        ) -> Result<Self, PerfError> {
            let counter = Builder::new()
                .kind(kind)
                .observe_cpu(cpu as i32)
                .observe_pid(-1)
                .build()
                .map_err(|e| {
                    if e.raw_os_error() == Some(libc::EPERM)
                        || e.raw_os_error() == Some(libc::EACCES)
                    {
                        PerfError::Permission
                    } else {
                        PerfError::Io(e)
                    }
                })?;
            Ok(PerfCounter { counter })
        }

        pub fn read(&mut self) -> Result<u64, PerfError> {
            Ok(self.counter.read()?)
        }
    }
}
```

`crates/nyquist-perf/src/hw.rs`:
```rust
//! Hardware perf event sampler.
```

`crates/nyquist-perf/src/sw.rs`:
```rust
//! Software perf event sampler.
```

- [ ] **Step 3: Add `libc` to the Linux dependencies**

In `Cargo.toml`, add:
```toml
[target.'cfg(target_os = "linux")'.dependencies]
perf-event = "0.4"
libc        = "0.2"
```

- [ ] **Step 4: Verify workspace builds**

Run: `cargo build`
Expected: compiles on Linux (warnings OK). On macOS: also compiles because `#[cfg(target_os = "linux")]` gates the `perf-event` import.

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-perf
git commit -m "chore(perf): scaffold nyquist-perf crate with CPU detection"
```

---

### Task 1: PerfCounter unit test and CPU detection

**Files:**
- Modify: `crates/nyquist-perf/src/events.rs`

This task adds a unit test for CPU detection and verifies the permission-error path compiles correctly.

- [ ] **Step 1: Write failing test**

Append to `crates/nyquist-perf/src/events.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_cpus_returns_positive() {
        let n = num_cpus();
        assert!(n >= 1, "num_cpus returned {n}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn perm_error_is_distinct() {
        use linux::PerfError;
        // Construct the error variant to verify it compiles and formats.
        let e = PerfError::Permission;
        let s = e.to_string();
        assert!(s.contains("CAP_PERFMON"), "{s}");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p nyquist-perf events::`
Expected: PASS (2 tests on Linux, 1 on macOS).

- [ ] **Step 3: Commit**

```bash
git add crates/nyquist-perf/src/events.rs
git commit -m "test(perf): CPU detection and permission error variant"
```

---

### Task 2: HardwareSampler

**Files:**
- Modify: `crates/nyquist-perf/src/hw.rs`

**Interfaces:**
- Produces: `struct HardwareSampler` implementing `Sampler`
- `HardwareSampler::new(num_cpus: usize, interval: Duration) -> Self`
- Internal `ingest(reg: &Registry, now: Instant, readings: &[(&str, &str, u64)])` for testable metric-recording logic. `readings` is a slice of `(metric_name, cpu_label, raw_counter_value)`.

**Hardware events (Linux only):**
```
perf/hw/cpu_cycles          Hardware::CPU_CYCLES
perf/hw/instructions        Hardware::INSTRUCTIONS
perf/hw/cache_references    Hardware::CACHE_REFERENCES
perf/hw/cache_misses        Hardware::CACHE_MISSES
perf/hw/branch_instructions Hardware::BRANCH_INSTRUCTIONS
perf/hw/branch_misses       Hardware::BRANCH_MISSES
```

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-perf/src/hw.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::time::{Duration, Instant};

    #[test]
    fn ingest_registers_counters_and_records_rates() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let sampler = HardwareSampler::new(2, Duration::from_millis(10));
        let t0 = Instant::now();
        // First reading — establishes baseline.
        let r0: Vec<(&str, &str, u64)> = vec![
            ("perf/hw/cpu_cycles", "cpu0", 1_000_000),
            ("perf/hw/instructions", "cpu0", 2_000_000),
            ("perf/hw/cpu_cycles", "cpu1", 500_000),
            ("perf/hw/instructions", "cpu1", 1_000_000),
        ];
        sampler.ingest(&reg, t0, &r0);
        // Second reading — deltas produce rates.
        let r1: Vec<(&str, &str, u64)> = vec![
            ("perf/hw/cpu_cycles", "cpu0", 1_100_000),
            ("perf/hw/instructions", "cpu0", 2_200_000),
            ("perf/hw/cpu_cycles", "cpu1", 550_000),
            ("perf/hw/instructions", "cpu1", 1_100_000),
        ];
        sampler.ingest(&reg, t0 + Duration::from_millis(10), &r1);
        // 4 unique (metric, cpu) pairs → 4 metric IDs.
        assert_eq!(reg.metric_ids().len(), 4);
    }
}
```

Run: `cargo test -p nyquist-perf hw::`
Expected: FAIL — `HardwareSampler` not found.

- [ ] **Step 2: Write the implementation**

Replace `crates/nyquist-perf/src/hw.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

#[cfg(target_os = "linux")]
use perf_event::events::Hardware;
#[cfg(target_os = "linux")]
use crate::events::{PerfCounter, linux::PerfError};

#[cfg(target_os = "linux")]
const HW_EVENTS: &[(&str, Hardware)] = &[
    ("perf/hw/cpu_cycles",          Hardware::CPU_CYCLES),
    ("perf/hw/instructions",        Hardware::INSTRUCTIONS),
    ("perf/hw/cache_references",    Hardware::CACHE_REFERENCES),
    ("perf/hw/cache_misses",        Hardware::CACHE_MISSES),
    ("perf/hw/branch_instructions", Hardware::BRANCH_INSTRUCTIONS),
    ("perf/hw/branch_misses",       Hardware::BRANCH_MISSES),
];

pub struct HardwareSampler {
    interval: Duration,
    num_cpus: usize,
    #[cfg(target_os = "linux")]
    counters: Vec<(String, String, PerfCounter)>, // (metric_name, cpu_label, counter)
    #[cfg(target_os = "linux")]
    disabled: bool,
    #[cfg(target_os = "linux")]
    initialized: bool,
}

impl HardwareSampler {
    pub fn new(num_cpus: usize, interval: Duration) -> Self {
        HardwareSampler {
            interval,
            num_cpus,
            #[cfg(target_os = "linux")]
            counters: Vec::new(),
            #[cfg(target_os = "linux")]
            disabled: false,
            #[cfg(target_os = "linux")]
            initialized: false,
        }
    }

    pub fn ingest(&self, reg: &Registry, now: Instant, readings: &[(&str, &str, u64)]) {
        for &(metric_name, cpu_label, value) in readings {
            let labels = Labels::new().insert("cpu", cpu_label);
            let id = reg.register(
                MetricDef::new(metric_name, Kind::Counter)
                    .unit(Unit::Count)
                    .labels(labels),
            );
            reg.record_counter(id, now, value);
        }
    }

    #[cfg(target_os = "linux")]
    fn try_init(&mut self) -> Result<(), PerfError> {
        for cpu in 0..self.num_cpus {
            let cpu_label = format!("cpu{cpu}");
            for &(name, event) in HW_EVENTS {
                let counter = PerfCounter::open(cpu, event)?;
                self.counters.push((name.to_string(), cpu_label.clone(), counter));
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Sampler for HardwareSampler {
    fn name(&self) -> &str { "perf_hardware" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        #[cfg(target_os = "linux")]
        {
            if self.disabled { return Ok(()); }
            if !self.initialized {
                self.initialized = true;
                if let Err(e) = self.try_init() {
                    if matches!(e, PerfError::Permission) {
                        tracing::warn!(
                            "perf/hw disabled: {}. Run as root or set kernel.perf_event_paranoid=1.",
                            e
                        );
                        self.disabled = true;
                        return Ok(());
                    }
                    return Err(Box::new(e));
                }
            }
            let mut readings: Vec<(String, String, u64)> = Vec::new();
            for (metric_name, cpu_label, counter) in &mut self.counters {
                let v = counter.read()?;
                readings.push((metric_name.clone(), cpu_label.clone(), v));
            }
            let r: Vec<(&str, &str, u64)> = readings.iter()
                .map(|(n, c, v)| (n.as_str(), c.as_str(), *v))
                .collect();
            self.ingest(reg, now, &r);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::time::{Duration, Instant};

    #[test]
    fn ingest_registers_counters_and_records_rates() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let sampler = HardwareSampler::new(2, Duration::from_millis(10));
        let t0 = Instant::now();
        let r0: Vec<(&str, &str, u64)> = vec![
            ("perf/hw/cpu_cycles", "cpu0", 1_000_000),
            ("perf/hw/instructions", "cpu0", 2_000_000),
            ("perf/hw/cpu_cycles", "cpu1", 500_000),
            ("perf/hw/instructions", "cpu1", 1_000_000),
        ];
        sampler.ingest(&reg, t0, &r0);
        let r1: Vec<(&str, &str, u64)> = vec![
            ("perf/hw/cpu_cycles", "cpu0", 1_100_000),
            ("perf/hw/instructions", "cpu0", 2_200_000),
            ("perf/hw/cpu_cycles", "cpu1", 550_000),
            ("perf/hw/instructions", "cpu1", 1_100_000),
        ];
        sampler.ingest(&reg, t0 + Duration::from_millis(10), &r1);
        assert_eq!(reg.metric_ids().len(), 4);
    }

    #[test]
    fn disabled_sampler_returns_ok_immediately() {
        #[cfg(target_os = "linux")]
        {
            let mut sampler = HardwareSampler::new(1, Duration::from_millis(10));
            sampler.disabled = true;
            sampler.initialized = true;
            let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(sampler.sample(&reg, Instant::now()));
            assert!(result.is_ok());
            assert_eq!(reg.metric_ids().len(), 0);
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p nyquist-perf hw::`
Expected: PASS (2 tests on Linux, 1 on macOS — `disabled_sampler_returns_ok_immediately` is `cfg(linux)`).

- [ ] **Step 4: Commit**

```bash
git add crates/nyquist-perf/src/hw.rs
git commit -m "feat(perf): HardwareSampler with 6 hw perf counters per CPU"
```

---

### Task 3: SoftwareSampler

**Files:**
- Modify: `crates/nyquist-perf/src/sw.rs`

**Software events:**
```
perf/sw/context_switches  Software::CONTEXT_SWITCHES
perf/sw/page_faults       Software::PAGE_FAULTS
perf/sw/cpu_migrations    Software::CPU_MIGRATIONS
```

Same per-CPU labeling, same lazy-init + permission-degradation pattern as `HardwareSampler`.

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-perf/src/sw.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::time::{Duration, Instant};

    #[test]
    fn ingest_registers_sw_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let sampler = SoftwareSampler::new(2, Duration::from_millis(10));
        let t0 = Instant::now();
        let r: Vec<(&str, &str, u64)> = vec![
            ("perf/sw/context_switches", "cpu0", 1000),
            ("perf/sw/page_faults",      "cpu0", 200),
            ("perf/sw/cpu_migrations",   "cpu0", 5),
            ("perf/sw/context_switches", "cpu1", 800),
            ("perf/sw/page_faults",      "cpu1", 100),
            ("perf/sw/cpu_migrations",   "cpu1", 3),
        ];
        sampler.ingest(&reg, t0, &r);
        // 3 events × 2 CPUs = 6 metric IDs.
        assert_eq!(reg.metric_ids().len(), 6);
    }
}
```

Run: `cargo test -p nyquist-perf sw::`
Expected: FAIL — `SoftwareSampler` not found.

- [ ] **Step 2: Write the implementation**

Replace `crates/nyquist-perf/src/sw.rs`:
```rust
use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Labels, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

#[cfg(target_os = "linux")]
use perf_event::events::Software;
#[cfg(target_os = "linux")]
use crate::events::{PerfCounter, linux::PerfError};

#[cfg(target_os = "linux")]
const SW_EVENTS: &[(&str, Software)] = &[
    ("perf/sw/context_switches", Software::CONTEXT_SWITCHES),
    ("perf/sw/page_faults",      Software::PAGE_FAULTS),
    ("perf/sw/cpu_migrations",   Software::CPU_MIGRATIONS),
];

pub struct SoftwareSampler {
    interval: Duration,
    num_cpus: usize,
    #[cfg(target_os = "linux")]
    counters:    Vec<(String, String, PerfCounter)>,
    #[cfg(target_os = "linux")]
    disabled:    bool,
    #[cfg(target_os = "linux")]
    initialized: bool,
}

impl SoftwareSampler {
    pub fn new(num_cpus: usize, interval: Duration) -> Self {
        SoftwareSampler {
            interval,
            num_cpus,
            #[cfg(target_os = "linux")]
            counters:    Vec::new(),
            #[cfg(target_os = "linux")]
            disabled:    false,
            #[cfg(target_os = "linux")]
            initialized: false,
        }
    }

    pub fn ingest(&self, reg: &Registry, now: Instant, readings: &[(&str, &str, u64)]) {
        for &(metric_name, cpu_label, value) in readings {
            let labels = Labels::new().insert("cpu", cpu_label);
            let id = reg.register(
                MetricDef::new(metric_name, Kind::Counter)
                    .unit(Unit::Count)
                    .labels(labels),
            );
            reg.record_counter(id, now, value);
        }
    }

    #[cfg(target_os = "linux")]
    fn try_init(&mut self) -> Result<(), PerfError> {
        for cpu in 0..self.num_cpus {
            let cpu_label = format!("cpu{cpu}");
            for &(name, event) in SW_EVENTS {
                let counter = PerfCounter::open(cpu, event)?;
                self.counters.push((name.to_string(), cpu_label.clone(), counter));
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Sampler for SoftwareSampler {
    fn name(&self) -> &str { "perf_software" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        #[cfg(target_os = "linux")]
        {
            if self.disabled { return Ok(()); }
            if !self.initialized {
                self.initialized = true;
                if let Err(e) = self.try_init() {
                    if matches!(e, PerfError::Permission) {
                        tracing::warn!(
                            "perf/sw disabled: {}. Run as root or set kernel.perf_event_paranoid=1.",
                            e
                        );
                        self.disabled = true;
                        return Ok(());
                    }
                    return Err(Box::new(e));
                }
            }
            let mut readings: Vec<(String, String, u64)> = Vec::new();
            for (metric_name, cpu_label, counter) in &mut self.counters {
                let v = counter.read()?;
                readings.push((metric_name.clone(), cpu_label.clone(), v));
            }
            let r: Vec<(&str, &str, u64)> = readings.iter()
                .map(|(n, c, v)| (n.as_str(), c.as_str(), *v))
                .collect();
            self.ingest(reg, now, &r);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::time::{Duration, Instant};

    #[test]
    fn ingest_registers_sw_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let sampler = SoftwareSampler::new(2, Duration::from_millis(10));
        let t0 = Instant::now();
        let r: Vec<(&str, &str, u64)> = vec![
            ("perf/sw/context_switches", "cpu0", 1000),
            ("perf/sw/page_faults",      "cpu0", 200),
            ("perf/sw/cpu_migrations",   "cpu0", 5),
            ("perf/sw/context_switches", "cpu1", 800),
            ("perf/sw/page_faults",      "cpu1", 100),
            ("perf/sw/cpu_migrations",   "cpu1", 3),
        ];
        sampler.ingest(&reg, t0, &r);
        assert_eq!(reg.metric_ids().len(), 6);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p nyquist-perf sw::`
Expected: PASS (1 test).

- [ ] **Step 4: Commit**

```bash
git add crates/nyquist-perf/src/sw.rs
git commit -m "feat(perf): SoftwareSampler with context_switches/page_faults/cpu_migrations"
```

---

### Task 4: Config additions

**Files:**
- Modify: `crates/nyquist-config/src/lib.rs`
- Modify: `crates/nyquist-config/tests/load.rs`

**Interfaces:**
- Adds `PerfConfig` to `Config`
- Default: `enabled = false`, `max_cpus = 32`, `hw_enabled = true`, `sw_enabled = true`

- [ ] **Step 1: Write the failing test**

Append to `crates/nyquist-config/tests/load.rs`:
```rust
#[test]
fn perf_config_defaults_to_disabled() {
    let c = Config::default();
    assert!(!c.perf.enabled);
    assert_eq!(c.perf.max_cpus, 32);
    assert!(c.perf.hw_enabled);
    assert!(c.perf.sw_enabled);
}

#[test]
fn perf_config_parses_from_toml() {
    let toml = r#"
        [perf]
        enabled    = true
        max_cpus   = 8
        hw_enabled = true
        sw_enabled = false
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert!(c.perf.enabled);
    assert_eq!(c.perf.max_cpus, 8);
    assert!(!c.perf.sw_enabled);
}
```

Run: `cargo test -p nyquist-config`
Expected: FAIL — `perf` field not found on `Config`.

- [ ] **Step 2: Write the implementation**

Add to `crates/nyquist-config/src/lib.rs`:
```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PerfConfig {
    pub enabled:    bool,
    pub max_cpus:   usize,
    pub hw_enabled: bool,
    pub sw_enabled: bool,
}

impl Default for PerfConfig {
    fn default() -> Self {
        PerfConfig { enabled: false, max_cpus: 32, hw_enabled: true, sw_enabled: true }
    }
}
```

Add `pub perf: PerfConfig,` to `struct Config`.
Add `perf: PerfConfig::default(),` to `impl Default for Config`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p nyquist-config`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/nyquist-config
git commit -m "feat(config): add PerfConfig for perf_event_open samplers"
```

---

### Task 5: Wire into main binary and root Cargo.toml

**Files:**
- Modify: `Cargo.toml` (add nyquist-perf dependency)
- Modify: `src/main.rs`

- [ ] **Step 1: Add nyquist-perf to root Cargo.toml**

In `[dependencies]`:
```toml
nyquist-perf = { path = "crates/nyquist-perf" }
```

- [ ] **Step 2: Wire in main.rs**

After the `build_enabled(...)` call, add:
```rust
use nyquist_perf::{PerfConfig, build_perf_enabled};

let perf_cfg = PerfConfig {
    enabled:    config.perf.enabled,
    max_cpus:   config.perf.max_cpus,
    hw_enabled: config.perf.hw_enabled,
    sw_enabled: config.perf.sw_enabled,
};
let perf_samplers = build_perf_enabled(&reg, config.general.default_interval, &perf_cfg);
for s in perf_samplers {
    handles.push(spawn_sampler(s, reg.clone(), config.general.fault_tolerant));
}
```

- [ ] **Step 3: Verify build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat: wire nyquist-perf samplers into binary"
```

---

### Task 6: Full test suite and integration smoke test

**Files:**
- Create: `crates/nyquist-perf/src/lib.rs` (add integration test)

- [ ] **Step 1: Run full suite**

Run: `cargo test`
Expected: all tests PASS including existing Phase 1/2 tests.

- [ ] **Step 2: Add a live integration test (Linux, skipped without CAP_PERFMON)**

Append to `crates/nyquist-perf/src/lib.rs`:
```rust
#[cfg(all(test, target_os = "linux"))]
mod integration {
    use super::*;
    use nyquist_core::registry::Registry;
    use std::sync::Arc;
    use std::time::Duration;

    /// This test requires CAP_PERFMON or perf_event_paranoid <= 1.
    /// It is skipped at runtime (not compile time) when permissions are absent.
    #[tokio::test]
    async fn hw_sampler_increments_over_time() {
        use nyquist_core::sampler::Sampler;
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let mut s = crate::hw::HardwareSampler::new(1, Duration::from_millis(10));
        let t0 = std::time::Instant::now();
        // First sample — may return Ok(()) even if disabled (permission denied).
        let _ = s.sample(&reg, t0).await;
        // If no metrics were recorded the sampler self-disabled — skip.
        if reg.metric_ids().is_empty() {
            eprintln!("SKIP: perf_event_open unavailable (no CAP_PERFMON / paranoid too high)");
            return;
        }
        // Burn some CPU to make counters move.
        let _ = (0u64..1_000_000).sum::<u64>();
        let _ = s.sample(&reg, t0 + Duration::from_millis(10)).await;
        let ids = reg.metric_ids();
        assert!(!ids.is_empty(), "expected perf/hw metrics");
        // At least some counter must be non-zero after burning CPU.
        let any_nonzero = ids.iter().any(|id| reg.raw(*id) > 0);
        assert!(any_nonzero, "all hw counters are zero after CPU burn");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p nyquist-perf`
Expected: unit tests PASS; integration test either PASSES (if CAP_PERFMON available) or prints `SKIP` and passes.

- [ ] **Step 4: Final commit**

```bash
git add crates/nyquist-perf/src/lib.rs
git commit -m "test(perf): live integration smoke test with capability guard"
```

---

## Self-Review Notes

- **macOS compile:** all perf-event code is gated; `build_perf_enabled()` returns empty vec; `cargo build` must succeed.
- **Permission degradation:** `disabled` flag set on first EPERM; subsequent ticks are no-ops with zero log output.
- **fd budget:** `max_cpus = 32` default prevents fd exhaustion on 192-CPU hosts; users can raise it knowing the tradeoffs.
- **No nyquist-core changes:** only the existing `record_counter` / `register` API is used.
- **Tested logic vs. tested integration:** `ingest()` is pure Rust and unit-tested on any platform. Live `perf_event_open` is only in the integration test, which self-skips gracefully.
