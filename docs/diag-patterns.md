# Diagnostic Patterns

Rules of thumb for using nyquist metrics to identify recurring failure modes. Each pattern lists the metric shape that distinguishes the cause, plus the operational implication.

## Block IO error mix

The composition of `blockio_errors` is more diagnostic than absolute counts. The error label buckets `blk_status_t` into seven coarse classes; the *shape* of the mix points at the cause.

| Pattern | Likely cause | Where to look next |
|---|---|---|
| `error="timeout"` rising, `error="io"` flat | Transport or controller blackhole — NVMe controller hung, NVMe-oF transport silent, iSCSI target unreachable, hypervisor not reattaching an EBS volume | `dmesg`, `/proc/interrupts` for IRQ delivery, `nvme list` |
| `error="io"` and `error="target"` together | Real device failure — failing media, namespace going read-only, SCSI EH exhausted retries | SMART data, `dmesg`, kernel log for `blk_update_request` errors |
| `error="nospc"` non-zero | Thin-provisioned storage out of physical capacity (dm-thin, VMDK datastore, RBD pool past `full_ratio`) — *not* a filesystem-full condition | The abstraction layer above the disk; FS metrics will show plenty of room |
| `error="protection"` non-zero (any rate) | T10 PI / DIF/DIX or NVMe end-to-end protection check failed. Treat as a data-integrity alarm even at very low rates | Hardware logs, ECC counters, controller PI configuration |
| `error="unsupported"` increasing | Configuration drift — feature toggled off, firmware update dropped a capability, kernel upgrade changed the discard/write-zeroes path | Recent infra changes, `nvme id-ctrl`, queue feature attributes |
| `blockio_requeues` rising, `blockio_errors` flat | Multipath or transport recovery absorbing faults cleanly — system is healthy, path is flaky | Multipath status, NVMe-oF connection counts, `dmesg` for path events |
| `blockio_requeues` and `blockio_errors` both rising | Recovery exhausted — paths failed for real | Same as above, plus prepare for failover |

PromQL pattern:

```
sum by (error) (irate(blockio_errors[5m]))      # mix shape
sum (irate(blockio_requeues[5m]))               # recovery rate
```

## IRQ pinning misconfiguration

Block IO completions and network packet processing run in softirq context on whichever CPU received the hardware IRQ. Lopsided per-CPU softirq distribution is the canonical signal that interrupt affinity isn't doing what the topology wants.

| Pattern | Likely cause |
|---|---|
| `softirq{kind="block"}` concentrated on one or two CPUs | Single-queue HBA driver, `smp_affinity` mask too narrow, or `irqbalance` failed/disabled. blk-mq devices should spread across many vCPUs naturally |
| `softirq{kind="net_rx"}` concentrated on one CPU | RPS/RFS misconfigured, RSS not enabled on the NIC, or single-queue NIC driver |
| Non-zero `softirq` on a CPU listed in `isolcpus` | IRQ leakage onto an isolated CPU — check `/proc/irq/*/smp_affinity_list` |
| `cpu_tlb_flush{reason="remote_send_ipi"}` non-zero on an isolated CPU | Cross-CPU `mm_struct` sharing — another CPU's `munmap`/`mprotect` is shooting down TLB entries on the isolated CPU |

PromQL pattern:

```
# Per-CPU softirq distribution — heatmap shape, look for hotspots
sum by (id) (irate(cpu_usage{state="softirq"}[5m]))

# Block-specific
sum by (id) (irate(softirq{kind="block"}[5m]))

# IPI leakage onto isolated CPUs
sum by (id) (irate(cpu_tlb_flush{reason="remote_send_ipi"}[5m]))
```

## Cloud VM contention vs self-induced overload

Two metrics decompose "my workload is queueing":

- `cpu_usage{state="steal"}` — vCPU was descheduled by the hypervisor (host-side cause).
- `scheduler_runqueue_wait` — your task wanted to run; something on your runqueue ran instead.

| Pattern | Interpretation |
|---|---|
| High runqueue wait, near-zero steal | Self-induced — too many runnable threads for the vCPUs you have. Scale up or fix concurrency |
| High runqueue wait, high steal | Host oversubscription — noisy neighbor or the host is saturated. Move to a dedicated tier or larger SKU |
| Low runqueue wait, high steal | Hypervisor preempting you for housekeeping (live migration window, host scheduler). Often transient |
| Steal spikes correlate with IO latency spikes | vCPU was preempted during interrupt-heavy moments. Common on shared-tenancy hosts under load — cloud IRQ tax |

