use std::io;
use std::collections::HashMap;
use crate::InterfaceBaselines;

const SIOCETHTOOL: libc::c_ulong = 0x8946;
const IFNAMSIZ: usize = 16;

const ETHTOOL_GDRVINFO:   u32 = 0x00000003;
const ETHTOOL_GRINGPARAM: u32 = 0x00000010;
const ETHTOOL_GCHANNELS:  u32 = 0x0000003c;
const ETHTOOL_GCOALESCE:  u32 = 0x0000000e;
const ETHTOOL_GRXFHINDIR: u32 = 0x00000038;
const ETHTOOL_GSTRINGS:   u32 = 0x0000001b;
const ETHTOOL_GSTATS:     u32 = 0x0000001d;
const ETH_SS_STATS:       u32 = 1;
const ETH_GSTRING_LEN:    usize = 32;

// Minimal ifreq for SIOCETHTOOL.
// Linux x86_64 ifreq = ifr_name[16] + ifr_ifru union[24] = 40 bytes.
// ifr_data is the first field of the union (pointer, 8 bytes); pad to 24.
#[repr(C)]
struct EthtoolReq {
    ifr_name: [u8; IFNAMSIZ],
    ifr_data: *mut libc::c_void,
    _pad:     [u8; 16],
}

#[repr(C)]
struct DrvInfoRaw {
    cmd:          u32,
    driver:       [libc::c_char; 32],
    version:      [libc::c_char; 32],
    fw_version:   [libc::c_char; 32],
    bus_info:     [libc::c_char; 32],
    erom_version: [libc::c_char; 32],
    reserved2:    [libc::c_char; 12],
    n_priv_flags: u32,
    n_stats:      u32,
    testinfo_len: u32,
    eedump_len:   u32,
    regdump_len:  u32,
}

#[repr(C)]
struct RingParamRaw {
    cmd:                  u32,
    rx_max_pending:       u32,
    rx_mini_max_pending:  u32,
    rx_jumbo_max_pending: u32,
    tx_max_pending:       u32,
    rx_pending:           u32,
    rx_mini_pending:      u32,
    rx_jumbo_pending:     u32,
    tx_pending:           u32,
}

#[repr(C)]
struct ChannelsRaw {
    cmd:            u32,
    max_rx:         u32,
    max_tx:         u32,
    max_other:      u32,
    max_combined:   u32,
    rx_count:       u32,
    tx_count:       u32,
    other_count:    u32,
    combined_count: u32,
}

#[repr(C)]
struct CoalesceRaw {
    cmd:                          u32,
    rx_coalesce_usecs:            u32,
    rx_max_coalesced_frames:      u32,
    rx_coalesce_usecs_irq:        u32,
    rx_max_coalesced_frames_irq:  u32,
    tx_coalesce_usecs:            u32,
    tx_max_coalesced_frames:      u32,
    tx_coalesce_usecs_irq:        u32,
    tx_max_coalesced_frames_irq:  u32,
    stats_block_coalesce_usecs:   u32,
    use_adaptive_rx_coalesce:     u32,
    use_adaptive_tx_coalesce:     u32,
    pkt_rate_low:                 u32,
    rx_coalesce_usecs_low:        u32,
    rx_max_coalesced_frames_low:  u32,
    tx_coalesce_usecs_low:        u32,
    tx_max_coalesced_frames_low:  u32,
    pkt_rate_high:                u32,
    rx_coalesce_usecs_high:       u32,
    rx_max_coalesced_frames_high: u32,
    tx_coalesce_usecs_high:       u32,
    tx_max_coalesced_frames_high: u32,
    rate_sample_interval:         u32,
}

#[repr(C)]
struct RxFhIndirSizeRaw {
    cmd:  u32,
    size: u32,
}

struct EthtoolSocket(i32);

impl EthtoolSocket {
    fn open() -> io::Result<Self> {
        // SOCK_CLOEXEC and the ethtool ioctls are Linux-only; on other platforms
        // compile a plain SOCK_DGRAM so the workspace builds (calls fail at runtime).
        #[cfg(target_os = "linux")]
        let sock_type = libc::SOCK_DGRAM | libc::SOCK_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let sock_type = libc::SOCK_DGRAM;
        let fd = unsafe {
            libc::socket(libc::AF_INET, sock_type, 0)
        };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        Ok(Self(fd))
    }

    fn ioctl<T>(&self, iface: &str, data: &mut T) -> io::Result<()> {
        self.ioctl_raw(iface, data as *mut T as *mut libc::c_void)
    }

    // For variable-length ethtool commands backed by a heap buffer.
    // Caller must ensure buf contains a valid ethtool command header at offset 0.
    fn ioctl_buf(&self, iface: &str, buf: &mut [u8]) -> io::Result<()> {
        self.ioctl_raw(iface, buf.as_mut_ptr() as *mut libc::c_void)
    }

    fn ioctl_raw(&self, iface: &str, ptr: *mut libc::c_void) -> io::Result<()> {
        let mut req = EthtoolReq {
            ifr_name: [0u8; IFNAMSIZ],
            ifr_data: ptr,
            _pad:     [0u8; 16],
        };
        let b = iface.as_bytes();
        req.ifr_name[..b.len().min(IFNAMSIZ - 1)].copy_from_slice(&b[..b.len().min(IFNAMSIZ - 1)]);
        let rc = unsafe { libc::ioctl(self.0, SIOCETHTOOL, &req as *const EthtoolReq) };
        if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }
}

impl Drop for EthtoolSocket {
    fn drop(&mut self) { unsafe { libc::close(self.0); } }
}

