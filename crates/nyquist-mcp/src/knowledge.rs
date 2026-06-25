//! Encoded nyquist troubleshooting knowledge: schema, known issues, playbooks.
//! Exposed as MCP resources + prompts so an assistant can interpret nyquist
//! metrics correctly without re-deriving the hard-won context.

pub struct Doc {
    pub uri:  &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub body: &'static str,
}

pub struct PromptDef {
    pub name: &'static str,
    pub desc: &'static str,
    pub body: &'static str,
}

pub const RESOURCES: &[Doc] = &[
    Doc {
        uri:  "nyquist://schema",
        name: "samples table schema & percentile semantics",
        desc: "ClickHouse `samples` columns and how to read the percentile rate columns",
        body: SCHEMA,
    },
    Doc {
        uri:  "nyquist://known-issues",
        name: "known issues & gotchas",
        desc: "GRO aliasing, cpu_usage source split, percentiles > line rate, per-flow ceiling, switch-MTU black hole",
        body: KNOWN_ISSUES,
    },
    Doc {
        uri:  "nyquist://playbook",
        name: "troubleshooting playbook",
        desc: "Step-by-step flows for the common nyquist/network investigations",
        body: PLAYBOOK,
    },
    Doc {
        uri:  "nyquist://metric-catalog",
        name: "metric families catalog",
        desc: "The main metric families, their labels, and units (use list_metrics for the live list)",
        body: METRIC_CATALOG,
    },
];

pub const PROMPTS: &[PromptDef] = &[
    PromptDef {
        name: "diagnose-throughput",
        desc: "Guided workflow to investigate a throughput number that looks wrong (too low or capped)",
        body: DIAGNOSE_THROUGHPUT,
    },
    PromptDef {
        name: "interpret-percentiles",
        desc: "Explains how to read nyquist p50/p90/p99/p999 rate percentiles vs physical limits",
        body: INTERPRET_PERCENTILES,
    },
];

const SCHEMA: &str = r#"
# nyquist ClickHouse `samples` table

Columns:
- `ts`    DateTime64(3, UTC)            — insert timestamp (inserts every ~10s)
- `name`  LowCardinality(String)        — metric name, e.g. `network_receive_bytes`
- `tags`  Map(LowCardinality(String), String) — labels, e.g. {'iface':'ens1f0np0'}
- `raw`   UInt64                         — last cumulative counter value (or last gauge)
- `p50/p90/p99/p999` UInt64              — percentiles of the 10ms sub-sample RATES within the insert window

## How percentiles work
Counters are sampled every ~10ms; rate = delta/dt is computed per sample and pushed
into an H2 histogram. The p50/p90/p99/p999 columns are percentiles over those 10ms rate
samples accumulated during each ~10s insert window. So ONE row carries the sub-second
distribution — that is the oversampling value proposition.

## Unit conversions
- bytes/sec → Gbps:  value / 1.25e8   (125,000,000 bytes/s = 1 Gbps)
- The `_bytes` metrics store rate in bytes/sec.

## Trust model
- p50 is the trustworthy central rate (matches ground truth within ~1%).
- p99/p999 are the extreme tail of 10ms samples and carry ~2% measurement overshoot
  (see known-issues: "percentiles exceed line rate"). Use them for relative burstiness,
  not as literal throughput.

## Querying tips
- Convert ts windows with fromUnixTimestamp64Milli(N) / toUnixTimestamp64Milli(ts).
- Filter labels with tags['iface']='ens1f0np0'.
- Per-cpu metrics use tags['cpu']='cpuN' (note the 'cpu' prefix, matching /proc/stat).
  The aggregate /proc/stat line is tags['cpu']='cpu' — exclude it for per-core views.
"#;

const KNOWN_ISSUES: &str = r#"
# nyquist known issues & gotchas

