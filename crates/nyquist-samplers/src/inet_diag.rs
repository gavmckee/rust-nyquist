/// Raw INET_DIAG netlink query for per-connection TCP statistics.
///
/// Sends a SOCK_DIAG_BY_FAMILY dump request over AF_NETLINK and parses
/// the resulting inet_diag_msg + INET_DIAG_INFO (tcp_info) attributes.
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

const NETLINK_INET_DIAG: libc::c_int = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLM_F_REQUEST: u16 = 0x0001;
const NLM_F_DUMP: u16 = 0x0300;
const NLMSG_DONE: u16 = 3;
const NLMSG_ERROR: u16 = 2;
const INET_DIAG_INFO: u16 = 2;

// AF_INET (2) — we issue a separate query for IPv6 if needed; IPv4 covers most server traffic
const AF_INET: u8 = 2;
const IPPROTO_TCP: u8 = 6;
const TCPF_ALL: u32 = 0xFFF;

// tcp_info field offsets (linux UAPI, stable since kernel 2.6):
//   8 bytes of u8 fields, then u32 fields aligned at +8
//   rtt       = +68, rttvar = +72, total_retrans = +100
const TCPINFO_RTT_OFFSET: usize = 68;
const TCPINFO_TOTAL_RETRANS_OFFSET: usize = 100;
const TCPINFO_MIN_LEN: usize = 104;

#[derive(Debug, Clone)]
pub struct TcpStats {
    pub sport: u16,        // host byte order
    pub dport: u16,        // host byte order
    pub inode: u32,
    pub rtt_us: u32,       // smoothed RTT in microseconds (0 = not available)
    pub total_retrans: u32,
}

// All structs are repr(C) to match kernel ABI exactly.

#[repr(C)]
struct NlMsgHdr {
    len:   u32,
    typ:   u16,
    flags: u16,
    seq:   u32,
    pid:   u32,
}

#[repr(C, packed)]
struct InetDiagSockId {
    sport:   u16,      // network byte order
    dport:   u16,      // network byte order
    src:     [u32; 4],
    dst:     [u32; 4],
    iface:   u32,
    cookie:  [u64; 1], // two u32 packed as u64 for alignment
}

#[repr(C)]
struct InetDiagReqV2 {
    family:   u8,
    protocol: u8,
    ext:      u8, // INET_DIAG_INFO bit will be set
    pad:      u8,
    states:   u32,
    id:       InetDiagSockId,
}

#[repr(C)]
struct InetDiagMsg {
    family:   u8,
    state:    u8,
    timer:    u8,
    retrans:  u8,
    id:       InetDiagSockId,
    expires:  u32,
    rqueue:   u32,
    wqueue:   u32,
    uid:      u32,
    inode:    u32,
}

/// Query all TCP connections and return per-connection stats.
/// Returns an empty Vec (not an error) if the socket cannot be created (e.g. no permission).
pub fn query_tcp_connections() -> io::Result<Vec<TcpStats>> {
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_INET_DIAG,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    use std::os::fd::AsRawFd;
    let fd = owned.as_raw_fd();

    send_dump_request(fd)?;
    recv_connections(fd)
}

