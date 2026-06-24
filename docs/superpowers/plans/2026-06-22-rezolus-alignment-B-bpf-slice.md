# Rezolus Alignment — Plan B: libbpf-rs Toolchain + TCP Reference Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Execution environment:** This plan builds and runs eBPF. It must be executed on a **Linux** host (or devcontainer) with `clang` (BPF target), `libbpf` headers, and `bpftool`. The dev Mac cannot compile or load BPF. Pure-Rust steps still compile on macOS behind `#[cfg(target_os = "linux")]` gates.
>
> **Depends on Plan A** (`2026-06-22-rezolus-alignment-A-foundation.md`): the `linkme` `SAMPLERS` slice and `SamplerEntry` in `nyquist-core::registration`, and the bucket-array `MetricSnapshot`. Do Plan A first.

**Goal:** Replace the Aya BPF toolchain with `libbpf-rs` + CO-RE C programs, and prove the rezolus model end-to-end by converting the TCP sampler to an in-kernel H2 histogram (RTT) + mmap'd counter (retransmits), deleting the 4 MB ring buffer and 1-in-128 sampling (design §3.1–3.3, §5).

**Architecture:** A `build.rs` using `libbpf_cargo::SkeletonBuilder` compiles each sampler's `mod.bpf.c` into a skeleton under `$OUT_DIR`, mirroring rezolus (`/Users/gmckee/projects/rezolus/build.rs`). BPF code uses CO-RE against checked-in per-arch `vmlinux.h`. Counters and histograms live in `BPF_MAP_TYPE_ARRAY` maps with `BPF_F_MMAPABLE`; userspace reads them via mmap (zero syscalls per refresh) and records into the existing `Registry` through a new direct-bucket seam. Samplers register via the `linkme` `SAMPLERS` slice from Plan A.

**Tech Stack:** Rust (stable), `libbpf-rs = "0.24"`, `libbpf-cargo = "0.24"` (build-dep), `memmap2`, CO-RE C (`clang`), `linkme`. **Pin the libbpf-rs/libbpf-cargo minor versions together** — the generated skeleton API is version-specific.

## Global Constraints

- **Linux-only BPF, gated.** All BPF loading/mmap code is behind `#[cfg(target_os = "linux")]`. The crate compiles on macOS to a no-op so `cargo build` works there; BPF tests are `#[cfg(target_os = "linux")]` + `#[ignore]`-gated on missing privileges.
- **`grouping_power = 3` for in-kernel histograms** (~6.25% max relative error, 496 buckets) — design §3.1. This differs from the windowed path's `7`; keep them separate.
- **License/provenance (design §8).** rezolus is dual MIT/Apache-2.0; its `*.bpf.c` files are `SPDX-License-Identifier: GPL-2.0`. Ported C files keep the GPL-2.0 SPDX line and a `// Adapted from rezolus <path> (MIT OR Apache-2.0); original BCC attribution retained.` NOTICE. Record attribution in a top-level `NOTICE` file before copying any file. `vmlinux.h` is a mechanical BTF dump (no copyright concern) but note its origin + kernel version.
- **`vmlinux.h` is version-pinned and per-arch**, checked in at `crates/nyquist-ebpf/bpf/{x86_64,aarch64}/vmlinux.h`. Never silently regenerated; updates are deliberate, per-arch, and noted in the commit.
- **Zero coverage regression (design §6).** The converted TCP samplers must still emit today's `tcp` RTT + retransmit tuples. Run Plan A's parity gate with a live `/metrics` capture (`NYQUIST_LIVE_METRICS`) before deleting the Aya sampler. The Aya path is removed only once parity is verified (design §6.3).
- **Frequent commits**; each task ends green on the Linux host (`cargo test --workspace` + the BPF build test).

---

### Task B0: Workspace surgery — scaffold the libbpf-rs crate, keep the build green

Replace the three Aya crates with one libbpf-rs-based `nyquist-ebpf` crate that compiles to a no-op stub. The TCP samplers come in B4/B5; this task just makes the workspace build with the new crate skeleton and the Aya wiring still functional behind it (we delete Aya only in B9, after parity is proven).

**Files:**
- Modify: `Cargo.toml` (workspace: stop excluding `nyquist-ebpf-programs`; we will remove all three Aya crates in B9 — for now leave them but stop building the new crate against them)
- Delete (deferred to B9): `crates/nyquist-ebpf-programs/`, `crates/nyquist-ebpf-common/`
- Replace: `crates/nyquist-ebpf/` contents (new libbpf-rs crate). Keep the crate name `nyquist-ebpf` (design §3.2 default).
- Create: `crates/nyquist-ebpf/Cargo.toml`, `crates/nyquist-ebpf/src/lib.rs`

> Strategy: build the new crate as `nyquist-ebpf2` is **not** used — we keep the name `nyquist-ebpf` but the current Aya code in it must be preserved until B9. To avoid a half-broken crate, do B0–B8 in a **new** module path inside the crate and flip `main.rs` from the Aya sampler to the libbpf samplers only in B7, deleting Aya code in B9. Concretely: add `src/libbpf/` submodule alongside the existing `src/sampler.rs`.

- [ ] **Step 1: Add libbpf deps to `nyquist-ebpf`**

In `crates/nyquist-ebpf/Cargo.toml`, add (keep the existing `aya` dep for now):
```toml
[target.'cfg(target_os = "linux")'.dependencies]
libbpf-rs = "0.24"
memmap2 = "0.9"
linkme = "0.3"

[target.'cfg(target_os = "linux")'.build-dependencies]
libbpf-cargo = "0.24"
```
Add `linkme = "0.3"` and `nyquist-core = { path = "../nyquist-core" }` (already present) to the main `[dependencies]` (linkme is needed on all targets for the registration entry).

