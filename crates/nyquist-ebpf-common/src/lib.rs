#![no_std]

/// Sent by the tcp_probe tracepoint when TCP data is acknowledged.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct TcpRttEvent {
    pub sport: u16,
    pub dport: u16,
    pub srtt_us: u32,
}

/// Sent by the tcp_retransmit_skb tracepoint on every TCP retransmit.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct TcpRetransmitEvent {
    pub sport: u16,
    pub dport: u16,
}