`cpu_l3_miss` per CPU adds one more axis: uniformly elevated across all your vCPUs (rather than concentrated on whichever ones your workload runs on) suggests external cache pressure from co-tenants — visible *without* steal time rising. Cache contention is the noisy-neighbor failure mode that doesn't show up in scheduling.

## CPU throttling vs scheduling pressure

`cgroup_cpu_throttled` and `cgroup_scheduler_runqueue_wait` answer different questions for a single cgroup:

| Pattern | Interpretation |
|---|---|
| `cgroup_cpu_throttled` non-zero, `cgroup_cpu_bandwidth_throttled_time` rising | CFS quota is the bottleneck — workload is ready to run but blocked by `cpu.max`. Raise the quota or remove it |
| `cgroup_scheduler_runqueue_wait` rising, throttling near zero | Cgroup has quota to spare, but is competing with co-located cgroups for actual CPU. Check sibling cgroups' usage |
| Both rising together | Both: quota is too tight *and* co-located workloads are competing |

These are often confused. Throttling is *policy* — quota enforcement. Runqueue wait is *contention* — multiple runnable threads sharing a CPU.


## High-bandwidth TCP receive not reaching line rate

The system is sustaining high throughput but is stuck below the NIC's rated capacity with no obvious CPU saturation, no packet drops, and a clean TCP health picture. Four distinct causes produce this pattern; the metric shapes distinguish them.

### Socket buffer ceiling (most common)

`net.core.rmem_max` caps every TCP receive socket buffer system-wide and overrides the `net.ipv4.tcp_rmem` max. If it is too small, the sender cannot keep the pipe full regardless of how much CPU headroom the receiver has.

**Signature**: throughput is stable but flat; no drops, no retransmits, no window-zero events; aggregate CPU idle >60%. The ceiling obeys:

```
throughput ≈ n_connections × rmem_max / RTT
```

Cross-check: if `n_connections × rmem_max / observed_throughput` gives a plausible datacenter RTT (100 µs – 2 ms), the buffer is the bottleneck. Linux default `rmem_max` is 208 KB; at 1 ms RTT, 69 connections × 208 KB = **14.4 MB in-flight → ~115 Gbps headroom** only if RTT is under ~115 µs. For any longer path it caps well below line rate.

| Pattern | Interpretation |
|---|---|
| Throughput flat, no drops, CPU idle >60%, RTT × connections × buffer ≈ observed ceiling | `net.core.rmem_max` (and/or `wmem_max`) is the bottleneck |
| `tcp_window_zero_sent` non-zero | Receiver buffer is full; sender is being throttled by the advertised window |
| `tcp_mem_pressure_events` non-zero | System-wide socket memory pressure — kernel is reclaiming socket buffers globally |

Fix:
```bash
sysctl -w net.core.rmem_max=16777216
sysctl -w net.core.wmem_max=16777216
sysctl -w net.ipv4.tcp_rmem="4096 131072 16777216"
sysctl -w net.ipv4.tcp_wmem="4096 87380 16777216"
```

### RSS queue imbalance

With N flows hashing to fewer active queues than expected, one CPU's softirq budget becomes the bottleneck while aggregate CPU looks idle. The signal is the per-queue byte distribution, not the CPU total.

**Signature**: `nic_rss_cv` is elevated (>30) and sustained during the test; one queue carries 2× the bytes of its peers; aggregate CPU idle is high but throughput is capped.

| Pattern | Interpretation |
|---|---|
| `nic_rss_cv` > 30, one queue > 2× peers | Flow-count too low — RSS 5-tuple hash collisions concentrating load on one queue/CPU. Increase connection count |
| `nic_rss_cv` > 50, most queues near zero | Extreme imbalance — RSS indirection table or hash key mismatched to the flow distribution. Retune `ethtool --config-ntuple` or the indirection table |
| `softirq{kind="net_rx"}` concentrated on one CPU (when BPF is available) | Confirms the hot-queue CPU is the bottleneck, not aggregate softirq |

Fix: increase connection count to add entropy to the RSS hash (128–256 flows), or reprogram the RSS indirection table to match the actual flow distribution.

### NIC ring buffer pressure

At 100 Gbps with MTU 1500, the NIC receives ~8.3 M packets/s. A small ring buffer fills in the gap between interrupts; if softirq processing falls behind, the ring overflows and the kernel counts missed packets.

