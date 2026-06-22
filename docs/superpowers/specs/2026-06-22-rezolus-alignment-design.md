# Rezolus Alignment — Design

**Date:** 2026-06-22
**Status:** Approved design; implementation plan to follow (writing-plans).
**Scope:** Foundation + one reference vertical slice. Per-sampler migration of the
rest is deferred to follow-on specs.

## 1. Background & motivation

rust-nyquist began as an *oversampling* agent: each sampler self-clocks on a
sub-second tokio timer (default 10 ms), reads procfs/sysfs (or drains an eBPF
ring buffer), computes instantaneous rates, and accumulates them into a
userspace sliding-window histogram from which percentiles are produced at export
time.

Rezolus takes the opposite approach for its instrumentation, captured in its
`docs/principles.md`:

- **Aggregate in the kernel.** Counters and histograms live in
  `BPF_MAP_TYPE_ARRAY` maps with `BPF_F_MMAPABLE`.
- **mmap-direct reads.** Userspace reads the maps via mmap with zero syscalls
  per refresh — never `bpf_map_lookup_elem` on the hot read path.
- **Consumers drive cadence.** The agent does no periodic flushing or
  downsampling; recorder, exporter, viewer, and query consumers each read on
  their own cadence.
- **Distributions over summaries.** Histograms flow downstream as full H2
  (HDR-style) bucket arrays; percentile choice and time-window choice live in
  the consumer, not the agent.
- **No per-event streaming for measurement.** Per-event submission to userspace
  scales with workload throughput and is refused.

A comparison of the two codebases found that rust-nyquist currently *contradicts*
rezolus on its most load-bearing axes — most starkly in its eBPF layer, which
streams raw `tcp_probe` / `tcp_retransmit_skb` events through a 4 MB ring buffer
with 1-in-128 in-kernel sampling. That is precisely the pattern rezolus's
principles refuse (overhead proportional to workload throughput).

The owner has decided to **fully adopt the rezolus model**. This document
specifies the architectural foundation for that adoption plus one reference
vertical slice — the conversion of the existing TCP eBPF sampler — that exercises
every layer end-to-end and remediates the worst existing violation.

## 2. Goals & non-goals

### Goals

1. Re-found rust-nyquist's instrumentation on the rezolus BPF model: in-kernel
   aggregation into `BPF_F_MMAPABLE` maps, mmap-direct reads, consumer-driven
   cadence, and full H2 histogram bucket arrays exposed (percentiles computed
   downstream).
2. Replace the Aya BPF toolchain with `libbpf-rs` + CO-RE C programs, matching
   rezolus's build system, checked-in per-arch `vmlinux.h`, and shared BPF
   headers.
3. Prove the model end-to-end with one reference slice: convert the TCP eBPF
   sampler to in-kernel H2 histogram (RTT) + mmap'd counter (retransmits),
   deleting the ring-buffer streaming and 1-in-128 sampling.
4. Adopt `linkme` distributed-slice sampler registration, matching rezolus.
5. **Preserve metric coverage with zero regressions** (see §6).
6. Establish a rust-nyquist `docs/principles.md`, derived from rezolus's, as the
   review checklist for every follow-on sampler migration.

### Non-goals (deferred)

- Converting the other ~13 samplers. They remain on their current procfs/perf
  implementation until each gets its own migration spec.
- **Removing** any procfs sampler. Procfs samplers are retired one at a time,
  only when a replacement proves coverage parity (see §6 invariant).
- Native-histogram storage (full bucket arrays into ClickHouse columns /
  Prometheus native histograms with query-time percentiles). Sinks continue to
  compute scalar percentile series from bucket arrays for now.
- ClickHouse / VictoriaMetrics schema redesign beyond what the bucket-array
  exposition requires.
- **Parquet recorder schema redesign.** Aligning `nyquist-recorder` to rezolus's
  columnar histogram layout (full H2 bucket arrays as `List<UInt64>` for
  distribution metrics, instead of the current pre-computed `p50/p90/p99/p99_9`
  columns) is a committed alignment item but is deferred to a **dedicated
  follow-on spec** (see §6.6 and the deliberate-deviation note in §3.4). Until
  then the recorder keeps writing percentile columns.

### Acknowledged consequence

The self-clocked userspace windowing engine (10 ms tickers building rate
distributions) is replaced **for BPF samplers** by continuous in-kernel
aggregation. The windowing engine survives only for the procfs/perf samplers
that still sample on a clock, until they too are migrated. The agent therefore
runs a **dual model** during migration; §4 defines the seam.

## 3. Target architecture

### 3.1 BPF toolchain (replaces Aya)

