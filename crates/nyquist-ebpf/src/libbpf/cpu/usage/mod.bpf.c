// SPDX-License-Identifier: GPL-2.0
// Adapted from rezolus src/agent/samplers/cpu/linux/usage/mod.bpf.c
// (MIT OR Apache-2.0). Simplified: per-CPU per-state counters only;
// no cgroup/task ring-buffer tracking.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

// Stride between CPU slots (power of 2; covers all 10 cpu_usage_stat values).
#define CPU_USAGE_GROUP_WIDTH 16
#define MAX_CPUS 1024

// Per-CPU x per-state nanosecond counters.
// Index = cpu * CPU_USAGE_GROUP_WIDTH + state_index.
// state_index matches kernel enum cpu_usage_stat:
//   0=user, 1=nice, 2=system, 3=softirq, 4=irq, 5=idle, 6=iowait, 7=steal, ...
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS * CPU_USAGE_GROUP_WIDTH);
} cpu_usage SEC(".maps");

// cpuacct_account_field is called by the scheduler whenever CPU time is
// charged to a task. `index` is the cpu_usage_stat enum value; `delta` is
// the time slice in nanoseconds. We accumulate per-CPU per-state totals.
SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, struct task_struct *task, u32 index, u64 delta) {
    if (index >= 10)
        return 0;

    u32 cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS)
        return 0;

    array_add(&cpu_usage, cpu * CPU_USAGE_GROUP_WIDTH + index, delta);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
