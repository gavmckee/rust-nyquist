#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::{Array, PerCpuArray, RingBuf},
    programs::TracePointContext,
};
use nyquist_ebpf_common::{TcpRetransmitEvent, TcpRttEvent};

// Sample 1-in-128 tcp_probe events per CPU. tcp_probe fires on every TCP
// segment; at 100GbE that is ~4M/s which adds ~400% CPU overhead if every
// event writes to the ring buffer. SRTT is already exponentially smoothed by
// the kernel so 1-in-128 gives statistically identical RTT readings at
// 128× less tracepoint overhead.
const SAMPLE_RATE: u64 = 128;

// ─── Configuration map ────────────────────────────────────────────────────────
// Userspace writes kernel-version-specific tracepoint field offsets here so the
// BPF programs can adapt without recompilation.
//
// Index 0: byte offset of `srtt` in tcp:tcp_probe tracepoint args
//   96  = kernel ≤ 5.x (no `state` field)
//   100 = kernel 6.x+ (`state` field added before `mark`)
// Index 1: byte offset of `sport` in tcp:tcp_probe (always 64)
// Index 2: byte offset of `dport` in tcp:tcp_probe (always 66)
// Index 3: byte offset of `sport` in tcp:tcp_retransmit_skb (always 28)
// Index 4: byte offset of `dport` in tcp:tcp_retransmit_skb (always 30)
#[map]
static PROBE_OFFSETS: Array<u32> = Array::with_max_entries(8, 0);

// ─── Ring buffers ─────────────────────────────────────────────────────────────
#[map]
static RTT_EVENTS: RingBuf = RingBuf::with_byte_size(4 * 1024 * 1024, 0);

// Per-CPU invocation counter for tcp_probe sampling. One u64 slot per CPU;
// no atomics needed since each CPU only touches its own slot.
#[map]
static PROBE_SAMPLE_CTR: PerCpuArray<u64> = PerCpuArray::with_max_entries(1, 0);

#[map]
static RETRANSMIT_EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

// ─── tcp:tcp_probe ────────────────────────────────────────────────────────────
// Fires when TCP data is sent (effectively: per ACK processed in established
// state). The `srtt` field is the smoothed RTT in microseconds as of the ack.
#[tracepoint]
pub fn tcp_probe(ctx: TracePointContext) -> i64 {
    match unsafe { try_tcp_probe(&ctx) } {
        Ok(_) => 0,
        Err(_) => 0, // never kill the probe on transient errors
    }
}

unsafe fn try_tcp_probe(ctx: &TracePointContext) -> Result<(), i64> {
    // Sample 1-in-SAMPLE_RATE invocations per CPU to keep overhead negligible
    // at high packet rates. SRTT is already smoothed so sparse sampling is fine.
    let ctr_ptr = PROBE_SAMPLE_CTR.get_ptr_mut(0).ok_or(1i64)?;
    let ctr = *ctr_ptr;
    *ctr_ptr = ctr.wrapping_add(1);
    if ctr % SAMPLE_RATE != 0 {
        return Ok(());
    }

    // Use configured offsets; fall back to kernel-6.x defaults if map not set.
    let srtt_off = PROBE_OFFSETS.get(0).copied().unwrap_or(100) as usize;
    let sport_off = PROBE_OFFSETS.get(1).copied().unwrap_or(64) as usize;
    let dport_off = PROBE_OFFSETS.get(2).copied().unwrap_or(66) as usize;

    let srtt_us: u32 = ctx.read_at(srtt_off).map_err(|_| 1i64)?;
    let sport: u16 = ctx.read_at(sport_off).map_err(|_| 1i64)?;
    let dport: u16 = ctx.read_at(dport_off).map_err(|_| 1i64)?;

    // Skip zero-RTT readings (connection not yet established or no data)
    if srtt_us == 0 {
        return Ok(());
    }

    RTT_EVENTS
        .output(
            &TcpRttEvent {
                sport: u16::from_be(sport),
                dport: u16::from_be(dport),
                srtt_us,
            },
            0,
        )
        .map_err(|_| 1i64)
}

// ─── tcp:tcp_retransmit_skb ───────────────────────────────────────────────────
// Fires on every TCP retransmit (both timeout-based and fast retransmit).
#[tracepoint]
pub fn tcp_retransmit_skb(ctx: TracePointContext) -> i64 {
    match unsafe { try_tcp_retransmit(&ctx) } {
        Ok(_) => 0,
        Err(_) => 0,
    }
}

unsafe fn try_tcp_retransmit(ctx: &TracePointContext) -> Result<(), i64> {
    let sport_off = PROBE_OFFSETS.get(3).copied().unwrap_or(28) as usize;
    let dport_off = PROBE_OFFSETS.get(4).copied().unwrap_or(30) as usize;

    let sport: u16 = ctx.read_at(sport_off).map_err(|_| 1i64)?;
    let dport: u16 = ctx.read_at(dport_off).map_err(|_| 1i64)?;

    RETRANSMIT_EVENTS
        .output(
            &TcpRetransmitEvent {
                sport: u16::from_be(sport),
                dport: u16::from_be(dport),
            },
            0,
        )
        .map_err(|_| 1i64)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