- **Build system.** A `build.rs` using `libbpf_cargo::SkeletonBuilder` compiles
  each sampler's `mod.bpf.c` into a skeleton under `$OUT_DIR`, mirroring
  rezolus's `build.rs`. Requires `clang` at build time. The Aya path
  (`bpf-linker`, nightly `-Z build-std`, `bpfel-unknown-none` target, embedded
  ELF via `include_bytes_aligned!`) is removed.
- **CO-RE.** BPF code uses CO-RE (`BPF_CORE_READ`, `bpf_core_read`) for kernel
  field access. The runtime debugfs `format`-file offset parsing currently used
  by the Aya sampler is removed in favour of CO-RE relocations.
- **vmlinux.h.** Per-arch, version-pinned snapshots are checked in at
  `crates/<bpf-crate>/bpf/{x86_64,aarch64}/vmlinux.h`. Updates are deliberate and
  per-arch, never silently regenerated.
- **Shared headers.** Ported from rezolus (subject to license/attribution, §8):
  - `histogram.h` — CLZ branch-tree H2 indexing, `grouping_power = 3`
    (~6.25% max relative error, 496 buckets).
  - `helpers.h` — `array_add`, `array_set_if_larger`, `histogram_incr`.
  - `counters.rs` — the `Counters` / `CpuCounters` / `PackedCounters`
    strategies.
  - `cgroup.h` / `task.h` — ported when first needed by a sampler (not in this
    slice).

### 3.2 Crate layout

The three Aya crates (`nyquist-ebpf`, `nyquist-ebpf-programs`,
`nyquist-ebpf-common`) are replaced by a libbpf-rs-based BPF crate (final
crate name decided in the plan; default `nyquist-ebpf`). Each BPF sampler is a
directory containing `mod.bpf.c`, `mod.rs`, and `stats.rs`, mirroring rezolus's
`src/agent/samplers/<category>/linux/<sampler>/` layout.

### 3.3 Sampler structure & registration

- The Rust side of each BPF sampler loads its skeleton (`include!` of the
  generated `*.bpf.rs`), attaches programs, and registers mmap'd maps through a
  `BpfBuilder`-style fluent API (`.histogram(name, &METRIC)`,
  `.counters(...)`), mirroring rezolus.
- **Registration uses a `linkme` distributed slice** (`SAMPLERS`), replacing the
  manual `inventory.rs` factory. Each sampler contributes a `SamplerEntry { name,
  init }`. The config still gates enablement by name. `inventory.rs`'s
  `all_sampler_names` / `build_enabled` are reworked to iterate the slice.

### 3.4 Data model & exposition

- **In-kernel aggregation.** Counters and histograms live in mmap'd BPF arrays;
  increments are `__atomic_fetch_add(__ATOMIC_RELAXED)`. Monotone high-water
  values may use `array_set_if_larger` (non-atomic, commented as a benign race).
- **mmap-direct reads.** Userspace reads the mmap'd region directly. The hot read
  path never calls `bpf_map_lookup_elem`.
- **No agent self-clock for BPF metrics.** No tokio ticker drives BPF sampler
  reads; the BPF program aggregates continuously. A `read_snapshot()` path reads
  the mmap'd state on demand.
- **Consumers drive cadence.** The `/metrics` endpoint, ClickHouse sink,
  VictoriaMetrics push, and Parquet recorder each read the mmap'd snapshot on
  their own interval.
  - *Deliberate interim deviation (Parquet).* The Parquet recorder is brought
    onto the consumer-driven read path in this spec, but **retains its current
    pre-computed percentile-column schema** (`p50/p90/p99/p99_9`). Switching it
    to store full H2 bucket arrays (`List<UInt64>`), per rezolus principle 9, is
    deferred to the follow-on recorder spec (§6.6). This is the one place the
    foundation knowingly diverges from "distributions over summaries," and it is
    tracked, not silent.
- **Histograms exposed as full H2 bucket arrays.** `MetricSnapshot` gains a
  bucket-array representation for distribution metrics. **Percentile computation
  moves out of `Registry` into a shared helper invoked by each sink at read
  time**, so VM/CH still emit `_p99`-style scalar series but the percentile set
  is a consumer concern, not baked into a collection clock.

## 4. The dual-model seam (migration period)

Two metric-production paths coexist until migration completes:

- **BPF path** — in-kernel aggregation, mmap-direct, consumer-driven, full bucket
  arrays. No windowing engine involvement.
- **Legacy windowed path** — procfs/perf samplers self-clock and feed the
  userspace sliding-window histogram, exactly as today.

