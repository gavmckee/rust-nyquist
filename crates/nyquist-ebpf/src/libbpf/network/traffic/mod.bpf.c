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
//   slot 0 — rx_bytes
//   slot 1 — tx_bytes
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

// RX bytes: fires when a packet enters the network stack from a device.
SEC("raw_tp/netif_receive_skb")
int BPF_PROG(netif_receive_skb, struct sk_buff *skb) {
    if (!skb) return 0;
    struct net_device *dev = BPF_CORE_READ(skb, dev);
    if (!dev) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);
    if (ifindex >= MAX_IFINDEX) return 0;
    __u64 len = BPF_CORE_READ(skb, len);
    array_add(&counters, ifindex * COUNTER_GROUP_WIDTH + 0, len);
    return 0;
}

// TX bytes + TX dropped: fires after the driver attempts to transmit an skb.
SEC("raw_tp/net_dev_xmit")
int BPF_PROG(net_dev_xmit, struct sk_buff *skb, int rc,
             struct net_device *dev, unsigned int skb_len) {
    if (!dev) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);
    if (ifindex >= MAX_IFINDEX) return 0;
    if (rc == 0) {
        array_add(&counters, ifindex * COUNTER_GROUP_WIDTH + 1, (__u64)skb_len);
    } else {
        array_add(&counters, ifindex * COUNTER_GROUP_WIDTH + 3, 1);
    }
    return 0;
}

// RX dropped: fires when an skb is freed due to a drop (not a normal consume).
// Only RX-path drop reasons are counted; TX drops are counted via net_dev_xmit.
SEC("raw_tp/kfree_skb")
int BPF_PROG(kfree_skb, struct sk_buff *skb, void *location,
             enum skb_drop_reason reason) {
    if (!skb) return 0;
    switch (reason) {
    case SKB_DROP_REASON_CPU_BACKLOG:
    case SKB_DROP_REASON_SOCKET_RCVBUFF:
    case SKB_DROP_REASON_PROTO_MEM:
    case SKB_DROP_REASON_NO_SOCKET:
    case SKB_DROP_REASON_SOCKET_BACKLOG:
    case SKB_DROP_REASON_NETFILTER_DROP:
    case SKB_DROP_REASON_TC_INGRESS:
    case SKB_DROP_REASON_UNHANDLED_PROTO:
        break;
    default:
        return 0;
    }
    struct net_device *dev = BPF_CORE_READ(skb, dev);
    if (!dev) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);
    if (ifindex >= MAX_IFINDEX) return 0;
    array_add(&counters, ifindex * COUNTER_GROUP_WIDTH + 2, 1);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
