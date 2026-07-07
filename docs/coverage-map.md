# Coverage Map — Migration Contract

This file is the migration contract referenced by every follow-on Rezolus
Alignment spec (design §6.4). It enumerates **every current metric family**,
including the dynamically generated ones a source grep cannot see, mapped to a
target source. A row is *realized* (the procfs sampler retired) **only** once its
replacement emits identical `(name, label-key set, kind, unit)` tuples — verified
by the parity gate (design §6.3, §6.5). Coverage never dips; it can only gain
resolution.

Legend — **Dynamic?**: families whose member set is discovered at runtime (per
CPU, per interface, per `/proc` field) rather than statically named in source.

| Family | Dynamic? | Representative emitted names | Current source | Target source | Status |
|---|---|---|---|---|---|
| `cpu/usage/user`, `cpu/usage/nice`, `cpu/usage/system` | yes — per CPU (`cpu=N`) | `cpu/usage/user{cpu="cpu0"}` | `/proc/stat` | BPF `cpu/usage` (`kprobe/cpuacct_account_field`, mmap ARRAY) | **Done** |
| `cpu/usage/idle`, `cpu/usage/iowait`, `cpu/usage/irq`, `cpu/usage/softirq` | yes — per CPU | `cpu/usage/idle{cpu="cpu0"}` | `/proc/stat` | BPF `cpu/vtime` (`tp_btf/sched_switch` for idle+iowait, `tp_btf/irq_handler_{entry,exit}` + `tp_btf/softirq_{entry,exit}`, mmap ARRAY) | **Done** |
| `cpu/usage/steal` | yes — per CPU | `cpu/usage/steal{cpu="cpu0"}` | `/proc/stat` (field 8) | retained-procfs | Retained — hypervisor-stolen time, invisible before; no BPF equiv yet |
| `cpu/load/{1,5,15}` | no | `cpu/load{window="1"}` | `/proc/loadavg` | retained-procfs | Retained |
| `memory/<field>` | yes — per `/proc/meminfo` field, lower-cased | `memory/memfree`, `memory/memtotal`, `memory/cached`, `memory/buffers`, … | `/proc/meminfo` | retained-procfs (acknowledged drift) | Retained |
| `psi/{cpu,memory,io}/{some,full}_avg{10,60,300}` | partly — `full` absent for cpu | `psi/cpu/some_avg10` | `/proc/pressure/*` | retained-procfs | Retained |
| `softirq/<type>` | yes — per softirq line, lower-cased | `softirq/timer`, `softirq/net_rx`, `softirq/net_tx`, `softirq/sched`, `softirq/rcu`, … | `/proc/softirqs` | BPF or retained-procfs (decide in follow-on) | Retained |
| `network/receive/bytes`, `network/transmit/bytes` | yes — per interface (`iface`) | `network/receive/bytes{iface="ens3",driver,mtu}` | `/proc/net/dev` | **retained-procfs** — the BPF `network/traffic` sampler intentionally does NOT count bytes (GRO super-packet aliasing put p999 above line rate); procfs is authoritative | Retained (bytes) |
| `network/receive/dropped`, `network/transmit/dropped` | yes — per interface | `network/receive/dropped{iface="ens3"}` | `/proc/net/dev` | BPF `network/traffic` (`kfree_skb` CO-RE-relocated RX drop reasons + `net_dev_xmit` rc≠0, mmap ARRAY) | **Done** |
| `network/receive/errors`, `network/transmit/errors` | yes — per interface | `network/receive/errors{iface="ens3"}` | `/proc/net/dev` | driver-level hardware counters — no generic BPF hook; retained-procfs | Retained (follow-on) |
| **Note** | | Both procfs `network` AND BPF `network/traffic` run: procfs for bytes/errors, BPF for the drop breakdown. They do not collide (procfs adds `driver`/`mtu` labels). | | | |
| `disk/{read,write}/{bytes,requests,latency}` | yes — per device (`device`,`source="ebpf"`) | `disk/read/bytes{device="nvme0n1",source="ebpf"}` | `/proc/diskstats` | BPF `disk/blockio` (`raw_tp/block_rq_complete`, mmap ARRAYs; per-device latency histograms; keyed by gendisk NAME to catch NVMe multipath) | **Done** |
| `ip/*`, `ip6/*` | no | `ip/reasm/failures`, `ip6/frag/failures` | `/proc/net/snmp` + `/proc/net/snmp6` | retained-procfs or BPF | Retained (v6 added) |
| `icmp/*`, `icmp6/*` | no | `icmp/in_dest_unreachable`, `icmp6/in_pkt_too_big` (v6 PMTUD signal) | `/proc/net/snmp` + `/proc/net/snmp6` | retained-procfs | Retained (v6 added) |
| `tcp/*` (snmp) | no | `tcp/active_opens`, `tcp/{in,out}_segs` | `/proc/net/snmp` | retained-procfs or BPF | Retained |
| `udp/*`, `udp6/*` | no | `udp/{in,out}_datagrams`, `udp6/in_errors` | `/proc/net/snmp` + `/proc/net/snmp6` | retained-procfs | Retained (v6 added) |
| `tcp/{retrans,window,drop,dsack,ofo,abort}/*` | partly | `/proc/net/netstat` TcpExt | `/proc/net/netstat` | retained-procfs or BPF | Retained |
| `net/sockets/*` | no | `net/sockets/tcp_inuse`, `net/sockets/tcp_tw` | `/proc/net/sockstat` | retained-procfs | Retained |
| `nic/*`, `nic/queue/*` | yes — per interface, ethtool-discovered | `nic/queue/rx_packets{iface,queue}` | ethtool ioctl | retained (ethtool discovery) | Retained |
| `perf/hw/*`, `perf/sw/*` | yes — per CPU | `perf/hw/instructions{cpu="0"}` | `perf_event_open` | perf — per-CPU PERF_FORMAT_GROUP reads (one syscall/group), counters enabled + kernel-inclusive | Keep |
| `tcp/rtt_us` (BPF) | no (aggregate) | `tcp/rtt_us{source="ebpf"}` — **microseconds** | Aya eBPF `ebpf/tcp/rtt_us` | BPF `tcp/packet_latency` (`raw_tp/tcp_probe`, mmap H2 histogram; ns→µs before bucketing) | **Done** |
| `tcp/retransmits` (BPF) | no (aggregate) | `tcp/retransmits{source="ebpf"}` — segments | Aya eBPF `ebpf/tcp/retransmits` | BPF `tcp/retransmit` (`kprobe/tcp_retransmit_skb`, mmap ARRAY) | **Done** |
| `tcp/rtt_us`, `tcp/retransmits` (tcpinfo) | yes — per port | `tcp/rtt_us{port="443"}` (µs, gauge) | `tcpinfo` sampler (`INET_DIAG` v4+v6) | retained — distinct series from the BPF ones via `source` vs `port` labels | Retained |
| `nyquist/sink/export_failures` | yes — per sink | `nyquist/sink/export_failures{sink="clickhouse"}` | scheduler self-metric | — | **Self-metric** (alertable data loss) |
| `nyquist/bpf/dropped_samples` | yes — per sampler | `nyquist/bpf/dropped_samples{sampler="tcp/packet_latency"}` | BPF map-full counter | — | **Self-metric** (flow-map overflow) |