The seam: `Registry` (or its successor) exposes a unified snapshot that merges
(a) BPF metrics read from mmap and (b) windowed metrics from the legacy engine.
Sinks consume the unified snapshot and compute percentiles uniformly. The two
paths are independently testable. As samplers migrate, metrics move from path (b)
to path (a); the snapshot interface does not change.

## 5. Reference vertical slice — TCP sampler conversion

Replace the Aya ring-buffer TCP sampler with two rezolus-style BPF samplers,
each templated on the existing rezolus sampler of the same name:

### 5.1 `tcp/packet_latency` (RTT)

- **Attach:** `tcp_probe` + `tcp_rcv_space_adjust` + `tcp_destroy_sock` (mirrors
  rezolus `tcp_packet_latency`).
- **Aggregation:** RTT computed in-kernel; `histogram_incr` into a
  `BPF_F_MMAPABLE` H2 histogram (`grouping_power = 3`).
- **Keying:** per-socket state keyed by `struct sock*` uses `BPF_MAP_TYPE_HASH` —
  the documented pointer-key exception in rezolus principle 5, carried over with
  the justifying comment.
- **Removed:** the 4 MB `RTT_EVENTS` ring buffer, the 1-in-128 `PROBE_SAMPLE_CTR`
  sampling, the `PROBE_OFFSETS` debugfs-offset map, and the userspace event-drain
  loop.

### 5.2 `tcp/retransmit`

- **Attach:** `tcp_retransmit_skb` (mirrors rezolus `tcp/retransmit`).
- **Aggregation:** relaxed-atomic increment of an mmap'd counter.
- **Removed:** the `RETRANSMIT_EVENTS` ring buffer and its drain loop.

### 5.3 Outcome that proves the model

- Overhead no longer scales with packet rate — no per-event work crosses into
  userspace, so the 1-in-128 sampling hack is unnecessary and removed.
- Zero syscalls per refresh on the read path.
- The full RTT distribution is exposed as bucket arrays; the sink computes the
  configured percentiles.

## 6. Coverage parity — zero metric regressions

This is a hard requirement on the implementation plan.

### 6.1 Definition

"Coverage" is the set of emitted **(metric name, label-key set, kind, unit)**
tuples. It includes both statically-named metrics and the **dynamically
generated** families that a source grep cannot see — `cpu/usage/{...}` per CPU,
`memory/*` per `/proc/meminfo` field, `psi/{cpu,memory,io}/*`, and most
`softirq/*` types.

### 6.2 Golden baseline

Before any migration, the implementation plan captures a **golden baseline** by
snapshotting a live agent's `/metrics` and `/metrics.json` output and reducing it
to the canonical tuple set. This baseline is committed as a test fixture.

### 6.3 Migration invariant

A procfs/perf sampler is removed **only** once its replacement emits the
identical metric tuples. Until a metric has a verified replacement, its current
sampler stays. Coverage therefore never dips during migration — it can only gain
resolution. "Procfs demoted to fallback" means *retired on parity*, not deleted
on a schedule.

### 6.4 Complete coverage map (migration roadmap)

The spec's roadmap enumerates **every current metric family** mapped to a target
source — BPF, perf, or retained-procfs — so each follow-on spec inherits a
defined contract. The current inventory (to be expanded with the dynamic
families in the plan):

| Current sampler / family | Metrics (representative) | Target source |
|---|---|---|
| `tcpinfo` / `inet_diag`, Aya eBPF | `tcp/rtt_us`, `ebpf/tcp/rtt_us`, `tcp/retransmits`, `ebpf/tcp/retransmits` | **BPF (this slice):** `tcp/packet_latency` + `tcp/retransmit` |
| `cpu` (`/proc/stat`) | `cpu/usage/{user,nice,system,idle,iowait,irq,softirq}` per CPU | BPF `cpu/usage` (follow-on) |
| `perf` hw/sw | `perf/hw/*`, `perf/sw/*` | perf (already aligned; keep) |
| `disk` (`/proc/diskstats`) | `disk/{read,write}/bytes` | BPF `blockio/{latency,requests}` (follow-on) |
| `softirqs` (`/proc/softirqs`) | `softirq/*` | BPF or retained-procfs (decide in follow-on) |
| `memory` (`/proc/meminfo`) | `memory/*` | retained-procfs (rezolus treats as acknowledged drift) |
| `psi` (`/proc/pressure/*`) | `psi/{cpu,memory,io}/*` | retained-procfs |
| `loadavg` | `cpu/load/{1,5,15}` | retained-procfs |
| `network` (`/proc/net/dev`) | `network/{receive,transmit}/{bytes,errors,dropped}` | BPF `network/{traffic,interfaces}` (follow-on) |
| `snmp` (`/proc/net/snmp`) | `ip/*`, `tcp/*`, `udp/*`, `icmp/*` | retained-procfs or BPF (follow-on) |
| `netstat` (`/proc/net/netstat`) | `tcp/retrans/*`, `tcp/window/*`, `tcp/drop/*`, `tcp/dsack/*`, `tcp/ofo/*`, `tcp/abort/*`, ... | retained-procfs or BPF (follow-on) |
| `sockstat` (`/proc/net/sockstat`) | `net/sockets/*` | retained-procfs |
| `nic_stats` (ethtool) | `nic/*`, `nic/queue/*` | retained (ethtool discovery) |

