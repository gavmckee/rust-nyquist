// SPDX-License-Identifier: GPL-2.0
// Hook: fentry/rtnetlink_rcv_msg
// Captures RTM_NEWLINK/SETLINK (MTU, flags), RTM_NEWROUTE/DELROUTE,
// RTM_NEWRULE/DELRULE events so userspace can re-read affected config.
// nlmsg_type is emitted so the decoder knows which re-read path to take.
#include <vmlinux.h>
#include "syswatch_common.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, SW_RINGBUF_SIZE);
} events SEC(".maps");

/* rtnetlink message type constants (from <linux/rtnetlink.h>) */
#define RTM_NEWLINK   16
#define RTM_SETLINK   19
#define RTM_NEWROUTE  24
#define RTM_DELROUTE  25
#define RTM_NEWRULE   32
#define RTM_DELRULE   33

SEC("fentry/rtnetlink_rcv_msg")
int BPF_PROG(sw_rtnetlink, struct sk_buff *skb, struct nlmsghdr *nlh,
             struct netlink_ext_ack *extack)
{
    u16 nlmsg_type = BPF_CORE_READ(nlh, nlmsg_type);

    if (nlmsg_type != RTM_NEWLINK  &&
        nlmsg_type != RTM_SETLINK  &&
        nlmsg_type != RTM_NEWROUTE &&
        nlmsg_type != RTM_DELROUTE &&
        nlmsg_type != RTM_NEWRULE  &&
        nlmsg_type != RTM_DELRULE)
        return 0;

    struct sw_event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) return 0;
    __builtin_memset(e, 0, sizeof(*e));

    e->ts_ns      = bpf_ktime_get_ns();
    e->pid        = (u32)(bpf_get_current_pid_tgid() >> 32);
    e->src        = SW_SRC_RTNETLINK;
    e->nlmsg_type = nlmsg_type;
    bpf_get_current_comm(e->comm, SW_COMM_LEN);

    bpf_ringbuf_submit(e, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
