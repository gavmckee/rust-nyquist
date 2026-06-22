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
| `cpu/usage/idle`, `cpu/usage/iowait`, `cpu/usage/irq`, `cpu/usage/softirq` | yes — per CPU | `cpu/usage/idle{cpu="cpu0"}` | `/proc/stat` | BPF `cpu/usage` (follow-on — needs sched_switch + irq tracepoints; vtime kernels skip `cpuacct_account_field` for these) | Retained (follow-on) |
| `cpu/load/{1,5,15}` | no | `cpu/load{window="1"}` | `/proc/loadavg` | retained-procfs | Retained |
| `memory/<field>` | yes — per `/proc/meminfo` field, lower-cased | `memory/memfree`, `memory/memtotal`, `memory/cached`, `memory/buffers`, … | `/proc/meminfo` | retained-procfs (acknowledged drift) | Retained |
| `psi/{cpu,memory,io}/{some,full}_avg{10,60,300}` | partly — `full` absent for cpu | `psi/cpu/some_avg10` | `/proc/pressure/*` | retained-procfs | Retained |
| `softirq/<type>` | yes — per softirq line, lower-cased | `softirq/timer`, `softirq/net_rx`, `softirq/net_tx`, `softirq/sched`, `softirq/rcu`, … | `/proc/softirqs` | BPF or retained-procfs (decide in follow-on) | Retained |
| `network/receive/bytes`, `network/transmit/bytes` | yes — per interface (`iface`) | `network/receive/bytes{iface="ens3"}` | `/proc/net/dev` | BPF `network/traffic` (`raw_tp/netif_receive_skb` + `raw_tp/net_dev_xmit`, mmap ARRAY) | **Done** |
| `network/receive/dropped`, `network/transmit/dropped` | yes — per interface | `network/receive/dropped{iface="ens3"}` | `/proc/net/dev` | BPF `network/traffic` (`kfree_skb` RX reasons + `net_dev_xmit` rc≠0) | **Done** |
| `network/receive/errors`, `network/transmit/errors` | yes — per interface | `network/receive/errors{iface="ens3"}` | `/proc/net/dev` | driver-level hardware counters — no generic BPF hook; retained-procfs | Retained (follow-on) |
| **Note** | | Label simplified to `{iface}` only; procfs uses `{iface,driver,mtu}`. Disable `[samplers.network]` and enable `[samplers."network/traffic"]` to switch. | | | |
| `disk/{read,write}/{bytes,requests,latency}` | yes — per device (`device`) | `disk/read/bytes{device="vda"}`, `disk/read/latency{device="vda"}` | `/proc/diskstats` | BPF `disk/blockio` (`raw_tp/block_rq_complete`, mmap ARRAYs; adds per-device latency histograms) | **Done** |
| `ip/*` | no | `/proc/net/snmp` Ip section | `/proc/net/snmp` | retained-procfs or BPF | Retained |
| `tcp/*` (snmp) | no | `tcp/active_opens`, `tcp/{in,out}_segs` | `/proc/net/snmp` | retained-procfs or BPF | Retained |
| `udp/*` | no | `udp/{in,out}_datagrams`, `udp/in_errors` | `/proc/net/snmp` | retained-procfs | Retained |
| `tcp/{retrans,window,drop,dsack,ofo,abort}/*` | partly | `/proc/net/netstat` TcpExt | `/proc/net/netstat` | retained-procfs or BPF | Retained |
| `net/sockets/*` | no | `net/sockets/tcp_inuse`, `net/sockets/tcp_tw` | `/proc/net/sockstat` | retained-procfs | Retained |
| `nic/*`, `nic/queue/*` | yes — per interface, ethtool-discovered | `nic/queue/rx_packets{iface,queue}` | ethtool ioctl | retained (ethtool discovery) | Retained |
| `perf/hw/*`, `perf/sw/*` | yes — per CPU | `perf/hw/instructions{cpu="0"}` | `perf_event_open` | perf (already aligned) | Keep |
| `tcp/rtt_us` | yes — per port | `tcp/rtt_us{port="443"}` | Aya eBPF `ebpf/tcp/rtt_us` | BPF `tcp/packet_latency` (`raw_tp/tcp_probe`, mmap H2 histogram) | **Done** |
| `tcp/retransmits` | yes — per port | `tcp/retransmits{port="443"}` | Aya eBPF `ebpf/tcp/retransmits` | BPF `tcp/retransmit` (`kprobe/tcp_retransmit_skb`, mmap ARRAY) | **Done** |
| `tcp/rtt_us`, `tcp/retransmits` (tcpinfo) | yes — per port | via `INET_DIAG` netlink | `tcpinfo` sampler | retained until BPF parity | Retained |

## Notes

- **Rename mapping (TCP slice).** The Aya sampler emitted `ebpf/tcp/rtt_us` and
  `ebpf/tcp/retransmits`. The libbpf replacement emits `tcp/rtt_us` and
  `tcp/retransmits` (aligning with the existing `tcpinfo` names). Any such rename
  is recorded as an explicit baseline mapping before the Aya sampler is deleted
  (design §6.5, Plan B Task B9) — never a silent drop.
- **The only rows implemented in the foundation effort** are the two TCP rows
  (Plan B). All other rows are realized by their own follow-on specs, which
  inherit this contract.
- **Dynamic families** (`cpu/usage/*` per CPU, `memory/*` per field, `psi/*`,
  `softirq/*`, `network/*`/`disk/*`/`nic/*` per device, `perf/*` per CPU) must be
  captured from a *live* agent for the golden baseline (design §6.2); a source
  grep cannot see them.