This table is the migration contract; rows are realized by follow-on specs, not
this one. The only rows *implemented here* are the TCP rows.

### 6.5 Parity gate (test)

A test diffs the live emitted tuple set against the golden baseline. The
migration may **add** tuples, but the baseline set must remain a subset
(no drops). For the reference slice this asserts the converted TCP samplers still
emit today's RTT and retransmit metrics. Any deliberate rename is recorded as an
explicit mapping in the baseline fixture — never a silent drop.

### 6.6 Parquet recorder alignment (committed, follow-on)

The Parquet recorder is a committed alignment target, deferred to its own
follow-on spec:

- **Schema change.** Replace the pre-computed `p50/p90/p99/p99_9` columns with
  rezolus's columnar histogram layout — full H2 bucket arrays stored as
  `List<UInt64>` for distribution metrics, alongside raw `UInt64`/`Int64` columns
  for counters/gauges and the `timestamp`/`duration` columns. This preserves the
  full distribution in the durable record so any percentile/window can be
  computed later (rezolus principle 9).
- **Coverage parity.** The recorder follow-on is itself subject to the §6 parity
  gate: every metric currently written to parquet must still be present
  (gaining bucket-array fidelity, not losing rows).
- **Interim state.** Until that spec lands, the recorder keeps writing percentile
  columns (the §3.4 deliberate deviation).

## 7. Testing strategy

- **BPF build test:** skeletons compile for `x86_64` and `aarch64` in CI
  (gated on `clang`).
- **H2 histogram unit tests:** port rezolus's indexing tests; verify bucket
  boundaries and the relative-error bound at `grouping_power = 3`.
- **Slice integration test:** load the `tcp/packet_latency` skeleton on a Linux
  runner, generate TCP traffic, assert the mmap'd histogram populates and
  percentiles are recoverable; assert `tcp/retransmit` counter increments under
  induced retransmits. Reuse the fixture harness under
  `crates/nyquist-samplers/tests` where applicable.
- **Exposition test:** assert `/metrics` emits bucket-derived percentile series
  matching (within the H2 error bound) the prior output for a synthetic
  histogram.
- **Coverage-parity gate:** §6.5, run in CI.
- **No-regression:** procfs/perf samplers and the legacy windowed path keep their
  current tests untouched.

## 8. Risks & mitigations

- **Toolchain churn.** Dropping Aya requires `clang` and checked-in `vmlinux.h`,
  and CI must build BPF. *Mitigation:* mirror rezolus's proven `build.rs` and
  skeleton flow.
- **Dual model complexity.** BPF (mmap, consumer-driven) and procfs (windowed,
  self-clocked) coexist during migration. *Mitigation:* the §4 seam isolates the
  two paths behind one snapshot interface; both are independently tested.
- **License / provenance.** Porting rezolus C headers and `vmlinux.h` must
  respect rezolus's license. *Mitigation:* record attribution and verify license
  compatibility in the plan before copying any file.
- **Coverage regression.** The whole point of §6. *Mitigation:* golden baseline +
  parity gate + retire-on-parity invariant.

## 9. Deliverables of the implementation plan

1. libbpf-rs build system + checked-in `vmlinux.h` (both arches) + ported shared
   headers.
2. `linkme` `SAMPLERS` registration replacing `inventory.rs`.
3. mmap-direct exposition: bucket-array `MetricSnapshot`, sink-side percentile
   helper, unified dual-model snapshot.
4. Reference slice: `tcp/packet_latency` + `tcp/retransmit` BPF samplers; removal
   of the Aya TCP sampler and its ring buffers.
5. Golden coverage baseline fixture + parity gate test.
6. rust-nyquist `docs/principles.md` derived from rezolus's, annotated with
   met / pending / deliberate-deviation status.
7. Full §6.4 coverage map expanded with the dynamic metric families.
