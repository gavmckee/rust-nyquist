// SPDX-License-Identifier: GPL-2.0
// BPF sampler for block I/O: bytes, requests, and latency per device.
// Principle 2: counters and latency histogram are in BPF_F_MMAPABLE ARRAYs;
//   userspace reads via mmap — no bpf_map_lookup_elem on the hot path.
// Principle 8: counters/latency are ARRAY-indexed by device slot.
//   name_to_slot is a HASH with documented sparse-key exception: disk names
//   are not dense bounded integers; see the map comment for why names (not
//   dev_t) are the key. Analogous to the sock* exception in tcp/packet_latency.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_DEVICES         64
#define HISTOGRAM_BUCKETS   HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER     3

#define DISK_NAME_LEN 32

struct disk_name {
    char name[DISK_NAME_LEN];
};

// name_to_slot: gendisk disk_name → device slot (u32, 0..MAX_DEVICES-1).
// Populated by userspace at sampler init; read-only in the hot path.
// Keyed by NAME, not dev_t: with NVMe native multipath, requests complete
// on the HIDDEN per-controller device (e.g. nvme0c0n1, dev 259:0), whose
// dev_t is not discoverable from userspace (no /sys dev file, absent from
// /sys/dev/block) — a devt-keyed map missed every real I/O on such hosts.
// disk_name is stable, names the whole disk even for partition I/O, and
// userspace can alias hidden multipath component names to their head.
// Sparse-key exception: a string key is not a dense bounded integer.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1024);
    __type(key, struct disk_name);
    __type(value, u32);
} name_to_slot SEC(".maps");

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
    // part is NULL for passthrough/admin commands (NVMe health polls etc.).
    struct block_device *part = BPF_CORE_READ(rq, part);
    if (!part) return 0;

    struct gendisk *disk = BPF_CORE_READ(part, bd_disk);
    if (!disk) return 0;

    struct disk_name key = {};
    BPF_CORE_READ_STR_INTO(&key.name, disk, disk_name);
    u32 *slot_p = bpf_map_lookup_elem(&name_to_slot, &key);
    if (!slot_p) return 0;
    u32 slot = *slot_p;
    if (slot >= MAX_DEVICES) return 0;

    // REQ_OP_MASK = lower 8 bits of cmd_flags; REQ_OP_READ=0, REQ_OP_WRITE=1.
    // Only READ and WRITE are counted: classifying every non-read op as a
    // write inflated write_bytes with multi-GB DISCARD/WRITE_ZEROES payloads
    // and booked FLUSHes as write requests.
    u32 op = (u32)BPF_CORE_READ(rq, cmd_flags) & 0xFF;
    if (op != 0 && op != 1) return 0;
    u32 is_write = op;

    // nr_bytes (tracepoint arg) is what THIS completion finished;
    // rq->__data_len is the length still outstanding at trace time.
    // Multi-segment requests complete across several events — reading
    // __data_len here over-counted bytes and double-counted requests.
    u32 remaining = BPF_CORE_READ(rq, __data_len);

    // counters layout: 0=read_bytes, 1=write_bytes, 2=read_requests, 3=write_requests
    array_add(&counters, slot * COUNTER_GROUP_WIDTH + is_write, (u64)nr_bytes); // 0 or 1

    // Request count and full-request latency only on the FINAL completion
    // (this event finishes everything still outstanding).
    if (nr_bytes >= remaining) {
        u64 start = BPF_CORE_READ(rq, start_time_ns);
        u64 now   = bpf_ktime_get_ns();
        u64 delta = (now > start) ? (now - start) : 0;

        array_add(&counters, slot * COUNTER_GROUP_WIDTH + 2 + is_write, 1); // 2 or 3

        // latency histogram — offset into the per-device, per-direction slice
        u32 hist_idx = value_to_index(delta, HISTOGRAM_POWER);
        u32 base     = slot * 2 * HISTOGRAM_BUCKETS + is_write * HISTOGRAM_BUCKETS;
        array_incr(&latency, base + hist_idx);
    }

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