**Signature**: `nic_rx_missed` or `nic_driver_rx_out_of_buffer` increments during load, even if small. At the default ring size of 1 024 on a 100 Gbps NIC, a single 120 µs gap in softirq draining overflows the ring.

| Pattern | Interpretation |
|---|---|
| `nic_rx_missed` non-zero during load, zero at idle | Ring buffer too small for the interrupt coalescing window at this packet rate |
| `nic_driver_rx_out_of_buffer` increments (same counter, driver-reported) | Hardware confirmed the ring was full when a packet arrived |

Fix:
```bash
ethtool -G <iface> rx 8192   # use the NIC's maximum
```

### MTU / packet-rate overhead

Standard 1500-byte MTU at 100 Gbps produces ~8.3 M packets/s. GRO coalesces received segments before the kernel sees them, but the NIC still DMA's each packet individually. The overhead per byte is ~6× higher than with jumbo frames (MTU 9000, ~1.4 M packets/s). This shows as elevated softirq *relative to throughput* rather than absolute saturation.

**Signature**: softirq % is disproportionately high for the observed throughput; `nic_rss_cv` and ring drops are fine; TCP is healthy. No single metric blows out — the ceiling is a diffuse CPU tax across all network queues.

Fix: enable jumbo frames end-to-end (NIC, switch port, and peer NIC):
```bash
ip link set dev <iface> mtu 9000
```

### Decision tree

```
Throughput below line rate, no drops, no retransmits?
│
├─ nic_rx_missed or nic_driver_rx_out_of_buffer > 0
│   └─► Ring buffer overflow → ethtool -G rx 8192
│
├─ nic_rss_cv > 30 and one queue >> peers
│   └─► RSS imbalance → more connections or retune indirection table
│
├─ n_connections × rmem_max / throughput ≈ plausible RTT
│   └─► Socket buffer ceiling → raise rmem_max / wmem_max
│
└─ Softirq high relative to throughput, nothing else
    └─► MTU overhead → jumbo frames
```

Collect from the sender side too: if the sender has the same `rmem_max`/`wmem_max` defaults, its send buffer limits how much it can have in flight and it will see the identical ceiling from the other direction.

ClickHouse query pattern:
```sql
-- Total RX throughput in Gbps (p99)
SELECT ts, p99 * 8 / 1e9 AS rx_gbps
FROM nyquist_live.samples
WHERE name = 'network_receive_bytes' AND tags['iface'] = 'ens1f1np1'
  AND $__timeFilter(ts)
ORDER BY ts

-- Per-queue distribution — spot the hot queue
SELECT tags['queue'] AS q, max(p99) / 1e9 AS peak_gbps
FROM nyquist_live.samples
WHERE name = 'nic_queue_rx_bytes' AND tags['iface'] = 'ens1f1np1'
  AND $__timeFilter(ts)
GROUP BY q ORDER BY peak_gbps DESC

-- RSS imbalance (CV > 30 is elevated, > 50 is severe)
SELECT ts, p99 AS rss_cv
FROM nyquist_live.samples
WHERE name = 'nic_rss_cv' AND tags['iface'] = 'ens1f1np1'
  AND $__timeFilter(ts)
ORDER BY ts

-- Ring drops — any non-zero during load is actionable
SELECT ts, raw, p99
FROM nyquist_live.samples
WHERE name = 'nic_rx_missed' AND tags['iface'] = 'ens1f1np1'
ORDER BY ts
```

## TCP retransmits as a path-quality signal

`tcp_retransmit` counts segments retransmitted because the peer didn't ack in time. The *shape* against load matters:

- **Constant low rate, scales with traffic** — normal background loss. Don't chase.
- **Spike independent of traffic load** — path event (route change, neighbor outage, transient saturation upstream).
- **Sustained elevation under load** — congestion at a fixed bottleneck or a specific peer. Pair with ENA allowance counters; a same-time spike in `bw_in_allowance_exceeded` rules out the network and points at instance sizing.

## Live-migration window detection (cloud)

A burst of `cpu_usage{state="steal"}` lasting tens of seconds, with `scheduler_offcpu` rising for *all* tasks at once and IO latency spiking briefly, usually corresponds to a hypervisor-driven live migration. Distinct from sustained noisy-neighbor steal because it ends abruptly. Worth correlating with cloud-provider maintenance windows / instance lifecycle events.

## Off-CPU time as a workload health signal

Per-cgroup `cgroup_scheduler_offcpu` counts nanoseconds spent off-CPU. Convert to a wall-time fraction:

```
sum by (id) (irate(cgroup_scheduler_offcpu[5m])) / 1e9
```

