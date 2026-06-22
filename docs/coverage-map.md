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
| `cpu/usage/{user,nice,system,idle,iowait,irq,softirq}` | yes — per CPU (`cpu=N`) | `cpu/usage/user{cpu="0"}` | `/proc/stat` | BPF `cpu/usage` | Retained (follow-on) |
| `cpu/load/{1,5,15}` | no | `cpu/load{window="1"}` | `/proc/loadavg` | retained-procfs | Retained |
| `memory/<field>` | yes — per `/proc/meminfo` field, lower-cased | `memory/memfree`, `memory/memtotal`, `memory/cached`, `memory/buffers`, … | `/proc/meminfo` | retained-procfs (acknowledged drift) | Retained |
| `psi/{cpu,memory,io}/{some,full}_avg{10,60,300}` | partly — `full` absent for cpu | `psi/cpu/some_avg10` | `/proc/pressure/*` | retained-procfs | Retained |
| `softirq/<type>` | yes — per softirq line, lower-cased | `softirq/timer`, `softirq/net_rx`, `softirq/net_tx`, `softirq/sched`, `softirq/rcu`, … | `/proc/softirqs` | BPF or retained-procfs (decide in follow-on) | Retained |
| `network/{receive,transmit}/{bytes,errors,dropped}` | yes — per interface | `network/receive/bytes{iface="eth0"}` | `/proc/net/dev` | BPF `network/{traffic,interfaces}` | Retained (follow-on) |
| `disk/{read,write}/bytes` | yes — per device | `disk/read/bytes{device="sda"}` | `/proc/diskstats` | BPF `blockio/{latency,requests}` | Retained (follow-on) |
| `ip/*` | no | `/proc/net/snmp` Ip section | `/proc/net/snmp` | retained-procfs or BPF | Retained |
| `tcp/*` (snmp) | no | `tcp/active_opens`, `tcp/{in,out}_segs` | `/proc/net/snmp` | retained-procfs or BPF | Retained |
| `udp/*` | no | `udp/{in,out}_datagrams`, `udp/in_errors` | `/proc/net/snmp` | retained-procfs | Retained |
| `tcp/{retrans,window,drop,dsack,ofo,abort}/*` | partly | `/proc/net/netstat` TcpExt | `/proc/net/netstat` | retained-procfs or BPF | Retained |
| `net/sockets/*` | no | `net/sockets/tcp_inuse`, `net/sockets/tcp_tw` | `/proc/net/sockstat` | retained-procfs | Retained |
| `nic/*`, `nic/queue/*` | yes — per interface, ethtool-discovered | `nic/queue/rx_packets{iface,queue}` | ethtool ioctl | retained (ethtool discovery) | Retained |
| `perf/hw/*`, `perf/sw/*` | yes — per CPU | `perf/hw/instructions{cpu="0"}` | `perf_event_open` | perf (already aligned) | Keep |
| **`tcp/rtt_us`** | yes — per port | `tcp/rtt_us{port="443"}` | Aya eBPF `ebpf/tcp/rtt_us` | **BPF `tcp/packet_latency`** | **In progress (Plan B)** |
| **`tcp/retransmits`** | yes — per port | `tcp/retransmits{port="443"}` | Aya eBPF `ebpf/tcp/retransmits` | **BPF `tcp/retransmit`** | **In progress (Plan B)** |
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
