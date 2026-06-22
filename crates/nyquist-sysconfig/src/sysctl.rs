use crate::SysctlBaselines;

pub fn collect() -> SysctlBaselines {
    SysctlBaselines {
        tcp_rmem:                read_triple("/proc/sys/net/ipv4/tcp_rmem"),
        tcp_wmem:                read_triple("/proc/sys/net/ipv4/tcp_wmem"),
        rmem_max:                read_u64("/proc/sys/net/core/rmem_max"),
        wmem_max:                read_u64("/proc/sys/net/core/wmem_max"),
        rmem_default:            read_u64("/proc/sys/net/core/rmem_default"),
        wmem_default:            read_u64("/proc/sys/net/core/wmem_default"),
        netdev_max_backlog:      read_u32("/proc/sys/net/core/netdev_max_backlog"),
        somaxconn:               read_u32("/proc/sys/net/core/somaxconn"),
        tcp_max_syn_backlog:     read_u32("/proc/sys/net/ipv4/tcp_max_syn_backlog"),
        tcp_congestion_control:  read_str("/proc/sys/net/ipv4/tcp_congestion_control"),
        tcp_fastopen:            read_u32("/proc/sys/net/ipv4/tcp_fastopen"),
        tcp_slow_start_after_idle: read_u32("/proc/sys/net/ipv4/tcp_slow_start_after_idle"),
    }
}

fn read_u32(path: &str) -> u32 {
    std::fs::read_to_string(path).ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_u64(path: &str) -> u64 {
    std::fs::read_to_string(path).ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_str(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default().trim().to_string()
}

fn read_triple(path: &str) -> [u32; 3] {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    let mut it = s.split_whitespace();
    [
        it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
    ]
}