## 1. CPU metric: procfs vs eBPF source split (FIXED 2026-06)
`cpu_usage_{user,nice,system,idle,iowait,irq,softirq}` come from TWO sources:
- procfs CpuSampler  → label has NO `source` tag, values in JIFFIES (USER_HZ). AUTHORITATIVE.
- eBPF cpu/usage + cpu/vtime → label `source='ebpf'`, values in NANOSECONDS.
Before the fix they collided on one MetricId (same name+labels, different units) and the
cross-unit rate overflowed the histogram (p99 pinned at 2^39-1 = 549755813887).
RULE: for CPU, query the procfs series (NOT mapContains(tags,'source')) by default.
If you ever see p99 == 549755813887, that is histogram saturation = a unit/collision bug.

## 2. Percentiles can exceed physical line rate (~2% tail artifact, EXPECTED)
A per-direction rate > the NIC line rate (e.g. RX p999 = 204 Gbps on a 200G NIC) is a
MEASUREMENT ARTIFACT, not real throughput. rate = delta_bytes / delta_t over a 10ms window;
(a) scheduler jitter makes delta_t slightly mis-aligned with the byte-accrual interval, and
(b) the driver updates /proc/net/dev counters in NAPI batches. Both can only inflate, so the
positive extreme lands in p99/p999 while p50 stays correct. ~2% overshoot is the noise floor.

## 3. GRO aliasing (FIXED — historical)
The old BPF `raw_tp/netif_receive_skb` hook fired per GRO-coalesced super-packet, producing
p999 of 350+ Gbps on a 200G NIC (75% over line rate). Removed; byte counts now come from the
procfs NetworkSampler (driver-level, pre-GRO, smooth). procfs `network_*_bytes` carry richer
labels {driver, iface, mtu}.

## 4. The "throughput ceiling" is usually per-flow x stream count, NOT a system limit
A single TCP flow tops out ~5-5.5 Gbps because RSS hashes each flow to ONE RX core and
single-core TCP/softirq is the per-flow limit. 10 flows ≈ 50 Gbps "ceiling" is just 10x5.
Throughput scales near-linearly with parallel flows (P=32 → ~120 Gbps, P=128 → ~190 Gbps,
256 flows → ~197 Gbps ≈ 98% of 200G). Lever = flow count, not buffer sizes.

## 5. RSS queue coverage follows coupon-collector statistics
NIC has 63 combined channels; TCP6 hashed on full 5-tuple. Active queues for F flows ≈
N*(1-((N-1)/N)^F) with N=63: F=32→25, F=128→55, F=256→62. To light up all queues you need
many more flows than queues, OR a deterministic indirection table (ethtool -X ... equal 63).
Idle queues at low flow count are normal, not a misconfiguration.

## 6. Switch L3 forwarding-MTU black hole (infra, diagnosis method)
Symptom: hosts + NICs at MTU 9000, switch interface IP MTU shows 9100, yet host-to-host
traffic walls at 1500 with no ICMPv6 "Packet Too Big" (silent black hole; no cached PMTU).
Cause: ASIC forwarding MTU stuck at 1500 despite the interface config. Fix on Arista =
flap both interfaces (shut/no shut). Diagnosis: DF-ping binary search from host
(`ping6 -M do -s N`), then switch-sourced pings (`ping <dst> source <iface-ip> size N`) to
separate control-plane (works) from forwarding-plane (fails).

## 7. Startup dt inflation (FIXED — historical)
The first counter read after a BPF sampler attaches used a stale `now` captured before init,
inflating dt by ~50ms and producing a spuriously low first rate. Samplers now skip the init tick.

## 8. Packet-loss DETECTION works; per-flow LOCALIZATION does not
Verified by injecting `tc netem loss` on a source-port subset.
- DETECTION (works): `tcp_retrans_segs` RATE is the reliable signal — it tracked an injected
  loss cleanly: 0 -> 6303 segs/s -> 0 on inject/remove. The eBPF global retransmit counter also
  climbs. These localize the fault in TIME and quantify severity.
