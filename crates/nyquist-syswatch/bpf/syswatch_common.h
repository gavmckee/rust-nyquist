/* SPDX-License-Identifier: GPL-2.0 */
/* Shared types and constants for all nyquist-syswatch BPF programs.
 * Must be kept in sync with SwEvent in src/event.rs. */
#pragma once

#define SW_COMM_LEN   16
#define SW_IFNAME_LEN 16
#define SW_KEY_LEN    128

/* src field values */
#define SW_SRC_SYSCTL    0u
#define SW_SRC_ETHTOOL   1u
#define SW_SRC_RTNETLINK 2u
#define SW_SRC_FSWATCH   3u  /* synthesized in userspace (inotify), never by BPF */

/* ethtool SET command IDs (from <linux/ethtool.h>) */
#define ETHTOOL_SSET          0x00000002u
#define ETHTOOL_SCOALESCE     0x0000000fu
#define ETHTOOL_SRINGPARAM    0x00000011u
#define ETHTOOL_SPAUSEPARAM   0x00000013u
#define ETHTOOL_SFEATURES     0x0000003bu
#define ETHTOOL_SCHANNELS     0x0000003du
#define ETHTOOL_SLINKSETTINGS 0x0000004cu

/*
 * Single event type emitted by all three BPF programs.
 * Layout (184 bytes, no implicit padding):
 *   offset  0: ts_ns       u64
 *   offset  8: pid         u32
 *   offset 12: src         u32
 *   offset 16: ethcmd      u32
 *   offset 20: nlmsg_type  u16
 *   offset 22: _pad        u8[2]
 *   offset 24: comm        u8[16]
 *   offset 40: ifname      u8[16]
 *   offset 56: key         u8[128]
 *   total:  184 bytes
 */
struct sw_event {
    __u64 ts_ns;
    __u32 pid;
    __u32 src;
    __u32 ethcmd;       /* ethtool only; 0 for others */
    __u16 nlmsg_type;   /* rtnetlink only; 0 for others */
    __u8  _pad[2];
    __u8  comm[SW_COMM_LEN];
    __u8  ifname[SW_IFNAME_LEN];
    __u8  key[SW_KEY_LEN];  /* sysctl: /proc/sys/net/<path> */
};

#define SW_RINGBUF_SIZE (256 * 1024)