## Notes

- **Rename mapping (TCP slice).** The Aya sampler emitted `ebpf/tcp/rtt_us` and
  `ebpf/tcp/retransmits`. The libbpf replacement emits `tcp/rtt_us{source="ebpf"}`
  and `tcp/retransmits{source="ebpf"}`. The `source="ebpf"` label is load-bearing:
  it keeps the BPF aggregate series distinct from the `tcpinfo` per-`port` series
  of the same name (without it both hash to one MetricId and their differing
  units/origins alternate into one rate histogram, pinning it at 2^39-1). The
  same `source="ebpf"` split applies to `cpu/usage/*` (BPF ns vs procfs jiffies)
  and `disk/*` (BPF since-start vs procfs since-boot).
- **`source`/label collisions are the recurring hazard here.** Any BPF sampler
  that reuses a procfs metric name MUST add a distinguishing label — see the
  0ea7e7e class of bug.
- **`tcp/rtt_us` is microseconds** on both paths; the BPF sampler divides
  `ktime` ns by 1000 before bucketing (it previously bucketed raw ns under a
  `_us` name, saturating the histogram).
- **IPv6 (`ip6/*`, `icmp6/*`, `udp6/*`)** is parsed from `/proc/net/snmp6`
  (one key/value per line, unlike snmp's header+data rows), and `tcpinfo` dumps
  both `AF_INET` and `AF_INET6`. `Icmp6InPktTooBigs` is the IPv6 PMTUD signal.
- **Self-metrics** (`nyquist/sink/*`, `nyquist/bpf/*`) make the agent's own data
  loss observable — export failures and BPF map-full drops were previously
  log-only.
- **Removed from `memory/*`:** `VmallocTotal`/`VmallocChunk` — the former is the
  vmalloc address-space size (12.5 PB on 5-level x86_64), not a measurement; it
  exceeded the histogram range. An explicit removal in the golden baseline.
- **Dynamic families** must be captured from a *live* agent for the golden
  baseline (design §6.2); a source grep cannot see them.

## syswatch config-change coverage (separate surface)

These are **not** metric tuples — syswatch writes change rows to the ClickHouse
`sysconfig_changes` table (key, old→new, pid/comm attribution), a different
output from `/metrics`. Covered surfaces and their capture mechanism:

| Surface | Key namespace | Capture | Attribution |
|---|---|---|---|
| sysctl writes (`/proc/sys/net/**`, any depth) | `sysctl.*` | `fentry/proc_sys_call_handler` (≤6-ancestor "net" walk) | writing `comm`/`pid` |
| ethtool ioctl SETs | `ring.*`, `channels.*`, `coalesce.*` | `fentry/dev_ethtool` | writing `comm`/`pid` |
| ethtool **netlink** SETs (modern) | same | `fentry/ethnl_ops_begin` + `fexit/ethnl_default_set_doit` | writing `comm`/`pid` |
| link/MTU changes | `mtu.*` | `rtnetlink_rcv_msg` | writing `comm`/`pid` |
| RPS/XPS masks, IRQ affinity | `rps.*`, `xps.*`, `irq.*` | inotify (`IN_CLOSE_WRITE`) | `comm="fswatch"` (no writer id) |
| per-iface `rp_filter` | `sysctl.conf.<if>.rp_filter` | sysctl BPF (deep walk) | writing `comm`/`pid` |

A 60s full poll backstops every surface (attribution `comm="poll"`).
