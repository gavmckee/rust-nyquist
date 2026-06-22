// SPDX-License-Identifier: GPL-2.0
// cpu/vtime BPF sampler: per-CPU idle, iowait, irq, and softirq nanoseconds.
// Complements cpu/usage (kprobe/cpuacct_account_field) which does not fire for
// these states on CONFIG_VIRT_CPU_ACCOUNTING_GEN kernels.
//
// Principle 2: cpu_state accumulator is BPF_F_MMAPABLE — zero-syscall reads.
// Principle 8: all maps keyed by cpu_id (dense, bounded by MAX_CPUS).
// tp_btf (not raw_tp) is required: BTF-typed args let the verifier accept
// direct dereferences (BPF_CORE_READ_BITFIELD for in_iowait bitfield) and
// typed pointer arithmetic. raw_tp would make prev/next opaque scalars.
#include <vmlinux.h>
#include "helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define MAX_CPUS    1024
#define GROUP_WIDTH 4

#define IDX_IDLE    0
#define IDX_IOWAIT  1
#define IDX_IRQ     2
#define IDX_SOFTIRQ 3

// Per-CPU, per-state nanosecond accumulators — mmapped by userspace.
// Layout: cpu * GROUP_WIDTH + state_idx (idle=0, iowait=1, irq=2, softirq=3).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS * GROUP_WIDTH);
} cpu_state SEC(".maps");

// Scratch: timestamp (ns) when this CPU entered the idle task (0 = not idle).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS);
} idle_start SEC(".maps");

// Scratch: 1 if the current idle period is iowait (a task blocked on I/O
// caused this CPU to go idle), 0 if plain idle.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u32);
    __uint(max_entries, MAX_CPUS);
} idle_is_iowait SEC(".maps");

// Scratch: timestamp when the current IRQ handler entered on this CPU.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS);
} irq_start SEC(".maps");

// Scratch: timestamp when the current softirq handler entered on this CPU.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS);
} softirq_start SEC(".maps");

// sched_switch: track transitions to/from the idle task (pid == 0).
// When entering idle, sample prev->in_iowait to classify the period.
// When leaving idle, accumulate elapsed ns into idle or iowait counter.
SEC("tp_btf/sched_switch")
int BPF_PROG(handle_sched_switch, bool preempt,
             struct task_struct *prev, struct task_struct *next) {
    u32 cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS) return 0;

    u32 next_pid = BPF_CORE_READ(next, pid);
    u32 prev_pid = BPF_CORE_READ(prev, pid);
    u64 now = bpf_ktime_get_ns();

    if (next_pid == 0) {
        // Entering idle: snapshot start time and iowait classification.
        // in_iowait is unsigned:1 in task_struct — BPF_CORE_READ_BITFIELD required.
        u32 iow = BPF_CORE_READ_BITFIELD(prev, in_iowait);
        u64 *sp = bpf_map_lookup_elem(&idle_start, &cpu);
        u32 *ip = bpf_map_lookup_elem(&idle_is_iowait, &cpu);
        if (sp) *sp = now;
        if (ip) *ip = iow;
    } else if (prev_pid == 0) {
        // Leaving idle: compute delta and choose idle vs iowait slot.
        u64 *sp = bpf_map_lookup_elem(&idle_start, &cpu);
        if (!sp || *sp == 0) return 0;
        u64 delta = (now > *sp) ? (now - *sp) : 0;
        *sp = 0;

        u32 *ip = bpf_map_lookup_elem(&idle_is_iowait, &cpu);
        u32 idx = (ip && *ip) ? IDX_IOWAIT : IDX_IDLE;
        array_add(&cpu_state, cpu * GROUP_WIDTH + idx, delta);
    }
    return 0;
}

// irq_handler_entry: snapshot IRQ entry time.
SEC("tp_btf/irq_handler_entry")
int BPF_PROG(handle_irq_entry, int irq, struct irqaction *action) {
    u32 cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS) return 0;
    u64 now = bpf_ktime_get_ns();
    u64 *sp = bpf_map_lookup_elem(&irq_start, &cpu);
    if (sp) *sp = now;
    return 0;
}

// irq_handler_exit: accumulate IRQ handler duration.
SEC("tp_btf/irq_handler_exit")
int BPF_PROG(handle_irq_exit, int irq, struct irqaction *action, int ret) {
    u32 cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS) return 0;
    u64 *sp = bpf_map_lookup_elem(&irq_start, &cpu);
    if (!sp || *sp == 0) return 0;
    u64 now = bpf_ktime_get_ns();
    u64 delta = (now > *sp) ? (now - *sp) : 0;
    *sp = 0;
    array_add(&cpu_state, cpu * GROUP_WIDTH + IDX_IRQ, delta);
    return 0;
}

// softirq_entry: snapshot softirq entry time.
SEC("tp_btf/softirq_entry")
int BPF_PROG(handle_softirq_entry, unsigned int vec_nr) {
    u32 cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS) return 0;
    u64 now = bpf_ktime_get_ns();
    u64 *sp = bpf_map_lookup_elem(&softirq_start, &cpu);
    if (sp) *sp = now;
    return 0;
}

// softirq_exit: accumulate softirq handler duration.
SEC("tp_btf/softirq_exit")
int BPF_PROG(handle_softirq_exit, unsigned int vec_nr) {
    u32 cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS) return 0;
    u64 *sp = bpf_map_lookup_elem(&softirq_start, &cpu);
    if (!sp || *sp == 0) return 0;
    u64 now = bpf_ktime_get_ns();
    u64 delta = (now > *sp) ? (now - *sp) : 0;
    *sp = 0;
    array_add(&cpu_state, cpu * GROUP_WIDTH + IDX_SOFTIRQ, delta);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