Yields 0..N where N is the number of CPUs the cgroup had threads on — total wall-time-equivalent seconds of blocking per second.

| Pattern | Interpretation |
|---|---|
| High off-CPU, low CPU usage | Workload is IO- or lock-bound. Look at `blockio_latency`, `tcp_packet_latency`, syscall latency by class |
| High off-CPU, paired with `cgroup_cpu_throttled` rising | Quota throttling is the off-CPU cause |
| High off-CPU, paired with high steal | Host preemption is part of the off-CPU cause |
| High off-CPU, none of the above | Self-induced blocking — application logic, lock contention. Profiling territory |

## MTU troubleshooting — finding the path MTU and the bottleneck hop

Use this pattern when:
- Enabling jumbo frames (MTU 9000) *decreased* throughput or increased retransmits
- Throughput is unexpectedly low and you suspect per-packet CPU overhead (MTU 1500 on a 100G+ link)
- You want to find the maximum MTU the full end-to-end path can carry without dropping or fragmenting

### Signal: jumbo frames hurt instead of help

The canonical indicator of a path MTU mismatch is retransmits rising (not falling) after increasing MTU, with CWND shrinking in response. RTT may improve (fewer packets → less NIC interrupt overhead) but throughput regresses because TCP cubic responds to the added loss events with window reduction.

| nyquist signal | Interpretation |
|---|---|
| `tcp_retrans_segs` rate increases after MTU change | Frames larger than the path supports are being silently dropped somewhere in the fabric |
| `tcp_retrans_segs` elevated, `tcp_drop_*` flat | Drops are mid-path, not local — local drops would show in `tcp_drop_rcvq`/`tcp_drop_backlog` |
| CWND (from `xfr --json`, `ss -tni`, or `tcp_info`) shrinks with larger MTU | TCP cubic has detected losses and backed off the congestion window |
| RTT improves but throughput regresses | NIC overhead reduced, but path loss rate increased — net negative |
| p99.9 RX variance widens | Larger frames make individual loss events more impactful; each retransmit is a bigger hole in the receive stream |

### Step 1 — Establish local MTU on all relevant interfaces

```bash
ip link show                          # check all interfaces
ip link show <iface>                  # confirm specific interface MTU
ethtool <iface> | grep -i mtu         # driver-reported max MTU capability
```

Expected: local interface MTU matches what you set. If not, the driver may not support jumbo frames (`ethtool` will report `Max MTU` < 9000).

### Step 2 — Probe the path MTU with binary search