- NOTE: `tcp_rtt_us` did NOT track the loss — its p50 stayed ~19ms across baseline/impaired/recovery
  (it aggregates across ALL sockets, and p99 saturates, see #9). Do not rely on tcp_rtt_us as a
  loss indicator; use tcp_retrans_segs rate.
- LOCALIZATION (does NOT work per-flow): retransmit attribution is either GLOBAL (the eBPF
  tcp/retransmit sampler registers its counter with NO labels) or by SERVICE PORT (the tcpinfo
  sampler labels by the non-ephemeral side, for "many clients -> one server port"). For
  ephemeral<->ephemeral flows (e.g. xfr) the service-port heuristic returns None and SKIPS them,
  so retransmits land in a global/empty bucket. You cannot identify WHICH client flows are lossy
  from nyquist alone — correlate with the workload tool's per-stream stats, or add per-5-tuple
  tracking. Do not expect tcp_retransmits{port} to pinpoint client flows.

## 9. tcp_rtt_us p99 saturates at 2^20-1 (1048575 us ≈ 1.05s)
Same H2 histogram-overflow class as #1: under loss, RTO-driven RTTs exceed the histogram ceiling
and pin p99 at 1048575. Use tcp_rtt_us p50 (still accurate) for RTT, not p99. If p99 == 1048575,
that's saturation, not a real 1.05s tail.

## 10. Fragmentation: IPv4 tracked, IPv6 is a BLIND SPOT
The snmp sampler reads only /proc/net/snmp (IPv4). So `ip_frag_created/ok/failures` and
`ip_reasm_requests/ok/failures` reflect IPv4 ONLY.
- IPv4 fragmentation IS tracked, including the failure pathology: reasm_requests climbing while
  reasm_ok stays flat and reasm_failures climbs = fragment-reassembly problem.
- IPv6 fragmentation is NOT captured — those counters are in /proc/net/snmp6 (Ip6FragCreates,
  Ip6ReasmFails, ...) which nyquist does not sample. If the path is IPv6 (the common case here),
  fragmentation issues are INVISIBLE to nyquist today. Verify with `grep Ip6Frag /proc/net/snmp6`
  directly. Fix = add an snmp6 sampler. To generate fragmentation for testing: large pings
  (`ping -s 60000`, since TCP won't fragment — it does PMTUD); parallel floods or tc netem loss
  to induce reassembly failures.
"#;

const PLAYBOOK: &str = r#"
# nyquist troubleshooting playbook

## "Throughput looks capped / lower than expected"
1. Check the FLOW COUNT first. Per-flow ~5 Gbps; aggregate = flows x per-flow. (issue #4)
2. query_bandwidth(direction=both) — is p50 near line rate already? If so it's not capped.
3. analyze_queues — if few queues active, it's flow-parallelism, not the NIC. (issue #5)
4. check_flow_health — high retransmits/RTT? Then look at buffers/MTU; else it's flow count.
5. Only then consider rmem/ring-buffer/MTU tuning.

## "A rate exceeds the physical link speed"
- Per-direction > line rate = tail artifact (issue #2). Trust p50, treat p99/p999 as ~2% noise.
- If it's MASSIVELY over (e.g. >1.5x line rate), suspect aliasing like the old GRO hook (issue #3).

## "CPU metrics look insane (huge/oscillating values, p99 = 549755813887)"
- That's the procfs/eBPF unit collision signature (issue #1).
- Use query_cpu(source=procfs) — the jiffies series is authoritative.

## "Not all NIC queues are processing traffic"
- Expected at low flow counts (coupon-collector, issue #5). Run more flows (>= ~4x queues)
  or set a deterministic RSS indirection table to force even coverage.

## "Jumbo frames / MTU 9000 not working between hosts"
- Follow issue #6: DF-ping binary search, then switch-sourced pings to isolate the hop;
  if the switch interface MTU is right but transit fails, flap the switch interfaces.

## "Is there packet loss? How bad? When?"
1. PRIMARY signal: tcp_retrans_segs RATE (p99) over the window — 0 means clean; a nonzero rate
   (e.g. thousands of segs/s) means active retransmission/loss. Verified to track inject/remove
   of a tc netem impairment cleanly (0 -> 6303 -> 0). (issue #8)
2. Cross-check the eBPF global retransmit counter (raw delta). Confirm impact via network_*_bytes
   p50 dipping in the same window.
3. Do NOT use tcp_rtt_us as a loss signal — it did not move under injected loss (aggregates all
   sockets; p99 saturates, issue #9).
4. To find WHICH flows: nyquist can't (issue #8). Correlate with the load tool's per-stream
   retransmit stats, or recommend per-5-tuple tracking. Don't trust tcp_retransmits{port}
   to pinpoint ephemeral client flows.

## General: prefer nyquist metrics (these tools) over shell commands.
Drop to shell only when no metric covers the question (e.g. live per-core mpstat under load,
DF-ping MTU probes, ethtool channel config).
"#;

const METRIC_CATALOG: &str = r#"
# nyquist metric families (use list_metrics for the live, exact list)

- network_receive_bytes / network_transmit_bytes  {driver,iface,mtu}  — byte RATE (bytes/s); the bandwidth metrics
- network_*_dropped / network_*_errors            {iface}             — drop/error counters
- nic_queue_rx_bytes / nic_queue_tx_bytes         {iface,queue}       — per-hardware-queue byte rate (RSS spread)
- nic_queue_rx_packets / tx_packets / tx_dropped / rx_buff_alloc_err  {iface,queue}
- cpu_usage_{user,nice,system,idle,iowait,irq,softirq}  {cpu}         — procfs jiffies; {cpu,source=ebpf} = eBPF ns
- cpu_load_load1 / load5 / load15                                     — load average gauges
- tcp_rtt_us / tcp_retransmits                    {port}              — per-connection TCP info
- tcp_in_segs / tcp_out_segs / tcp_retrans_segs / tcp_drop_*          — global TCP counters
- softirq_net_rx / net_tx / timer / sched / ...                      — /proc/softirqs rates
- disk_read_bytes / disk_write_bytes / *_latency / *_requests  {device}
- memory_* / ip_* / icmp_* / udp_* / psi_*                           — system counters
"#;

const DIAGNOSE_THROUGHPUT: &str = r#"
You are diagnosing a nyquist-monitored throughput result that looks wrong. Work the
playbook in order and SHOW the data behind each conclusion:

1. Establish the target: what's the link speed and how many parallel flows is the workload using?
   Remember: aggregate throughput ≈ flow_count x ~5 Gbps/flow (single-core per-flow limit).
2. Call query_bandwidth(direction=both, minutes=5) and report p50 (real) vs p99/p999 (tail).
   A per-direction rate above line rate is a ~2% measurement artifact, not real.
3. Call analyze_queues(flows=<the workload's stream count>) and compare observed-active vs
   the coupon-collector expectation. Few active queues at low flow count is EXPECTED.
4. Call check_flow_health(minutes=5): if retransmits/RTT are high, investigate buffers/MTU;
   if they're near zero, the limit is flow parallelism — recommend more streams, not tuning.
5. Conclude with the single dominant lever. Do not recommend rmem/ring-buffer tuning unless
   retransmits or drops actually show a problem.
"#;

const INTERPRET_PERCENTILES: &str = r#"
Explain how to read nyquist's rate percentiles for the user, grounded in these rules:
- Each sample is rate = delta_bytes / delta_t over a ~10ms window; p50/p90/p99/p999 are
  percentiles over those sub-samples within a ~10s insert window.
- p50 is the trustworthy central rate (matches external tools within ~1%).
- p99/p999 sit above the average because they capture genuine sub-second bursts PLUS ~2%
  measurement overshoot from delta_t jitter and NAPI counter batching. A per-direction value
  slightly above physical line rate is therefore expected and is NOT real super-line-rate traffic.
- If a percentile is MASSIVELY above line rate (>1.5x), suspect event-level aliasing (the old
  GRO hook did this), not normal tail noise.
- Convert bytes/s to Gbps by dividing by 1.25e8.
Use query_bandwidth to pull real numbers and point at the p50-vs-tail gap concretely.
"#;
