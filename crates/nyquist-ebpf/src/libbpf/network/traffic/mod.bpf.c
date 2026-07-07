// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/network/linux/traffic/mod.bpf.c
// (MIT OR Apache-2.0). Rewritten to use BPF_MAP_TYPE_ARRAY + BPF_F_MMAPABLE:
//   Principle 2 (zero-syscall reads) — mmap, never bpf_map_lookup_elem on hot path.
//   Principle 8 (arrays over hashmaps) — ifindex is a bounded integer; use ARRAY.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

// counters[ifindex * GROUP_WIDTH + slot]:
//   slot 0 — unused (was rx_bytes; removed — GRO batching aliased BPF counts)
//   slot 1 — unused (was tx_bytes; removed — procfs NetworkSampler covers bytes)
//   slot 2 — rx_dropped
//   slot 3 — tx_dropped
#define COUNTER_GROUP_WIDTH 8
#define MAX_IFINDEX 512

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_IFINDEX * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// TX dropped: fires after the driver fails to transmit an skb (rc != 0).
// TX bytes (rc == 0 branch) are intentionally omitted — use the procfs
// NetworkSampler for byte counts. The raw_tp/netif_receive_skb hook is also
// gone; it fired on GRO-coalesced super-packets, causing aliased p999 values
// that exceeded the physical link capacity.
SEC("raw_tp/net_dev_xmit")
int BPF_PROG(net_dev_xmit, struct sk_buff *skb, int rc,
             struct net_device *dev, unsigned int skb_len) {
    if (!dev || rc == 0) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);
    if (ifindex >= MAX_IFINDEX) return 0;
    array_add(&counters, ifindex * COUNTER_GROUP_WIDTH + 3, 1);
    return 0;
}

// RX dropped: fires when an skb is freed due to a drop (not a normal consume).
// Only RX-path drop reasons are counted; TX drops are counted via net_dev_xmit.
//
// enum skb_drop_reason has been renumbered repeatedly upstream (reasons
// inserted mid-enum across 5.18..6.x), so the values MUST be CO-RE-relocated
// at load time — a switch over the vendored vmlinux.h numerals silently
// matches the wrong reasons on other kernels. The _exists guard also makes
// pre-5.17 kernels (no such enum member, garbage arg2) count nothing rather
// than garbage.
#define IS_RX_DROP(name) \
    (bpf_core_enum_value_exists(enum skb_drop_reason, name) && \
     reason == bpf_core_enum_value(enum skb_drop_reason, name))

SEC("raw_tp/kfree_skb")
int BPF_PROG(kfree_skb, struct sk_buff *skb, void *location,
             enum skb_drop_reason reason) {
    if (!skb) return 0;
    if (!(IS_RX_DROP(SKB_DROP_REASON_CPU_BACKLOG) ||
          IS_RX_DROP(SKB_DROP_REASON_SOCKET_RCVBUFF) ||
          IS_RX_DROP(SKB_DROP_REASON_PROTO_MEM) ||
          IS_RX_DROP(SKB_DROP_REASON_NO_SOCKET) ||
          IS_RX_DROP(SKB_DROP_REASON_SOCKET_BACKLOG) ||
          IS_RX_DROP(SKB_DROP_REASON_NETFILTER_DROP) ||
          IS_RX_DROP(SKB_DROP_REASON_TC_INGRESS) ||
          IS_RX_DROP(SKB_DROP_REASON_UNHANDLED_PROTO)))
        return 0;
    struct net_device *dev = BPF_CORE_READ(skb, dev);
    if (!dev) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);
    if (ifindex >= MAX_IFINDEX) return 0;
    array_add(&counters, ifindex * COUNTER_GROUP_WIDTH + 2, 1);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