- [ ] **Step 2: Create the no-op module skeleton**

Create `crates/nyquist-ebpf/src/libbpf/mod.rs`:
```rust
//! libbpf-rs based BPF samplers (design §3). Linux-only; a no-op elsewhere.
#[cfg(target_os = "linux")]
pub mod h2;
#[cfg(target_os = "linux")]
pub mod tcp;
```
Add to `crates/nyquist-ebpf/src/lib.rs`:
```rust
pub mod libbpf;
```

- [ ] **Step 3: Verify the workspace still builds on this host**

Run: `cargo build --workspace`
Expected: PASS (empty module). On Linux, also confirm `clang --version` and `bpftool version` succeed (prerequisites for later tasks).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/nyquist-ebpf/Cargo.toml crates/nyquist-ebpf/src/lib.rs crates/nyquist-ebpf/src/libbpf/mod.rs
git commit -m "chore(ebpf): scaffold libbpf-rs module alongside Aya (no-op)"
```

---

### Task B1: Vendor shared BPF headers + per-arch vmlinux.h + attribution

**Files:**
- Create: `NOTICE` (attribution)
- Create: `crates/nyquist-ebpf/bpf/histogram.h` (from `/Users/gmckee/projects/rezolus/src/agent/bpf/histogram.h`)
- Create: `crates/nyquist-ebpf/bpf/helpers.h` (from `/Users/gmckee/projects/rezolus/src/agent/bpf/helpers.h`)
- Create: `crates/nyquist-ebpf/bpf/x86_64/vmlinux.h` (from rezolus x86_64)
- Create: `crates/nyquist-ebpf/bpf/aarch64/vmlinux.h` (from rezolus aarch64)

- [ ] **Step 1: Record attribution**

Create `NOTICE`:
```
This product includes BPF C headers and program logic adapted from Rezolus
(https://github.com/iopsystems/rezolus), licensed under MIT OR Apache-2.0.
The adapted *.bpf.c programs are SPDX-License-Identifier: GPL-2.0, retaining
the original BCC project attribution where applicable. vmlinux.h files are
mechanical BTF dumps; their source kernel version is noted in each file header.
```

- [ ] **Step 2: Copy the shared headers verbatim**

```bash
mkdir -p crates/nyquist-ebpf/bpf/x86_64 crates/nyquist-ebpf/bpf/aarch64
cp /Users/gmckee/projects/rezolus/src/agent/bpf/histogram.h crates/nyquist-ebpf/bpf/histogram.h
cp /Users/gmckee/projects/rezolus/src/agent/bpf/helpers.h   crates/nyquist-ebpf/bpf/helpers.h
```
Prepend to each a comment: `// Adapted from rezolus src/agent/bpf/<name> (MIT OR Apache-2.0).` Confirm `histogram.h` defines `HISTOGRAM_BUCKETS_POW_3 496` and `value_to_index(value, grouping_power)`, and `helpers.h` defines `array_add`, `histogram_incr`, `array_set_if_larger`.

- [ ] **Step 3: Copy per-arch vmlinux.h (pinned)**

```bash
cp /Users/gmckee/projects/rezolus/src/agent/bpf/x86_64/vmlinux.h  crates/nyquist-ebpf/bpf/x86_64/vmlinux.h
cp /Users/gmckee/projects/rezolus/src/agent/bpf/aarch64/vmlinux.h crates/nyquist-ebpf/bpf/aarch64/vmlinux.h
```
Note in the commit message the rezolus commit / kernel version these came from. (Alternatively regenerate on the Linux host: `bpftool btf dump file /sys/kernel/btf/vmlinux format c > .../<arch>/vmlinux.h` — but prefer the rezolus-pinned copy so CO-RE field sets match the tested samplers.)

- [ ] **Step 4: Commit**

```bash
git add NOTICE crates/nyquist-ebpf/bpf/
git commit -m "feat(ebpf): vendor rezolus histogram.h/helpers.h + per-arch vmlinux.h with attribution"
```

---

### Task B2: `build.rs` with `libbpf_cargo::SkeletonBuilder`

Compile each sampler's `mod.bpf.c` into a skeleton under `$OUT_DIR`, mirroring rezolus's `build.rs` (`/Users/gmckee/projects/rezolus/build.rs`, the `bpf` module).

**Files:**
- Create: `crates/nyquist-ebpf/build.rs` (replaces the Aya-invoking build.rs; the old one stays until B9 — write the new logic guarded so both can't run. Simplest: replace `build.rs` now since the Aya ELF is loaded by `src/sampler.rs` via `include_bytes_aligned!(OUT_DIR/...)`. **Keep the Aya build path** by merging: the new `build.rs` does BOTH the Aya invocation (unchanged, until B9) AND the skeleton generation. To avoid that complexity, gate skeleton generation to run and leave the Aya invocation intact.)

> Decision: keep it simple — the new `build.rs` runs the skeleton generation for the libbpf samplers AND preserves the existing Aya cargo-invocation block verbatim until B9. Read the current `crates/nyquist-ebpf/build.rs` and append the skeleton block.

- [ ] **Step 1: Add the skeleton-generation block to `build.rs`**

Append to `crates/nyquist-ebpf/build.rs` a Linux-gated function and call it from `main`:
```rust
#[cfg(target_os = "linux")]
fn generate_skeletons() {
    use libbpf_cargo::SkeletonBuilder;
    use std::path::PathBuf;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bpf_dir = manifest.join("bpf");

    // Per-arch include dir + target macro (mirrors rezolus build.rs).
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let (arch_dir, arch_def) = match arch.as_str() {
        "x86_64" => ("x86_64", "-D__TARGET_ARCH_x86"),
        "aarch64" => ("aarch64", "-D__TARGET_ARCH_arm64"),
        other => panic!("unsupported BPF arch: {other}"),
    };
    let arch_inc = bpf_dir.join(arch_dir);

    // (sampler dir name, generated skeleton stem) pairs.
    let samplers = [
        ("tcp/packet_latency", "tcp_packet_latency"),
        ("tcp/retransmit", "tcp_retransmit"),
    ];

    for (dir, stem) in samplers {
        let src = manifest.join("src/libbpf").join(dir).join("mod.bpf.c");
        let dst = out_dir.join(format!("{stem}.bpf.rs"));
        println!("cargo:rerun-if-changed={}", src.display());
        SkeletonBuilder::new()
            .source(&src)
            .clang_args([
                format!("-I{}", arch_inc.display()),
                format!("-I{}", bpf_dir.display()),
                arch_def.to_string(),
                "-fno-unwind-tables".to_string(),
                "-Wall".to_string(),
                "-Werror".to_string(),
            ])
            .build_and_generate(&dst)
            .unwrap_or_else(|e| panic!("skeleton build failed for {dir}: {e}"));
    }
    println!("cargo:rerun-if-changed={}", bpf_dir.display());
}

#[cfg(not(target_os = "linux"))]
fn generate_skeletons() {}
```
Call `generate_skeletons();` at the end of `main()`. The generated `*.bpf.rs` are `include!`d by each sampler's `mod.rs` (B4/B5). The `vmlinux.h` is found via the `-I{arch_inc}` include path (each `mod.bpf.c` does `#include <vmlinux.h>`).

- [ ] **Step 2: Confirm prerequisites and that the (empty) build still passes**

This task adds no `.bpf.c` yet, so the `samplers` loop will fail on missing source. Temporarily set `let samplers: [(&str,&str); 0] = [];` until B4 adds the first source, OR order execution so B4/B5 land the `.bpf.c` before running. Recommended: leave the array as above but create the `.bpf.c` files in B4/B5 first. For this task's verification, set the array empty, build, then restore in B4.

Run (Linux): `cargo build -p nyquist-ebpf`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/nyquist-ebpf/build.rs
git commit -m "feat(ebpf): add libbpf-cargo SkeletonBuilder build.rs (per-arch CO-RE)"
```

---

### Task B3: H2 bucket → snapshot conversion (Rust, testable on any OS)

The in-kernel histogram is a 496-entry `u64` array indexed by `value_to_index(value, 3)`. Userspace converts it to the sparse `(upper_bound, count)` array `MetricSnapshot.buckets` expects (Plan A). Use the `histogram` crate to reconstruct bucket bounds, matching rezolus (`RwLockHistogram::new(grouping_power, 64)`).

**Files:**
- Create: `crates/nyquist-ebpf/src/libbpf/h2.rs`

**Interfaces:**
- Produces: `pub const BPF_GROUPING_POWER: u8 = 3; pub const BPF_MAX_VALUE_POWER: u8 = 64; pub const BPF_BUCKETS: usize = 496;`
- Produces: `pub fn buckets_from_counts(counts: &[u64]) -> Vec<(u64, u64)>` — sparse `(upper_bound, count)` ascending, for the snapshot.

- [ ] **Step 1: Verify the `histogram` crate reconstruction API (Linux, online)**

Run: `cargo doc -p histogram --no-deps` and confirm a constructor exists that builds a `Histogram` from raw bucket counts at a given `(grouping_power, max_value_power)` (rezolus uses this to load BPF arrays). Likely `Histogram::from_buckets(grouping_power, max_value_power, Vec<u64>)`. Record the exact name; it's used once below.

- [ ] **Step 2: Write the failing test**

Create `crates/nyquist-ebpf/src/libbpf/h2.rs`:
```rust
//! Convert an in-kernel H2 histogram (grouping_power=3, 496 buckets) into the
//! sparse `(upper_bound, count)` array used by `MetricSnapshot` (design §3.4).
use histogram::Histogram;

pub const BPF_GROUPING_POWER: u8 = 3;
pub const BPF_MAX_VALUE_POWER: u8 = 64;
pub const BPF_BUCKETS: usize = 496;

/// Reconstruct sparse `(upper_bound, count)` pairs from the raw mmap'd counts.
pub fn buckets_from_counts(counts: &[u64]) -> Vec<(u64, u64)> {
    // from_buckets requires exactly the configured bucket count.
    let mut v = counts.to_vec();
    v.resize(BPF_BUCKETS, 0);
    let h = Histogram::from_buckets(BPF_GROUPING_POWER, BPF_MAX_VALUE_POWER, v)
        .expect("valid H2 config");
    (&h)
        .into_iter()
        .filter(|b| b.count() > 0)
        .map(|b| (b.end(), b.count()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_count_matches_pow3() {
        // index 0..(2<<3)=16 is the linear region; counts there map to value==index.
        let mut counts = vec![0u64; BPF_BUCKETS];
        counts[5] = 10;   // value 5
        counts[12] = 3;   // value 12
        let buckets = buckets_from_counts(&counts);
        let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
        assert_eq!(total, 13);
        // Ascending bounds, non-empty only.
        assert!(buckets.iter().all(|&(_, c)| c > 0));
        let bounds: Vec<u64> = buckets.iter().map(|&(b, _)| b).collect();
        let mut sorted = bounds.clone(); sorted.sort_unstable();
        assert_eq!(bounds, sorted);
    }
}
```
(If Step 1 found a different constructor name/signature, adjust the single `Histogram::from_buckets(...)` call.)

- [ ] **Step 3: Run the test**

Run: `cargo test -p nyquist-ebpf h2::`
Expected: PASS. (`h2.rs` is Linux-gated in `mod.rs`; run on the Linux host. If you must check on macOS, temporarily un-gate for the test.)

- [ ] **Step 4: Commit**

```bash
git add crates/nyquist-ebpf/src/libbpf/h2.rs
git commit -m "feat(ebpf): H2 bucket-array to sparse snapshot conversion (grouping_power=3)"
```

---

### Task B6 (do before B4/B5): Registry direct-bucket seam

BPF histograms are not windowed — their buckets come straight from the kernel. Add a Registry path that stores a metric's buckets directly, and have `snapshot` prefer them. (Numbered B6 in the design's mental model but sequenced here because B4/B5 depend on it.)

**Files:**
- Modify: `crates/nyquist-core/src/registry.rs` (add `direct_buckets` to `MetricState`; add `record_distribution_buckets`; prefer direct buckets in `snapshot`)

**Interfaces:**
- Produces: `Registry::record_distribution_buckets(&self, id: MetricId, buckets: Vec<(u64, u64)>)`

- [ ] **Step 1: Write the failing test**

Add to `crates/nyquist-core/src/registry.rs` tests:
```rust
    #[test]
    fn direct_buckets_appear_in_snapshot() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let id = reg.register(MetricDef::new("tcp/packet_latency", Kind::Distribution));
        reg.record_distribution_buckets(id, vec![(100, 5), (1000, 2)]);
        let snap = reg.snapshot(Instant::now());
        let m = snap.metrics.iter().find(|m| m.name == "tcp/packet_latency").unwrap();
        assert_eq!(m.buckets, vec![(100, 5), (1000, 2)]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p nyquist-core direct_buckets_appear_in_snapshot`
Expected: FAIL — no method `record_distribution_buckets`.

- [ ] **Step 3: Implement the seam**

In `crates/nyquist-core/src/registry.rs`:
- Add a field to `MetricState` (line ~25): `direct_buckets: Option<Vec<(u64, u64)>>,` and initialize `direct_buckets: None` in `register` (line ~48).
- Add the method:
```rust
    /// Store an externally-aggregated H2 bucket array directly (BPF path,
    /// design §3.4). Bypasses the windowing engine — these buckets come from
    /// the kernel and are exposed as-is.
    pub fn record_distribution_buckets(&self, id: MetricId, buckets: Vec<(u64, u64)>) {
        if let Some(state) = self.metrics.get(&id) {
            let mut s = state.lock().unwrap();
            let total: u64 = buckets.iter().map(|&(_, c)| c).sum();
            s.raw = total;
            s.direct_buckets = Some(buckets);
        }
    }
```
- In `snapshot`, prefer direct buckets:
```rust
            let buckets = match &s.direct_buckets {
                Some(b) => b.clone(),
                None => s.window.bucket_counts(now),
            };
```

- [ ] **Step 4: Run to verify pass + full core suite**

Run: `cargo test -p nyquist-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-core/src/registry.rs
git commit -m "feat(core): add direct-bucket Registry seam for in-kernel histograms (design §4)"
```

---

### Task B4: `tcp/packet_latency` BPF sampler (RTT)

Port rezolus's `tcp_packet_latency` (`/Users/gmckee/projects/rezolus/src/agent/samplers/tcp/linux/packet_latency/`). In-kernel RTT histogram via `tcp_probe` + `tcp_rcv_space_adjust` + `tcp_destroy_sock`; per-socket state keyed by `struct sock*` in a `HASH` (the documented pointer-key exception, design §5.1); `BPF_F_MMAPABLE` H2 histogram. No ring buffer, no sampling.

**Files:**
- Create: `crates/nyquist-ebpf/src/libbpf/tcp/mod.rs` (`pub mod packet_latency; pub mod retransmit;`)
- Create: `crates/nyquist-ebpf/src/libbpf/tcp/packet_latency/mod.bpf.c`
- Create: `crates/nyquist-ebpf/src/libbpf/tcp/packet_latency/mod.rs`
- Modify: `crates/nyquist-ebpf/build.rs` (restore the `samplers` array entry for `tcp/packet_latency`)

**Interfaces:**
- Registers `SamplerEntry { name: "tcp/packet_latency", init }` into `nyquist_core::registration::SAMPLERS`.
- Emits metric `tcp/rtt_us` (`Kind::Distribution`) via `record_distribution_buckets`. (Name chosen to match the existing `tcpinfo` `tcp/rtt_us` tuple for parity — see Coverage note below.)

- [ ] **Step 1: Write `mod.bpf.c`**

Create `crates/nyquist-ebpf/src/libbpf/tcp/packet_latency/mod.bpf.c` (adapted from rezolus; note the histogram records nanoseconds):
```c
// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/tcp/linux/packet_latency/mod.bpf.c
// (MIT OR Apache-2.0); based on tcppktlat.bpf.c from the BCC project.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_ENTRIES 10240
#define NO_EXIST 1

// Per-socket entry timestamp. Key is the u64 cast of `struct sock*` — the
// documented pointer-key exception (design §5.1, rezolus principle 5): there is
// no bounded integer index for a live socket, so a HASH keyed by sock* is used.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, u64);
    __type(value, u64);
} start SEC(".maps");

// In-kernel H2 histogram of latency (ns). BPF_F_MMAPABLE: userspace reads via mmap.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} latency SEC(".maps");

static __always_inline u64 sock_ident(struct sock *sk) { return (u64)sk; }

static int handle_tcp_probe(struct sock *sk, struct sk_buff *skb) {
    const struct tcphdr *th = (const struct tcphdr *)BPF_CORE_READ(skb, data);
    u64 doff = BPF_CORE_READ_BITFIELD_PROBED(th, doff);
    u64 len = BPF_CORE_READ(skb, len);
    if (len <= doff * 4) return 0; // pure ACK, no data
    u64 id = sock_ident(sk), ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&start, &id, &ts, NO_EXIST);
    return 0;
}

static int handle_rcv_space_adjust(struct sock *sk) {
    u64 id = sock_ident(sk);
    u64 *tsp = bpf_map_lookup_elem(&start, &id);
    if (!tsp) return 0;
    u64 now = bpf_ktime_get_ns();
    if (*tsp <= now) {
        histogram_incr(&latency, HISTOGRAM_POWER, now - *tsp);
    }
    bpf_map_delete_elem(&start, &id);
    return 0;
}

static int handle_destroy_sock(struct sock *sk) {
    u64 id = sock_ident(sk);
    bpf_map_delete_elem(&start, &id);
    return 0;
}

SEC("raw_tp/tcp_probe")
int BPF_PROG(tcp_probe, struct sock *sk, struct sk_buff *skb) { return handle_tcp_probe(sk, skb); }

SEC("raw_tp/tcp_rcv_space_adjust")
int BPF_PROG(tcp_rcv_space_adjust, struct sock *sk) { return handle_rcv_space_adjust(sk); }

SEC("raw_tp/tcp_destroy_sock")
int BPF_PROG(tcp_destroy_sock, struct sock *sk) { return handle_destroy_sock(sk); }

char LICENSE[] SEC("license") = "GPL";
```

- [ ] **Step 2: Restore the build.rs sampler entry and compile the skeleton**

In `crates/nyquist-ebpf/build.rs`, ensure the `samplers` array contains `("tcp/packet_latency", "tcp_packet_latency")`.
Run (Linux): `cargo build -p nyquist-ebpf`
Expected: PASS — `$OUT_DIR/tcp_packet_latency.bpf.rs` is generated. If clang errors on a CO-RE field, confirm the `vmlinux.h` arch matches the host and that `bpf_core_read` macros resolve.

- [ ] **Step 3: Write the Rust sampler `mod.rs`**

Create `crates/nyquist-ebpf/src/libbpf/tcp/mod.rs`:
```rust
pub mod packet_latency;
pub mod retransmit;
```
Create `crates/nyquist-ebpf/src/libbpf/tcp/packet_latency/mod.rs`. This loads the skeleton, attaches the three programs, mmaps the `latency` array, and on each `sample()` reconstructs buckets and records them. The skeleton API below targets **libbpf-rs 0.24** (field-style `skel.maps.latency`, `skel.progs.tcp_probe`); adjust if the pinned version differs.
```rust
const NAME: &str = "tcp/packet_latency";
const METRIC: &str = "tcp/rtt_us";

mod skel {
    include!(concat!(env!("OUT_DIR"), "/tcp_packet_latency.bpf.rs"));
}

use std::time::{Duration, Instant};
use std::os::fd::AsRawFd;
use async_trait::async_trait;
use memmap2::Mmap;
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use nyquist_core::model::{Kind, Unit, Labels};
use crate::libbpf::h2::{buckets_from_counts, BPF_BUCKETS};
use skel::*;

enum State { Uninit, Disabled, Running { _skel: TcpPacketLatencySkel<'static>, mmap: Mmap, _links: Vec<libbpf_rs::Link> } }

pub struct PacketLatency { interval: Duration, state: State, metric_id: Option<nyquist_core::model::MetricId> }

impl PacketLatency {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        let id = reg.register(
            MetricDef::new(METRIC, Kind::Distribution).unit(Unit::None),
        );
        PacketLatency { interval, state: State::Uninit, metric_id: Some(id) }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use libbpf_rs::skel::{OpenSkel, SkelBuilder};
        let builder = TcpPacketLatencySkelBuilder::default();
        let open = builder.open()?;
        let skel = open.load()?;
        // Attach the three raw tracepoints.
        let mut links = Vec::new();
        links.push(skel.progs.tcp_probe.attach()?);
        links.push(skel.progs.tcp_rcv_space_adjust.attach()?);
        links.push(skel.progs.tcp_destroy_sock.attach()?);
        // mmap the histogram array (BPF_F_MMAPABLE).
        let map = &skel.maps.latency;
        let bytes = BPF_BUCKETS * std::mem::size_of::<u64>();
        let fd = map.as_fd().as_raw_fd();
        let mmap = unsafe {
            memmap2::MmapOptions::new().len(bytes)
                .map(&std::fs::File::from(std::os::fd::OwnedFd::from_raw_fd_dup(fd)?))?
        };
        // SAFETY: skel must outlive mmap; we move both into State::Running.
        let skel: TcpPacketLatencySkel<'static> = unsafe { std::mem::transmute(skel) };
        self.state = State::Running { _skel: skel, mmap, _links: links };
        Ok(())
    }

    fn read_counts(&self) -> Vec<u64> {
        if let State::Running { mmap, .. } = &self.state {
            let (_p, vals, _s) = unsafe { mmap.align_to::<u64>() };
            vals.to_vec()
        } else { Vec::new() }
    }
}

#[async_trait]
impl Sampler for PacketLatency {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, _now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error=%e, "tcp/packet_latency: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!("tcp/packet_latency attached (raw_tp, mmap histogram)");
            }
            State::Running { .. } => {}
        }
        // Zero-syscall read: reconstruct buckets from the mmap'd array.
        let counts = self.read_counts();
        if let Some(id) = self.metric_id {
            reg.record_distribution_buckets(id, buckets_from_counts(&counts));
        }
        Ok(())
    }
}

use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry {
    name: NAME,
    init: |reg, iv| Box::new(PacketLatency::new(reg, iv)),
};
```
> **Note on the `'static` transmute / mmap lifetime:** the exact safe pattern depends on the pinned libbpf-rs API (some versions expose `Map::initial_value`/`mmap` helpers, removing the manual `File`/`OwnedFd` dance). Verify against `cargo doc -p libbpf-rs` for the pinned version and prefer the library's own mmap accessor if present (rezolus uses `MmapMut` on the map fd; see `/Users/gmckee/projects/rezolus/src/agent/bpf/counters.rs` `PackedCounters::new`). The `OwnedFd::from_raw_fd_dup` shorthand stands in for "dup the map fd into an owned File for mmap" — implement with `libc::dup` + `File::from_raw_fd` if no helper exists.

- [ ] **Step 4: Build**

Run (Linux): `cargo build -p nyquist-ebpf`
Expected: PASS. Resolve skeleton type-name mismatches (`TcpPacketLatencySkel` vs generated name) by checking the generated `$OUT_DIR/tcp_packet_latency.bpf.rs` (libbpf-cargo derives the type name from the `.bpf.c` object name).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-ebpf/src/libbpf/tcp/ crates/nyquist-ebpf/build.rs
git commit -m "feat(ebpf): tcp/packet_latency in-kernel RTT histogram (libbpf-rs, no ring buffer)"
```

---

### Task B5: `tcp/retransmit` BPF sampler

Port rezolus's `tcp/retransmit`. `kprobe/tcp_retransmit_skb`, relaxed-atomic increment of a per-CPU-banked mmap'd counter; userspace sums bank slot 0 across CPUs and records a counter (design §5.2). No ring buffer.

**Files:**
- Create: `crates/nyquist-ebpf/src/libbpf/tcp/retransmit/mod.bpf.c`
- Create: `crates/nyquist-ebpf/src/libbpf/tcp/retransmit/mod.rs`
- Modify: `crates/nyquist-ebpf/build.rs` (add `("tcp/retransmit", "tcp_retransmit")`)

**Interfaces:**
- Registers `SamplerEntry { name: "tcp/retransmit", init }`.
- Emits `tcp/retransmits` (`Kind::Counter`, `Unit::Count`) via `reg.record_counter` (rate + raw), matching the existing tuple for parity.

- [ ] **Step 1: Write `mod.bpf.c`**

Create `crates/nyquist-ebpf/src/libbpf/tcp/retransmit/mod.bpf.c`:
```c
// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/tcp/linux/retransmit/mod.bpf.c (MIT OR Apache-2.0).
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

// Per-CPU banks; each CPU writes only its own slot (no contention).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS *COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

SEC("kprobe/tcp_retransmit_skb")
int BPF_KPROBE(tcp_retransmit_skb, struct sock *sk, struct sk_buff *skb, int segs) {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();
    array_add(&counters, idx, (u64)(segs > 0 ? segs : 1));
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
```

- [ ] **Step 2: Write the Rust sampler `mod.rs`**

Create `crates/nyquist-ebpf/src/libbpf/tcp/retransmit/mod.rs`, mirroring B4 but summing per-CPU bank slot 0 and recording a counter:
```rust
const NAME: &str = "tcp/retransmit";
const METRIC: &str = "tcp/retransmits";
const COUNTER_GROUP_WIDTH: usize = 8;
const MAX_CPUS: usize = 1024;

mod skel { include!(concat!(env!("OUT_DIR"), "/tcp_retransmit.bpf.rs")); }

use std::time::{Duration, Instant};
use std::os::fd::AsRawFd;
use async_trait::async_trait;
use memmap2::Mmap;
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use nyquist_core::model::{Kind, Unit};
use skel::*;

enum State { Uninit, Disabled, Running { _skel: TcpRetransmitSkel<'static>, mmap: Mmap, _link: libbpf_rs::Link } }
pub struct Retransmit { interval: Duration, state: State, id: Option<nyquist_core::model::MetricId> }

impl Retransmit {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        let id = reg.register(MetricDef::new(METRIC, Kind::Counter).unit(Unit::Count));
        Retransmit { interval, state: State::Uninit, id: Some(id) }
    }
    fn try_init(&mut self) -> anyhow::Result<()> {
        use libbpf_rs::skel::{OpenSkel, SkelBuilder};
        let skel = TcpRetransmitSkelBuilder::default().open()?.load()?;
        let link = skel.progs.tcp_retransmit_skb.attach()?;
        let bytes = MAX_CPUS * COUNTER_GROUP_WIDTH * std::mem::size_of::<u64>();
        let fd = skel.maps.counters.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(libc::dup(fd)) };
        let mmap = unsafe { memmap2::MmapOptions::new().len(bytes).map(&file)? };
        let skel: TcpRetransmitSkel<'static> = unsafe { std::mem::transmute(skel) };
        self.state = State::Running { _skel: skel, mmap, _link: link };
        Ok(())
    }
    fn total(&self) -> u64 {
        if let State::Running { mmap, .. } = &self.state {
            let (_p, vals, _s) = unsafe { mmap.align_to::<u64>() };
            (0..MAX_CPUS).map(|cpu| vals.get(cpu * COUNTER_GROUP_WIDTH).copied().unwrap_or(0)).sum()
        } else { 0 }
    }
}

#[async_trait]
impl Sampler for Retransmit {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() { tracing::warn!(error=%e, "tcp/retransmit: load failed"); self.state = State::Disabled; return Ok(()); }
                tracing::info!("tcp/retransmit attached (kprobe, mmap counter)");
            }
            State::Running { .. } => {}
        }
        let total = self.total();
        if let Some(id) = self.id { reg.record_counter(id, now, total); }
        Ok(())
    }
}