Use ICMP with the DF (Don't Fragment) bit set. The kernel will receive an ICMP "Fragmentation Needed" message from the first hop that can't forward the packet — this is the bottleneck.

**IPv4:**
```bash
# ping with DF bit; payload size = MTU - 28 (20 IP + 8 ICMP headers)
ping -M do -s 8972 <peer_ipv4>        # tests MTU 9000
ping -M do -s 1472 <peer_ipv4>        # tests MTU 1500 (baseline)
```

**IPv6 (no fragmentation in the protocol; all hops must forward or drop):**
```bash
ping6 -M do -s 8952 <peer_ipv6>       # tests MTU 9000 (IPv6 header = 40 bytes)
ping6 -M do -s 1452 <peer_ipv6>       # tests MTU 1500 baseline
```

Interpret the result:
- **Succeeds** → that MTU is supported end-to-end
- **`Message too long` / `Frag needed`** → a hop on the path cannot forward this size; the ICMP error will contain the hop's MTU limit
- **Timeout (no ICMP error back)** → a firewall is blocking ICMP unreachables — use `tracepath` instead (Step 3)

**Binary search for max MTU** (automate this loop):
```bash
# Finds the largest MTU the path accepts (IPv6 example)
lo=1280; hi=9000
while [ $((hi - lo)) -gt 1 ]; do
  mid=$(( (lo + hi) / 2 ))
  payload=$(( mid - 40 ))   # subtract IPv6 header
  if ping6 -M do -c 1 -W 1 -s $payload <peer_ipv6> &>/dev/null; then
    lo=$mid
  else
    hi=$mid
  fi
done
echo "Path MTU: $lo"
```

This converges in ≤13 iterations across the 1280–9000 range.

### Step 3 — Locate the bottleneck hop

If the binary search finds a path MTU below your NIC's MTU, `tracepath` will identify which hop imposed the limit:

```bash
tracepath <peer_ipv4>                 # walks hops, reports MTU at each
tracepath6 <peer_ipv6>
```

Look for the hop where the reported MTU drops. That's the device (switch, router, or tunnel endpoint) that needs configuration.

If `tracepath` is unavailable or the path blocks probes, use `traceroute` with large UDP packets:
```bash
traceroute -F -m 30 --sendsize 8972 <peer_ipv4>
```

Hops that return `!F` (fragmentation needed) or time out immediately after a successful smaller-size hop are the bottleneck.

### Step 4 — Verify NIC and switch configuration

Once you know which hop is limiting, check the device:

**Local NIC:**
```bash
ethtool <iface> | grep -i "max mtu\|jumbo"
ip link set dev <iface> mtu 9000      # set jumbo
```

**Switch port (if you have access):**
- Cisco: `show interface <port> | include MTU`; set with `mtu 9216` under the interface
- Arista/EOS: `show interfaces <port> | grep MTU`; `mtu 9214` under interface config
- Mellanox/NVIDIA: `show interfaces ethernet <port> | include MTU`
- The switch's *system MTU* must also be raised, not just the port MTU

**Peer host:**
```bash
ssh <peer> ip link show <iface>       # confirm peer MTU matches
ssh <peer> sysctl net.ipv4.tcp_mtu_probing   # should be 1 (enabled) or 2 (forced)
```

### Step 5 — Enable TCP MTU probing as a safety net

Even with PMTUD working, black-hole routers that drop oversized packets without returning ICMP unreachables will cause silent stalls. MTU probing (RFC 4821) detects and works around these:

```bash
sysctl -w net.ipv4.tcp_mtu_probing=1   # 0=off, 1=on-when-stalled, 2=always
```

With `tcp_mtu_probing=1`, TCP will fall back to MTU 536 if it detects a black hole and probe upward. Prefer `=1` over `=2` (always probe adds overhead on clean paths).

### Step 6 — Correlate with nyquist after fix

After adjusting the bottleneck hop, re-run the load test and compare:

```sql
-- Retransmit rate before and after: should drop toward zero
SELECT toStartOfInterval(ts, INTERVAL 10 second) AS t,
       max(p99) AS retrans_p99
FROM nyquist_live.samples
WHERE name = 'tcp_retrans_segs'
ORDER BY t

-- TX throughput: should increase with larger MTU on a clean path
SELECT toStartOfInterval(ts, INTERVAL 10 second) AS t,
       round(max(p99)*8/1e9, 2) AS tx_gbps
FROM nyquist_live.samples
WHERE name = 'network_transmit_bytes' AND tags['iface'] = '<iface>'
ORDER BY t
```

A successful MTU increase on a clean path produces: retransmits near zero, RTT down 20–40%, TX throughput up 5–15% (less CPU overhead per byte), and p99.9 RX variance narrows.

### Decision tree

```
Jumbo frames enabled but throughput regressed or retransmits rose?
│
├─ ping6 -M do -s 8952 <peer> fails or returns "Message too long"
│   ├─ tracepath6 shows hop with MTU < 9000
│   │   └─► Configure that switch/router port for jumbo frames
│   └─ tracepath6 times out at a hop (black hole)
│       └─► sysctl net.ipv4.tcp_mtu_probing=1 ; contact network team
│
├─ ping6 -M do -s 8952 succeeds (path supports MTU 9000)
│   ├─ Peer host ip link shows MTU < 9000
│   │   └─► Set peer MTU: ip link set dev <iface> mtu 9000
│   └─ Both ends 9000 but retransmits still elevated
│       └─► Likely NIC driver or firmware issue; check ethtool -k <iface>
│           for LRO/GRO state; try ethtool -K <iface> lro off
│
└─ Using MTU 1500 and throughput is below line rate with high softirq?
    └─► See "MTU / packet-rate overhead" section above
        MTU 9000 reduces packets/s by 6× — enable if path supports it
```

### Real-world example (this system, 2026-06-24)

MTU was raised from 1500 → 9000 on `ens1f0np0`. A 120s bidir xfr test showed:

- TX regressed: 63.46 → 58.56 Gbps (−7.7%)
- Retransmits increased: ~2/120s → 9/120s (+350%)
- CWND shrank: 3.74 MB → 2.34 MB/stream
- RTT improved: 699 → 478 µs (−32%) — the NIC overhead benefit was real

Diagnosis: path does not fully support MTU 9000. The RTT improvement confirms the NIC side is happy, but a fabric hop is silently dropping oversized frames. Run Steps 2–4 to find which switch port needs `mtu 9216` configured.