# rust-nyquist Instrumentation Principles

Derived from [rezolus's `docs/principles.md`](https://github.com/iopsystems/rezolus)
(MIT OR Apache-2.0) and adapted to rust-nyquist. This is the review checklist for
every follow-on sampler migration under the Rezolus Alignment effort
(`docs/superpowers/specs/2026-06-22-rezolus-alignment-design.md`).

Each principle is tagged with its current status:
- **[MET]** — realized today.
- **[PENDING]** — committed; lands in a named plan/slice.
- **[DEVIATION]** — knowingly diverges during migration; tracked, not silent.

rust-nyquist runs a **dual model** during migration: BPF samplers aggregate in
the kernel and are read mmap-direct; legacy procfs/perf samplers still self-clock
and feed the userspace sliding-window engine. Principles below apply to the BPF
path unless noted.

---

## 1. Aggregate in the kernel — **[MET, TCP slice]**
Counters and histograms live in `BPF_MAP_TYPE_ARRAY` maps with `BPF_F_MMAPABLE`;
increments are `__atomic_fetch_add(__ATOMIC_RELAXED)`. Realized for the TCP slice
(`tcp/packet_latency`, `tcp/retransmit`); procfs samplers remain
userspace-windowed until each is migrated.

## 2. mmap-direct, zero-syscall reads — **[MET, TCP slice]**
Userspace reads the mmap'd region directly; the hot read path never calls
`bpf_map_lookup_elem`. (`Registry::record_distribution_buckets` seam +
`memmap2` on the BPF array fd.)

## 3. Consumers drive cadence — **[MET]**
No periodic flush/downsample is baked into collection. The `/metrics` endpoint,
ClickHouse sink, VictoriaMetrics push, and Parquet recorder each read on their
own interval, and **percentile selection is a consumer concern** computed at read
time from bucket arrays (`nyquist_core::percentiles::percentiles_from_buckets`),
not on a collection clock. (Plan A, Tasks A1–A2.) The legacy procfs samplers
still self-clock — see §10.

## 4. Distributions over summaries — **[MET]** (storage caveat in §9)
Histograms flow downstream as full H2 bucket arrays. `MetricSnapshot` carries a
sparse `(upper_bound, count)` array; percentile/time-window choice lives in the
consumer. (Plan A, Task A1.)

## 5. No per-event streaming for measurement — **[MET, TCP slice]**
Per-event submission to userspace scales with workload throughput and is refused.
TCP ring buffers and the 1-in-128 `tcp_probe` sampling hack are gone; overhead
no longer scales with packet rate.

## 6. CO-RE on vanilla kernels — **[MET, TCP slice]**
BPF code uses CO-RE (`BPF_CORE_READ`); per-arch, version-pinned `vmlinux.h`
snapshots are checked in at `crates/nyquist-ebpf/bpf/{x86_64,aarch64}/`. Updates
are deliberate and per-arch, never silently regenerated.

## 7. Bounded constant work per probe — **[MET, TCP slice]**
O(1) hot paths, no loops, relaxed atomics. The CLZ branch-tree H2 indexing in
`histogram.h` is branch-bounded.

## 8. Arrays over hashmaps; documented pointer-key exception — **[MET, TCP slice]**
Bounded-integer keys use `BPF_MAP_TYPE_ARRAY`. The one pointer-keyed `HASH`
(per-socket state keyed by `struct sock*` in `tcp/packet_latency`) is the
documented exception, carried with its justifying comment.

## 9. H2 histograms with bounded relative error — **[MET]**
The windowed path uses the `histogram` crate at `grouping_power = 7` (~1% error).
The in-kernel BPF path uses `grouping_power = 3` (~6.25% max error, 496 buckets),
matching rezolus. Both paths are realized.

## 10. Tolerate benign races for monotone values — **[MET, TCP slice]**
Monotone high-water values may use non-atomic `array_set_if_larger`, commented as
a benign race. (`helpers.h` ported from rezolus.)

## 11. Shared BPF infrastructure in headers — **[MET, TCP slice]**
Cross-cutting BPF logic (CLZ/H2 indexing, helpers) lives in shared headers
(`histogram.h`, `helpers.h`), not duplicated per sampler.

## 12. Userspace overhead is part of the budget — **[MET]**
Snapshots are O(active metrics); bucket arrays are sparse `(bound, count)` pairs,
not dense per-metric arrays. (Plan A.)

## 13. Prefer BPF probes over parsing procfs in the hot path — **[DEVIATION, migrating]**
The dual model (§4 of the design) keeps procfs/perf samplers on their current
implementation. Procfs is **retired on parity**, one sampler at a time, never on
a schedule (design §6.3). The retained-procfs families are enumerated in
[`coverage-map.md`](coverage-map.md).

## 14. Coverage parity — never drop a metric tuple — **[MET]**
The emitted `(name, label-key set, kind, unit)` tuple set may only grow. A golden
baseline + parity-gate test guards this (`nyquist_core::coverage`, the
`coverage_parity` exposition test). A procfs sampler is removed only once its
replacement emits identical tuples. (Plan A, Task A4; design §6.)

## 15. `linkme` distributed-slice registration — **[MET]**
Samplers register into a `linkme` `SAMPLERS` slice declared in
`nyquist_core::registration`, replacing the manual factory. All procfs samplers
registered in Plan A (Task A3); BPF samplers (`tcp/packet_latency`,
`tcp/retransmit`) registered in Plan B.

---

## Operational checklist (run on every sampler migration)

Before retiring a procfs/perf sampler in favour of a BPF replacement:

1. **Tuple parity.** Does the replacement emit the *identical* `(name, labels,
   kind, unit)` tuples (incl. dynamic families)? Record any rename as an explicit
   mapping in the golden baseline — never a silent drop. (§14)
2. **In-kernel aggregation.** Counters/histograms in `BPF_F_MMAPABLE` arrays;
   relaxed atomics; no per-event userspace streaming. (§1, §5)
3. **mmap-direct read.** No `bpf_map_lookup_elem` on the read path. (§2)
4. **CO-RE.** Field access via `BPF_CORE_READ`; no runtime offset parsing. (§6)
5. **Map choice.** Arrays for bounded keys; any `HASH` (pointer key) carries a
   justifying comment. (§8)
6. **Bucket exposition.** Histograms surface as H2 bucket arrays; percentiles
   computed consumer-side. (§4, §9)
7. **Parity gate green** with a live capture (`NYQUIST_LIVE_METRICS`). (§14)

## Known deviations

- **Dual model (§13).** procfs/perf and BPF paths coexist during migration behind
  one snapshot interface; both are independently tested.
- **Retained procfs families.** `memory/*`, `psi/*`, `loadavg`, `sockstat`,
  `snmp`/`netstat`/`snmp6` (interim, now incl. IPv6), `network/*` bytes+errors
  (BPF traffic covers only the drop breakdown), `cpu/usage/steal`, `nic_stats`
  (ethtool discovery) — see [`coverage-map.md`](coverage-map.md).

## Resolved deviations

- **Durable-record summaries (§9, design §3.4/§6.6) — RESOLVED.** The Parquet
  recorder stores full H2 bucket arrays (`buckets_json`), not pre-computed
  percentile columns, so percentile/window choice stays a read-time consumer
  concern for durable records too.
- **eBPF distributions are windowed — RESOLVED.** Kernel histograms are
  cumulative; the registry keeps periodic checkpoints and snapshots emit
  `latest − checkpoint(≈window ago)`, so BPF latency percentiles reflect the
  trailing window like every other metric (not since-agent-start). Stale
  direct-bucket metrics (dead sampler) expire to empty rather than freezing.
