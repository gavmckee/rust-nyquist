// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/tcp/linux/retransmit/mod.bpf.c (MIT OR Apache-2.0).
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

// Per-CPU banks; each CPU writes only its own slot (no contention).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

SEC("kprobe/tcp_retransmit_skb")
int BPF_KPROBE(tcp_retransmit_skb, struct sock *sk, struct sk_buff *skb, int segs) {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();
    array_add(&counters, idx, (u64)(segs > 0 ? segs : 1));
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
