// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/network/linux/traffic/mod.bpf.c
// (MIT OR Apache-2.0). Extended with per-interface PERCPU_HASH tracking.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define MAX_IFACES 256

// Per-CPU per-interface byte and drop counters.
// Key: u32 ifindex. Value: iface_counters (one copy per CPU, no atomics needed).
// Userspace aggregates across CPUs via bpf_map_lookup_percpu.
struct iface_counters {
    __u64 rx_bytes;
    __u64 tx_bytes;
    __u64 rx_dropped;
    __u64 tx_dropped;
};

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_HASH);
    __uint(max_entries, MAX_IFACES);
    __type(key, __u32);
    __type(value, struct iface_counters);
} iface_counters_map SEC(".maps");

// Look up (or create) the current CPU's counter slot for `ifindex`.
static __always_inline struct iface_counters *get_counters(__u32 ifindex) {
    struct iface_counters *c = bpf_map_lookup_elem(&iface_counters_map, &ifindex);
    if (!c) {
        struct iface_counters zero = {};
        // BPF_NOEXIST: only creates if absent; races with another CPU creating
        // the same entry are safe because each CPU owns its own PERCPU slot.
        bpf_map_update_elem(&iface_counters_map, &ifindex, &zero, BPF_NOEXIST);
        c = bpf_map_lookup_elem(&iface_counters_map, &ifindex);
    }
    return c;
}

// RX bytes: fires when a packet enters the network stack from a device.
SEC("raw_tp/netif_receive_skb")
int BPF_PROG(netif_receive_skb, struct sk_buff *skb) {
    if (!skb) return 0;
    struct net_device *dev = BPF_CORE_READ(skb, dev);
    if (!dev) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);
    __u64 len = BPF_CORE_READ(skb, len);

    struct iface_counters *c = get_counters(ifindex);
    if (c) c->rx_bytes += len;
    return 0;
}

// TX bytes + TX dropped: fires after the driver attempts to transmit an skb.
// `rc` is the return code from ndo_start_xmit; 0 = success, non-zero = dropped.
SEC("raw_tp/net_dev_xmit")
int BPF_PROG(net_dev_xmit, struct sk_buff *skb, int rc,
             struct net_device *dev, unsigned int skb_len) {
    if (!dev) return 0;
    __u32 ifindex = BPF_CORE_READ(dev, ifindex);

    struct iface_counters *c = get_counters(ifindex);
    if (!c) return 0;

    if (rc == 0) {
        c->tx_bytes += skb_len;
    } else {
        c->tx_dropped++;
    }
    return 0;
}

// RX dropped: fires when an skb is freed due to a drop (not a normal consume).
// We count only RX-path drop reasons; TX-path drops are counted via net_dev_xmit.
SEC("raw_tp/kfree_skb")
int BPF_PROG(kfree_skb, struct sk_buff *skb, void *location,
             enum skb_drop_reason reason) {
    if (!skb) return 0;

    // Filter for RX-path drop reasons. SKB_DROP_REASON_NOT_SPECIFIED (2) and
    // reasons > SKB_DROP_REASON_MAX are excluded as non-specific.
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

    struct iface_counters *c = get_counters(ifindex);
    if (c) c->rx_dropped++;
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
