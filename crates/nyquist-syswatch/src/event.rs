/// Mirror of `struct sw_event` from bpf/syswatch_common.h.
/// Must match the C layout exactly (184 bytes, no implicit padding).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwEvent {
    pub ts_ns:      u64,        // offset   0
    pub pid:        u32,        // offset   8
    pub src:        u32,        // offset  12
    pub ethcmd:     u32,        // offset  16
    pub nlmsg_type: u16,        // offset  20
    pub _pad:       [u8; 2],    // offset  22
    pub comm:       [u8; 16],   // offset  24
    pub ifname:     [u8; 16],   // offset  40
    pub key:        [u8; 128],  // offset  56
}                               // total: 184 bytes

pub const SW_SRC_SYSCTL:    u32 = 0;
pub const SW_SRC_ETHTOOL:   u32 = 1;
pub const SW_SRC_RTNETLINK: u32 = 2;

impl SwEvent {
    pub fn from_bytes(data: &[u8]) -> Option<&Self> {
        if data.len() < std::mem::size_of::<SwEvent>() { return None; }
        Some(unsafe { &*(data.as_ptr() as *const SwEvent) })
    }

    pub fn comm_str(&self) -> &str {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(self.comm.len());
        std::str::from_utf8(&self.comm[..end]).unwrap_or("?")
    }

    pub fn ifname_str(&self) -> &str {
        let end = self.ifname.iter().position(|&b| b == 0).unwrap_or(self.ifname.len());
        std::str::from_utf8(&self.ifname[..end]).unwrap_or("")
    }

    /// Reconstructs the sysctl path from the three packed dentry name slots
    /// (the LAST three components of the written file's path):
    ///   key[0..32)  = grandparent ("net" for 3-deep keys; an intermediate
    ///                 dir like "conf" for deeper ones)
    ///   key[32..64) = parent
    ///   key[64..128)= leaf
    /// 3-deep keys reconstruct exactly; deeper keys get an elision marker
    /// since only the last three components were captured.
    pub fn key_str(&self) -> String {
        let seg = |start: usize, len: usize| -> &str {
            let s = &self.key[start..start + len];
            let end = s.iter().position(|&b| b == 0).unwrap_or(len);
            std::str::from_utf8(&s[..end]).unwrap_or("?")
        };
        let (gp, p, leaf) = (seg(0, 32), seg(32, 32), seg(64, 64));
        if gp == "net" {
            format!("/proc/sys/net/{p}/{leaf}")
        } else {
            format!("/proc/sys/net/…/{gp}/{p}/{leaf}")
        }
    }
}

const _: () = assert!(std::mem::size_of::<SwEvent>() == 184);