fn cstr(buf: &[libc::c_char]) -> String {
    buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8 as char).collect()
}

/// Return all ethtool driver stats for an interface as (name, cumulative_value) pairs.
/// Returns empty if the driver doesn't support ETHTOOL_GSTATS or the call fails.
pub fn get_driver_stats(iface: &str) -> Vec<(String, u64)> {
    let eth = match EthtoolSocket::open() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut drv: DrvInfoRaw = unsafe { std::mem::zeroed() };
    drv.cmd = ETHTOOL_GDRVINFO;
    if eth.ioctl(iface, &mut drv).is_err() { return Vec::new(); }

    let n = drv.n_stats as usize;
    // 0 stats or implausibly large → driver doesn't support GSTATS
    if n == 0 || n > 8192 { return Vec::new(); }

    // GSTRINGS layout: [cmd:u32, string_set:u32, len:u32] + n * 32 bytes
    let str_sz = 12 + n * ETH_GSTRING_LEN;
    let mut str_buf = vec![0u8; str_sz];
    str_buf[0..4].copy_from_slice(&ETHTOOL_GSTRINGS.to_ne_bytes());
    str_buf[4..8].copy_from_slice(&ETH_SS_STATS.to_ne_bytes());
    str_buf[8..12].copy_from_slice(&(n as u32).to_ne_bytes());
    if eth.ioctl_buf(iface, &mut str_buf).is_err() { return Vec::new(); }

    // GSTATS layout: [cmd:u32, n_stats:u32] + n * 8 bytes
    let val_sz = 8 + n * 8;
    let mut val_buf = vec![0u8; val_sz];
    val_buf[0..4].copy_from_slice(&ETHTOOL_GSTATS.to_ne_bytes());
    val_buf[4..8].copy_from_slice(&(n as u32).to_ne_bytes());
    if eth.ioctl_buf(iface, &mut val_buf).is_err() { return Vec::new(); }

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = 12 + i * ETH_GSTRING_LEN;
        let raw = &str_buf[off..off + ETH_GSTRING_LEN];
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(ETH_GSTRING_LEN);
        if nul == 0 { continue; }
        let name = String::from_utf8_lossy(&raw[..nul]).into_owned();

        let voff = 8 + i * 8;
        let val = u64::from_ne_bytes(val_buf[voff..voff + 8].try_into().unwrap_or([0u8; 8]));
        out.push((name, val));
    }
    out
}

pub fn collect_interfaces() -> HashMap<String, InterfaceBaselines> {
    let eth = match EthtoolSocket::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "sysconfig: ethtool socket unavailable");
            return HashMap::new();
        }
    };
    list_ifaces().into_iter().filter_map(|name| {
        let cfg = collect_one(&eth, &name)?;
        Some((name, cfg))
    }).collect()
}

fn list_ifaces() -> Vec<String> {
    std::fs::read_dir("/sys/class/net")
        .map(|d| d.filter_map(|e| {
            let n = e.ok()?.file_name().into_string().ok()?;
            if n == "lo" { None } else { Some(n) }
        }).collect())
        .unwrap_or_default()
}

fn collect_one(eth: &EthtoolSocket, iface: &str) -> Option<InterfaceBaselines> {
    // drvinfo is required — if it fails, skip the interface entirely
    let mut drv: DrvInfoRaw = unsafe { std::mem::zeroed() };
    drv.cmd = ETHTOOL_GDRVINFO;
    eth.ioctl(iface, &mut drv).ok()?;

    let mut ring: RingParamRaw = unsafe { std::mem::zeroed() };
    ring.cmd = ETHTOOL_GRINGPARAM;
    let _ = eth.ioctl(iface, &mut ring);

    let mut ch: ChannelsRaw = unsafe { std::mem::zeroed() };
    ch.cmd = ETHTOOL_GCHANNELS;
    let _ = eth.ioctl(iface, &mut ch);

    let mut coal: CoalesceRaw = unsafe { std::mem::zeroed() };
    coal.cmd = ETHTOOL_GCOALESCE;
    let _ = eth.ioctl(iface, &mut coal);

    let mut rss: RxFhIndirSizeRaw = unsafe { std::mem::zeroed() };
    rss.cmd = ETHTOOL_GRXFHINDIR;
    // size=0 → kernel fills in actual table size and returns 0 or EINVAL
    match eth.ioctl(iface, &mut rss) {
        Ok(()) | Err(_) => {} // rss.size filled in on both paths
    }

    let mtu = std::fs::read_to_string(format!("/sys/class/net/{iface}/mtu"))
        .ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);

    let msix_vectors = std::fs::read_dir(
            format!("/sys/class/net/{iface}/device/msi_irqs")
        ).map(|d| d.count() as u32).unwrap_or(0);

    Some(InterfaceBaselines {
        driver:           cstr(&drv.driver),
        driver_version:   cstr(&drv.version),
        fw_version:       cstr(&drv.fw_version),
        bus_info:         cstr(&drv.bus_info),
        mtu,
        ring_rx_max:      ring.rx_max_pending,
        ring_rx:          ring.rx_pending,
        ring_tx_max:      ring.tx_max_pending,
        ring_tx:          ring.tx_pending,
        rx_queues:        ch.rx_count,
        tx_queues:        ch.tx_count,
        combined_queues:  ch.combined_count,
        msix_vectors,
        rss_table_size:   rss.size,
        coalesce_rx_usecs: coal.rx_coalesce_usecs,
        coalesce_tx_usecs: coal.tx_coalesce_usecs,
    })
}
