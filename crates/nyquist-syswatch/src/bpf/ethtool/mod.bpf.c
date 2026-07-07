// SPDX-License-Identifier: GPL-2.0
// Hook: fentry/dev_ethtool
// Captures ethtool SET operations (ring size, queues, coalescing, etc.)
// and emits interface name + command ID so userspace can re-read the
// affected parameters via nyquist_sysconfig::collect_interfaces().
#include <vmlinux.h>
#include "syswatch_common.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, SW_RINGBUF_SIZE);
} events SEC(".maps");

static __always_inline int is_set_cmd(u32 cmd)
{
    return (cmd == ETHTOOL_SSET          ||
            cmd == ETHTOOL_SCOALESCE     ||
            cmd == ETHTOOL_SRINGPARAM    ||
            cmd == ETHTOOL_SPAUSEPARAM   ||
            cmd == ETHTOOL_SFEATURES     ||
            cmd == ETHTOOL_SCHANNELS     ||
            cmd == ETHTOOL_SLINKSETTINGS);
}

/* dev_ethtool(struct net *net, struct ifreq *ifr, void __user *useraddr)
 * useraddr is a user-space pointer to an ethtool struct whose first 4 bytes
 * are the cmd field (the ETHTOOL_* constant). */
SEC("fentry/dev_ethtool")
int BPF_PROG(sw_ethtool, struct net *net, struct ifreq *ifr, void *useraddr)
{
    /* Read cmd first — cheap filter before touching the ring buffer. */
    u32 ethcmd = 0;
    if (bpf_probe_read_user(&ethcmd, sizeof(ethcmd), useraddr) != 0)
        return 0;
    if (!is_set_cmd(ethcmd)) return 0;

    struct sw_event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) return 0;
    __builtin_memset(e, 0, sizeof(*e));

    e->ts_ns  = bpf_ktime_get_ns();
    e->pid    = (u32)(bpf_get_current_pid_tgid() >> 32);
    e->src    = SW_SRC_ETHTOOL;
    e->ethcmd = ethcmd;
    bpf_get_current_comm(e->comm, SW_COMM_LEN);

    /* ifr_name is the first member of the ifr_ifrn union (char[IFNAMSIZ]). */
    bpf_probe_read_kernel(e->ifname, SW_IFNAME_LEN,
                          ifr->ifr_ifrn.ifrn_name);

    bpf_ringbuf_submit(e, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