use std::os::fd::FromRawFd;
use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry { name: NAME, init: |reg, iv| Box::new(Retransmit::new(reg, iv)) };
```
Add `libc = "0.2"` to the crate's Linux deps if not present.

- [ ] **Step 3: Build**

Run (Linux): `cargo build -p nyquist-ebpf`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/nyquist-ebpf/src/libbpf/tcp/retransmit/ crates/nyquist-ebpf/build.rs
git commit -m "feat(ebpf): tcp/retransmit mmap'd counter (libbpf-rs, no ring buffer)"
```

---

### Task B7: Wire the libbpf samplers in; drop the Aya wiring

Switch `main.rs` from the Aya `EbpfSampler` to the linkme-registered libbpf samplers. The procfs samplers already come through `build_enabled` (Plan A); the BPF samplers register into the same `SAMPLERS` slice, so they flow through `build_enabled` too — gated by config name (`tcp/packet_latency`, `tcp/retransmit`).

**Files:**
- Modify: `src/main.rs:10,116-120` (remove `nyquist_ebpf::EbpfSampler` import + the `if config.ebpf.enabled { ... }` block)
- Modify: `nyquist.toml` / config defaults (enable `tcp/packet_latency`, `tcp/retransmit` by name; deprecate `[ebpf] enabled`)
- Modify: `crates/nyquist-ebpf/src/lib.rs` (ensure the libbpf `tcp` module is referenced so linkme entries aren't dead-stripped)

- [ ] **Step 1: Ensure linkme entries are linked**

In `crates/nyquist-ebpf/src/lib.rs`, the `pub mod libbpf;` chain must reach the sampler modules. Add a no-op referenced symbol if needed:
```rust
/// Touch the BPF sampler modules so their `#[distributed_slice(SAMPLERS)]`
/// entries are linked into the final binary (linkme requires the defining
/// crate to be referenced).
pub fn registered() -> usize { nyquist_core::registration::all_sampler_names().len() }
```
Call `nyquist_ebpf::registered()` once in `main.rs` startup (e.g. log it) so the crate is linked.

- [ ] **Step 2: Remove the Aya sampler wiring from main.rs**

Delete the `if config.ebpf.enabled { let ebpf_sampler = EbpfSampler::new(...); ... }` block (lines 116-120) and the `use nyquist_ebpf::EbpfSampler;` import (line 10). Add near startup:
```rust
tracing::info!(registered = nyquist_ebpf::registered(), "samplers registered (incl. BPF)");
```

- [ ] **Step 3: Enable the BPF samplers by name in config**

In `nyquist.toml`, replace `[ebpf]\nenabled = true` with per-sampler config:
```toml
[samplers."tcp/packet_latency"]
enabled = true

[samplers."tcp/retransmit"]
enabled = true
```
(The config already gates by name via `Config::sampler(name)`; the BPF samplers use slash-named keys.)

- [ ] **Step 4: Build + run the agent on the Linux host**

Run (Linux, as root or with `CAP_BPF`+`CAP_PERFMON`): `cargo run --release -- --config nyquist.toml`
Expected: logs `tcp/packet_latency attached` and `tcp/retransmit attached`; `curl localhost:9100/metrics | grep tcp_rtt_us` shows percentile series; under induced retransmits, `tcp_retransmits_rate` is non-zero.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs nyquist.toml crates/nyquist-ebpf/src/lib.rs
git commit -m "feat(ebpf): wire libbpf TCP samplers via linkme; retire Aya ebpf wiring from main"
```

---

### Task B8: Tests — H2 indexing, build gate, slice integration, exposition, CI

**Files:**
- Create: `crates/nyquist-ebpf/tests/h2_indexing.rs` (port rezolus indexing tests; verify bucket boundaries + relative-error bound at `grouping_power=3`)
- Create: `crates/nyquist-ebpf/tests/slice_integration.rs` (Linux-gated, `#[ignore]` unless privileged)
- Create: `crates/nyquist-exposition/tests/bpf_exposition.rs` (bucket-derived percentile series within H2 error)
- Create/Modify: CI workflow under `.github/workflows/` to build skeletons for both arches (gated on `clang`) and run the parity gate

- [ ] **Step 1: H2 indexing/error-bound test**

In `crates/nyquist-ebpf/tests/h2_indexing.rs`, assert that for a spread of values, `buckets_from_counts` (B3) reconstructs upper bounds within 6.25% relative error of the input value at `grouping_power=3`. Mirror rezolus's indexing tests (`/Users/gmckee/projects/rezolus/src/agent/bpf/histogram.h` companion tests). Run: `cargo test -p nyquist-ebpf --test h2_indexing` → PASS.

- [ ] **Step 2: Slice integration test (Linux, privileged)**

In `crates/nyquist-ebpf/tests/slice_integration.rs`, `#[cfg(target_os = "linux")]` + `#[ignore]`: load the `tcp/packet_latency` skeleton, generate loopback TCP traffic (connect/echo), then assert the mmap'd histogram has non-zero total and recovered percentiles are plausible; load `tcp/retransmit`, induce retransmits (e.g. via `tc qdisc netem loss`), assert the counter increments. Document running with `cargo test -p nyquist-ebpf --test slice_integration -- --ignored` as root. Reuse the fixture harness pattern under `crates/nyquist-samplers/tests` where applicable (design §7).

- [ ] **Step 3: Exposition test**

In `crates/nyquist-exposition/tests/bpf_exposition.rs`, build a `RegistrySnapshot` with a synthetic distribution metric whose `buckets` come from `buckets_from_counts` of a known histogram, render `to_prometheus(&snap, &[50.0,90.0,99.0])`, and assert the emitted `_value`/`_rate` percentile series match the expected values within the H2 error bound. Run → PASS.

- [ ] **Step 4: CI — build skeletons for both arches**

Add a CI job that installs `clang` + `bpftool`, then runs `cargo build -p nyquist-ebpf` natively (x86_64) and cross-checks the aarch64 skeleton compiles (e.g. via a cross toolchain or a second runner). Gate the job on `clang` availability. Add the parity gate: run the agent, capture `/metrics`, and run Plan A's `coverage_parity` test with `NYQUIST_LIVE_METRICS` set (design §6.5).

- [ ] **Step 5: Commit**

```bash
git add crates/nyquist-ebpf/tests/ crates/nyquist-exposition/tests/bpf_exposition.rs .github/workflows/
git commit -m "test(ebpf): H2 indexing, slice integration, exposition, CI BPF build + parity gate"
```

---

### Task B9: Verify parity, then delete Aya

Only after the parity gate confirms the converted TCP samplers emit today's RTT + retransmit tuples (design §6.3), remove the Aya toolchain.

**Files:**
- Delete: `crates/nyquist-ebpf-programs/`, `crates/nyquist-ebpf-common/`
- Modify: `crates/nyquist-ebpf/` — delete `src/sampler.rs` (Aya `EbpfSampler`) and the Aya build block in `build.rs`; remove `aya`/`aya-build`/`async-trait`(if unused) and the `include_bytes_aligned!` path; remove the `[ebpf]` config struct
- Modify: `Cargo.toml` (workspace) — remove the `exclude = ["crates/nyquist-ebpf-programs"]` and the two deleted members
- Modify: `crates/nyquist-config/src/lib.rs` — remove the `[ebpf]` section (or keep as deprecated no-op)
- Modify: `docs/principles.md` (Plan A) — flip principles 1,2,5,6,7,8,10,11 and §9.5/§9.9 from `[PENDING]` to `[MET]`/`[MET, TCP slice]`

- [ ] **Step 1: Capture a live baseline and run the parity gate**

On the Linux host, with both the old (Aya) and procfs samplers running, capture `/metrics` to commit as the golden baseline (Plan A Task A4 follow-up). Then run the new build and re-run the gate:
```bash
scripts/capture-golden-baseline.sh > crates/nyquist-exposition/tests/fixtures/golden_metrics.txt   # from Aya build
# switch to libbpf build, capture live, then:
NYQUIST_LIVE_METRICS=/tmp/live_metrics.txt cargo test -p nyquist-exposition --test coverage_parity golden_baseline_never_drops
```
Expected: PASS — no dropped tuples (`tcp/rtt_us`, `tcp/retransmits`, and the legacy `ebpf/tcp/*` if you keep those names; see note). If the old emitter used `ebpf/tcp/rtt_us` and the new uses `tcp/rtt_us`, record the rename as an explicit mapping in the baseline fixture (design §6.5) — never a silent drop.

- [ ] **Step 2: Delete the Aya crates and code**

```bash
git rm -r crates/nyquist-ebpf-programs crates/nyquist-ebpf-common
git rm crates/nyquist-ebpf/src/sampler.rs
```
Remove the Aya build block from `crates/nyquist-ebpf/build.rs`, the `aya` deps from `crates/nyquist-ebpf/Cargo.toml`, the `exclude`/members in the workspace `Cargo.toml`, and the `EbpfSampler` re-export from `crates/nyquist-ebpf/src/lib.rs`. Remove `crates/nyquist-ebpf-programs/rust-toolchain.toml` reliance — the root `rust-toolchain.toml` stays `stable`.

- [ ] **Step 3: Build + full test on Linux**

Run: `cargo test --workspace` and `cargo build -p nyquist-ebpf`
Expected: PASS, no Aya references remain (`grep -rn "aya\|include_bytes_aligned\|RTT_EVENTS\|PROBE_SAMPLE_CTR\|PROBE_OFFSETS" crates/ src/` returns nothing).

- [ ] **Step 4: Update principles.md statuses**

Flip the relevant `[PENDING]` tags to `[MET]` in `docs/principles.md`, citing the TCP slice.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(ebpf): remove Aya toolchain; TCP slice fully on libbpf-rs (parity verified)"
```

---

## Self-Review Notes (for the implementer)

- **Spec coverage:** Plan B covers design §9 deliverables 1 (libbpf build + vmlinux.h + ported headers: B1/B2), 4 (TCP slice + Aya removal: B4/B5/B7/B9), and the BPF testing strategy §7 (B8). Deliverable 4's "removal of ring buffers/1-in-128 sampling" is inherent — the new `.bpf.c` has neither.
- **Sequencing:** B6 (Registry seam) is implemented before B4/B5 despite its design number, because the samplers call `record_distribution_buckets`. B9 (Aya deletion) is last and gated on the parity gate.
- **Version risk:** the libbpf-rs 0.24 skeleton API (`skel.maps.<name>`, `skel.progs.<name>`, `*SkelBuilder`) and the `histogram` `from_buckets` constructor are the two API-version-sensitive spots. Each has an explicit `cargo doc` verification step (B3 Step 1, B4 Step 4). Pin libbpf-rs and libbpf-cargo to the same minor version.
- **mmap lifetime:** the `transmute` to `'static` + manual fd dup in B4/B5 is the fragile part; prefer the pinned libbpf-rs's own map-mmap accessor if it exists (check `cargo doc`), modeled on rezolus `PackedCounters` (`/Users/gmckee/projects/rezolus/src/agent/bpf/counters.rs`).
- **Coverage parity (design §6):** the converted samplers must keep emitting the TCP RTT + retransmit tuples; any name change from `ebpf/tcp/*` to `tcp/*` is an explicit recorded mapping, verified by the gate before B9 deletes Aya.
