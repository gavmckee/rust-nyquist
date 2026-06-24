// SPDX-License-Identifier: GPL-2.0
// BPF sampler for block I/O: bytes, requests, and latency per device.
// Principle 2: counters and latency histogram are in BPF_F_MMAPABLE ARRAYs;
//   userspace reads via mmap — no bpf_map_lookup_elem on the hot path.
// Principle 8: counters/latency are ARRAY-indexed by device slot.
//   devt_to_slot is a HASH with documented sparse-key exception: dev_t is not a
//   dense bounded integer (sparse major/minor space), analogous to the sock*
//   exception in tcp/packet_latency.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_DEVICES         64
#define HISTOGRAM_BUCKETS   HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER     3

// devt_to_slot: dev_t (u32) → device slot (u32, 0..MAX_DEVICES-1).
// Populated by userspace at sampler init; read-only in the hot path.
// Sparse-key exception: dev_t encodes (major << 20 | minor) and is not
// suitable as a direct array index.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1024);
    __type(key, u32);
    __type(value, u32);
} devt_to_slot SEC(".maps");

// counters[slot * COUNTER_GROUP_WIDTH + type]:
//   0 — read_bytes
//   1 — write_bytes
//   2 — read_requests
//   3 — write_requests
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_DEVICES * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// latency[slot * 2 * HISTOGRAM_BUCKETS + dir * HISTOGRAM_BUCKETS + bucket]:
//   dir 0 — read latency (ns)
//   dir 1 — write latency (ns)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_DEVICES * 2 * HISTOGRAM_BUCKETS);
} latency SEC(".maps");

// block_rq_complete fires on every block I/O completion.
// rq->start_time_ns is the kernel-stamped submission time — no start hash needed.
SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete, struct request *rq, int error,
             unsigned int nr_bytes) {
    struct block_device *part = BPF_CORE_READ(rq, part);
    if (!part) return 0;

    dev_t devt = BPF_CORE_READ(part, bd_dev);
    u32 *slot_p = bpf_map_lookup_elem(&devt_to_slot, &devt);
    if (!slot_p) return 0;
    u32 slot = *slot_p;
    if (slot >= MAX_DEVICES) return 0;

    // REQ_OP_MASK = lower 8 bits of cmd_flags; REQ_OP_READ=0, REQ_OP_WRITE=1.
    u32 op    = (u32)BPF_CORE_READ(rq, cmd_flags) & 0xFF;
    u32 bytes = BPF_CORE_READ(rq, __data_len);
    u64 start = BPF_CORE_READ(rq, start_time_ns);
    u64 now   = bpf_ktime_get_ns();
    u64 delta = (now > start) ? (now - start) : 0;

    // counters layout: 0=read_bytes, 1=write_bytes, 2=read_requests, 3=write_requests
    u32 is_write = (op != 0) ? 1 : 0;
    array_add(&counters, slot * COUNTER_GROUP_WIDTH + is_write,     (u64)bytes); // 0 or 1
    array_add(&counters, slot * COUNTER_GROUP_WIDTH + 2 + is_write, 1);          // 2 or 3

    u32 dir = is_write; // 0=read, 1=write — reuse for latency offset

    // latency histogram — offset into the per-device, per-direction slice
    u32 hist_idx = value_to_index(delta, HISTOGRAM_POWER);
    u32 base     = slot * 2 * HISTOGRAM_BUCKETS + dir * HISTOGRAM_BUCKETS;
    array_incr(&latency, base + hist_idx);

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
