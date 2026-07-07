mod ethtool;
mod sysctl;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
pub use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysConfig {
    pub timestamp_ms: u64,
    pub hostname: String,
    pub kernel: String,
    pub sysctl: SysctlBaselines,
    pub interfaces: HashMap<String, InterfaceBaselines>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysctlBaselines {
    /// [min, default, max] bytes
    pub tcp_rmem: [u32; 3],
    /// [min, default, max] bytes
    pub tcp_wmem: [u32; 3],
    pub rmem_max: u64,
    pub wmem_max: u64,
    pub rmem_default: u64,
    pub wmem_default: u64,
    pub netdev_max_backlog: u32,
    pub somaxconn: u32,
    pub tcp_max_syn_backlog: u32,
    pub tcp_congestion_control: String,
    pub tcp_fastopen: u32,
    pub tcp_slow_start_after_idle: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceBaselines {
    pub driver: String,
    pub driver_version: String,
    pub fw_version: String,
    pub bus_info: String,
    pub mtu: u32,
    pub ring_rx_max: u32,
    pub ring_rx: u32,
    pub ring_tx_max: u32,
    pub ring_tx: u32,
    pub rx_queues: u32,
    pub tx_queues: u32,
    pub combined_queues: u32,
    pub msix_vectors: u32,
    pub rss_table_size: u32,
    pub coalesce_rx_usecs: u32,
    pub coalesce_tx_usecs: u32,
}

/// Collect only per-interface baselines (driver, mtu, ring params, queues).
/// Cheaper than `collect()` — skips sysctl and kernel reads. Used by the
/// network sampler for label enrichment on every 60s context refresh.
pub fn collect_interfaces() -> HashMap<String, InterfaceBaselines> {
    ethtool::collect_interfaces()
}

/// Return ethtool driver stats for a single interface as (name, cumulative_value) pairs.
/// One-shot; per-tick callers should hold an [`ethtool::StatsReader`] (re-exported
/// as [`StatsReader`]) so the socket, scratch buffer, and stat-name Strings are reused.
pub fn get_driver_stats(iface: &str) -> Vec<(String, u64)> {
    ethtool::get_driver_stats(iface)
}

pub use ethtool::StatsReader;

pub fn collect() -> SysConfig {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    SysConfig {
        timestamp_ms,
        hostname: hostname(),
        kernel: kernel_version(),
        sysctl: sysctl::collect(),
        interfaces: ethtool::collect_interfaces(),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn kernel_version() -> String {
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return String::new();
    }
    name.release
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as u8 as char)
        .collect()
}
