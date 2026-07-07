// SPDX-License-Identifier: GPL-2.0
// Hook: fentry/proc_sys_call_handler
// Captures writes to /proc/sys/net/* using a manual dentry walk.
// bpf_d_path is not permitted here (restricted to VFS/security fentry points),
// so we read the LAST THREE dentry name components and pack them into
// e->key as fixed 32/32/64-byte slots:
//   key[0..32)  = grandparent name ("net" for 3-deep keys, "conf" for
//                 net.ipv4.conf.<iface>.<leaf>)
//   key[32..64) = parent name
//   key[64..128)= leaf name
// "net" membership is validated by walking UP TO 6 ancestors — the old
// fixed-depth check (grandparent == "net") silently discarded every key
// nested deeper than /proc/sys/net/<dir>/<leaf>, e.g. per-interface
// net.ipv4.conf.*.* writes. Userspace reassembles the path from the slots.
#include <vmlinux.h>
#include "syswatch_common.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, SW_RINGBUF_SIZE);
} events SEC(".maps");

SEC("fentry/proc_sys_call_handler")
int BPF_PROG(sw_sysctl, struct kiocb *iocb, struct iov_iter *iter, int write)
{
    if (!write) return 0;

    struct sw_event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) return 0;
    __builtin_memset(e, 0, sizeof(*e));

    e->ts_ns = bpf_ktime_get_ns();
    e->pid   = (u32)(bpf_get_current_pid_tgid() >> 32);
    e->src   = SW_SRC_SYSCTL;
    bpf_get_current_comm(e->comm, SW_COMM_LEN);

    /* Walk three dentry levels: leaf → parent → grandparent. */
    struct file   *filp = BPF_CORE_READ(iocb, ki_filp);
    struct dentry *d    = BPF_CORE_READ(filp, f_path.dentry);
    struct dentry *p1   = BPF_CORE_READ(d,  d_parent);
    struct dentry *p2   = BPF_CORE_READ(p1, d_parent);

    /* Pack names into fixed slots of e->key. */
    bpf_probe_read_kernel_str(e->key + 0,  32, BPF_CORE_READ(p2, d_name.name));
    bpf_probe_read_kernel_str(e->key + 32, 32, BPF_CORE_READ(p1, d_name.name));
    bpf_probe_read_kernel_str(e->key + 64, 64, BPF_CORE_READ(d,  d_name.name));

    /* Filter: emit only for /proc/sys/net/… — some ancestor must be "net".
     * Start at the grandparent (the 3-deep common case matches immediately)
     * and walk up a bounded number of levels; procfs' root dentry has
     * d_parent == itself, which terminates the walk. */
    bool is_net = false;
    struct dentry *cur = p2;
    for (int i = 0; i < 6; i++) {
        char nbuf[8] = {};
        if (bpf_probe_read_kernel_str(nbuf, sizeof(nbuf),
                                      BPF_CORE_READ(cur, d_name.name)) < 0)
            break;
        if (nbuf[0] == 'n' && nbuf[1] == 'e' && nbuf[2] == 't' && nbuf[3] == '\0') {
            is_net = true;
            break;
        }
        struct dentry *parent = BPF_CORE_READ(cur, d_parent);
        if (parent == cur)
            break;
        cur = parent;
    }
    if (!is_net) {
        bpf_ringbuf_discard(e, 0);
        return 0;
    }

    bpf_ringbuf_submit(e, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
