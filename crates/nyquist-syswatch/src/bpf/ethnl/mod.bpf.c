// SPDX-License-Identifier: GPL-2.0
// Hooks: fentry/ethnl_ops_begin + fexit/ethnl_default_set_doit
//
// Modern ethtool userspace (>=5.x) speaks the ethtool NETLINK API and never
// calls the legacy dev_ethtool ioctl covered by the sibling hook — so ring/
// channel/coalesce changes made by current tooling were only caught by the
// 60s poll, with attribution lost to comm="poll".
//
// The resolved net_device is only in scope inside the request handling
// (ethnl_ops_begin fires with it, for gets and sets alike); whether a SET
// happened and SUCCEEDED is only known at ethnl_default_set_doit fexit.
// A pid-keyed LRU map bridges the two: ops_begin caches the ifname for the
// current task, set_doit fexit (retval == 0) emits the event with it.
// Entries written by get-path tasks are simply overwritten/evicted (LRU).
#include <vmlinux.h>
#include "syswatch_common.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "GPL";

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, SW_RINGBUF_SIZE);
} events SEC(".maps");

struct ifname_val {
    char name[SW_IFNAME_LEN];
};

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 256);
    __type(key, u64);               /* pid_tgid */
    __type(value, struct ifname_val);
} last_dev SEC(".maps");

SEC("fentry/ethnl_ops_begin")
int BPF_PROG(sw_ethnl_ops_begin, struct net_device *dev)
{
    if (!dev) return 0;
    u64 id = bpf_get_current_pid_tgid();
    struct ifname_val v = {};
    BPF_CORE_READ_STR_INTO(&v.name, dev, name);
    bpf_map_update_elem(&last_dev, &id, &v, BPF_ANY);
    return 0;
}

SEC("fexit/ethnl_default_set_doit")
int BPF_PROG(sw_ethnl_set, struct sk_buff *skb, struct genl_info *info, int ret)
{
    u64 id = bpf_get_current_pid_tgid();
    struct ifname_val *v = bpf_map_lookup_elem(&last_dev, &id);

    /* A failed SET changed nothing: no refresh, no attribution. */
    if (ret != 0) {
        bpf_map_delete_elem(&last_dev, &id);
        return 0;
    }

    struct sw_event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) {
        bpf_map_delete_elem(&last_dev, &id);
        return 0;
    }
    __builtin_memset(e, 0, sizeof(*e));

    e->ts_ns  = bpf_ktime_get_ns();
    e->pid    = (u32)(bpf_get_current_pid_tgid() >> 32);
    e->src    = SW_SRC_ETHTOOL;
    e->ethcmd = 0;   /* 0 = netlink path (no ioctl cmd id) */
    bpf_get_current_comm(e->comm, SW_COMM_LEN);
    if (v)
        __builtin_memcpy(e->ifname, v->name, SW_IFNAME_LEN);

    bpf_map_delete_elem(&last_dev, &id);
    bpf_ringbuf_submit(e, 0);
    return 0;
}