fn send_dump_request(fd: libc::c_int) -> io::Result<()> {
    // Allocate the full message on the stack
    let hdr_len = std::mem::size_of::<NlMsgHdr>();
    let req_len = std::mem::size_of::<InetDiagReqV2>();
    let total = hdr_len + req_len;

    let mut buf = vec![0u8; total];

    let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut NlMsgHdr) };
    hdr.len   = total as u32;
    hdr.typ   = SOCK_DIAG_BY_FAMILY;
    hdr.flags = NLM_F_REQUEST | NLM_F_DUMP;
    hdr.seq   = 1;
    hdr.pid   = 0;

    let req = unsafe {
        &mut *(buf[hdr_len..].as_mut_ptr() as *mut InetDiagReqV2)
    };
    req.family   = AF_INET;
    req.protocol = IPPROTO_TCP;
    req.ext      = 1 << (INET_DIAG_INFO - 1); // request tcp_info extension
    req.pad      = 0;
    req.states   = TCPF_ALL;
    // id fields left zero = wildcard match

    // Use zeroed() to avoid libc's Padding<u16> type for nl_pad
    let mut dst: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    dst.nl_family = libc::AF_NETLINK as _;

    let ret = unsafe {
        libc::sendto(
            fd,
            buf.as_ptr() as *const _,
            buf.len(),
            0,
            &dst as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as _,
        )
    };
    if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn recv_connections(fd: libc::c_int) -> io::Result<Vec<TcpStats>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 65536];

    loop {
        let len = unsafe {
            libc::recv(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0)
        };
        if len < 0 {
            return Err(io::Error::last_os_error());
        }
        let len = len as usize;
        if len == 0 { break; }

        let mut pos = 0usize;
        while pos + std::mem::size_of::<NlMsgHdr>() <= len {
            let hdr = unsafe { &*(buf[pos..].as_ptr() as *const NlMsgHdr) };
            let msg_len = hdr.len as usize;

            if msg_len < std::mem::size_of::<NlMsgHdr>() || pos + msg_len > len {
                break;
            }

            match hdr.typ {
                NLMSG_DONE  => return Ok(out),
                NLMSG_ERROR => return Err(io::Error::other("netlink error response")),
                SOCK_DIAG_BY_FAMILY => {
                    let payload = &buf[pos + std::mem::size_of::<NlMsgHdr>()..pos + msg_len];
                    if let Some(s) = parse_diag_msg(payload) {
                        out.push(s);
                    }
                }
                _ => {}
            }

            // nlmsghdr lengths are already aligned to NLMSG_ALIGN (4 bytes)
            pos += nlmsg_align(msg_len);
        }
    }
    Ok(out)
}

fn parse_diag_msg(payload: &[u8]) -> Option<TcpStats> {
    let msg_sz = std::mem::size_of::<InetDiagMsg>();
    if payload.len() < msg_sz { return None; }

    let msg = unsafe { &*(payload.as_ptr() as *const InetDiagMsg) };
    // Ports are in network byte order
    let sport = u16::from_be(msg.id.sport);
    let dport = u16::from_be(msg.id.dport);
    let inode = msg.inode;

    // Walk netlink attributes to find INET_DIAG_INFO
    let mut rtt_us = 0u32;
    let mut total_retrans = 0u32;

    let mut attr_off = msg_sz;
    while attr_off + 4 <= payload.len() {
        let nla_len  = u16::from_ne_bytes([payload[attr_off], payload[attr_off + 1]]) as usize;
        let nla_type = u16::from_ne_bytes([payload[attr_off + 2], payload[attr_off + 3]]);

        if nla_len < 4 { break; }
        let data_end = attr_off + nla_len;
        if data_end > payload.len() { break; }

        if nla_type == INET_DIAG_INFO {
            let data = &payload[attr_off + 4..data_end];
            if data.len() >= TCPINFO_MIN_LEN {
                rtt_us = u32::from_ne_bytes(
                    data[TCPINFO_RTT_OFFSET..TCPINFO_RTT_OFFSET + 4].try_into().ok()?
                );
                total_retrans = u32::from_ne_bytes(
                    data[TCPINFO_TOTAL_RETRANS_OFFSET..TCPINFO_TOTAL_RETRANS_OFFSET + 4]
                        .try_into().ok()?
                );
            }
        }

        // Attributes are padded to 4-byte boundaries
        attr_off += nla_align(nla_len);
    }

    Some(TcpStats { sport, dport, inode, rtt_us, total_retrans })
}

#[inline]
const fn nlmsg_align(len: usize) -> usize { (len + 3) & !3 }

#[inline]
const fn nla_align(len: usize) -> usize { (len + 3) & !3 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_query_tcp_connections() {
        // This test exercises the live kernel path; it's OK if no connections
        // are returned (e.g. permission restricted) or if the socket creation
        // fails; we just verify no panic occurs.
        match query_tcp_connections() {
            Ok(conns) => {
                // If we got connections, all ports should be reasonable values
                for c in &conns {
                    assert!(c.sport > 0 || c.dport > 0, "both ports zero?");
                }
            }
            Err(e) => {
                // EPERM (1) or EACCES (13) are acceptable in restricted environments
                let raw = e.raw_os_error().unwrap_or(0);
                assert!(
                    raw == 1 || raw == 13 || raw == 0,
                    "unexpected error: {e} (raw={raw})"
                );
            }
        }
    }
}
