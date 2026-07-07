// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/tcp/linux/packet_latency/mod.bpf.c
// (MIT OR Apache-2.0); based on tcppktlat.bpf.c from the BCC project.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_ENTRIES 10240
#define NO_EXIST 1

// Per-socket entry timestamp. Key is the u64 cast of `struct sock*` — the
// documented pointer-key exception (design §5.1): no bounded integer index
// exists for a live socket, so a HASH keyed by sock* is used.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, u64);
    __type(value, u64);
} start SEC(".maps");

// In-kernel H2 histogram of RTT in MICROSECONDS (the metric is tcp/rtt_us;
// bucketing raw ktime ns here made every bucket bound 1000x the implied
// unit and saturated the top of the histogram range on LAN RTTs).
// BPF_F_MMAPABLE: userspace reads via mmap.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} latency SEC(".maps");

static __always_inline u64 sock_ident(struct sock *sk) { return (u64)sk; }

static int handle_tcp_probe(struct sock *sk, struct sk_buff *skb) {
    const struct tcphdr *th = (const struct tcphdr *)BPF_CORE_READ(skb, data);
    u64 doff = BPF_CORE_READ_BITFIELD_PROBED(th, doff);
    u64 len = BPF_CORE_READ(skb, len);
    if (len <= doff * 4) return 0; // pure ACK, no data
    u64 id = sock_ident(sk), ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&start, &id, &ts, NO_EXIST);
    return 0;
}

static int handle_rcv_space_adjust(struct sock *sk) {
    u64 id = sock_ident(sk);
    u64 *tsp = bpf_map_lookup_elem(&start, &id);
    if (!tsp) return 0;
    u64 now = bpf_ktime_get_ns();
    if (*tsp <= now) {
        // ns -> us before bucketing; see histogram map comment.
        histogram_incr(&latency, HISTOGRAM_POWER, (now - *tsp) / 1000);
    }
    bpf_map_delete_elem(&start, &id);
    return 0;
}

static int handle_destroy_sock(struct sock *sk) {
    u64 id = sock_ident(sk);
    bpf_map_delete_elem(&start, &id);
    return 0;
}

SEC("raw_tp/tcp_probe")
int BPF_PROG(tcp_probe, struct sock *sk, struct sk_buff *skb) { return handle_tcp_probe(sk, skb); }

SEC("raw_tp/tcp_rcv_space_adjust")
int BPF_PROG(tcp_rcv_space_adjust, struct sock *sk) { return handle_rcv_space_adjust(sk); }

SEC("raw_tp/tcp_destroy_sock")
int BPF_PROG(tcp_destroy_sock, struct sock *sk) { return handle_destroy_sock(sk); }

char LICENSE[] SEC("license") = "GPL";
