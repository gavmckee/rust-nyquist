use std::collections::HashMap;
use std::time::{Duration, Instant};

use aya::maps::{Array, MapData};
use aya::programs::TracePoint;
use aya::maps::ring_buf::RingBuf;
use aya::{Ebpf, include_bytes_aligned};

use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use nyquist_ebpf_common::{TcpRetransmitEvent, TcpRttEvent};

// Embedded BPF ELF, compiled via build.rs. Empty slice if build failed.
static EBPF_BYTES: &[u8] = include_bytes_aligned!(
    concat!(env!("OUT_DIR"), "/nyquist-ebpf-programs")
);

// ─── Tracepoint format parsing ────────────────────────────────────────────────

/// Read a single field's byte offset from a tracepoint format file.
/// Format: `\tfield:<type> <name>; offset:<N>; size:...; signed:...;`
fn tracepoint_field_offset(category: &str, event: &str, field: &str) -> Option<u32> {
    let path = format!("/sys/kernel/debug/tracing/events/{category}/{event}/format");
    let text = std::fs::read_to_string(&path).ok()?;

    for line in text.lines() {
        // Match lines like: "	field:__u32 srtt; offset:96; size:4; signed:0;"
        if line.contains(&format!(" {field};")) && line.contains("offset:") {
            let off = line.split("offset:").nth(1)?;
            let off = off.split(';').next()?.trim();
            return off.parse().ok();
        }
    }
    None
}

/// Determine whether the running kernel has the `state` field in tcp:tcp_probe.
/// This was added in Linux 6.x and shifts `srtt`'s offset from 96 → 100.
fn tcp_probe_srtt_offset() -> u32 {
    // Try to read the exact offset from debugfs (requires root in production)
    if let Some(off) = tracepoint_field_offset("tcp", "tcp_probe", "srtt") {
        return off;
    }
    // Fall back: detect presence of `state` field
    if tracepoint_field_offset("tcp", "tcp_probe", "state").is_some() {
        100 // kernel 6.x+
    } else {
        96 // kernel ≤ 5.x
    }
}

// ─── Service port heuristic ───────────────────────────────────────────────────

/// Returns the "server side" port for labelling: the non-ephemeral port.
/// Linux ephemeral range starts at 32768 by default.
fn service_port(sport: u16, dport: u16) -> Option<u16> {
    match (sport < 32768, dport < 32768) {
        (true, false)  => Some(sport),
        (false, true)  => Some(dport),
        (true, true)   => Some(sport.min(dport)),
        (false, false) => None,
    }
}

// ─── Sampler ─────────────────────────────────────────────────────────────────

enum State {
    /// Not yet attempted.
    Uninit,
    /// Successfully loaded and attached.
    Running {
        _ebpf: Ebpf, // keep alive; dropping detaches programs
        rtt_ring: RingBuf<MapData>,
        retransmit_ring: RingBuf<MapData>,
    },
    /// Load/attach failed; give up permanently.
    Disabled,
}

pub struct EbpfSampler {
    interval: Duration,
    state: State,
    rtt_ids: HashMap<u16, MetricId>,
    retrans_ids: HashMap<u16, MetricId>,
    // inode-based retransmit tracking isn't available via BPF ring buffers;
    // instead we accumulate per-port event counts.
    retrans_accum: HashMap<u16, u64>,
}

impl EbpfSampler {
    pub fn new(interval: Duration) -> Self {
        EbpfSampler {
            interval,
            state: State::Uninit,
            rtt_ids: HashMap::new(),
            retrans_ids: HashMap::new(),
            retrans_accum: HashMap::new(),
        }
    }

    fn try_init(&mut self) -> Result<(), anyhow::Error> {
        if EBPF_BYTES.is_empty() {
            anyhow::bail!("eBPF programs not compiled (bpf-linker unavailable at build time)");
        }

        let mut ebpf = Ebpf::load(EBPF_BYTES)?;

        // Configure tracepoint field offsets for the running kernel
        let srtt_offset = tcp_probe_srtt_offset();
        tracing::debug!(srtt_offset, "tcp_probe srtt offset detected");

        let mut offsets: Array<_, u32> =
            Array::try_from(ebpf.map_mut("PROBE_OFFSETS").expect("PROBE_OFFSETS map"))?;
        offsets.set(0, srtt_offset, 0)?;      // srtt offset in tcp_probe
        offsets.set(1, 64u32, 0)?;             // sport offset in tcp_probe
        offsets.set(2, 66u32, 0)?;             // dport offset in tcp_probe
        offsets.set(3, 28u32, 0)?;             // sport offset in tcp_retransmit_skb
        offsets.set(4, 30u32, 0)?;             // dport offset in tcp_retransmit_skb
        drop(offsets);

        // Attach tcp:tcp_probe
        let prog: &mut TracePoint = ebpf.program_mut("tcp_probe")
            .expect("tcp_probe program")
            .try_into()?;
        prog.load()?;
        prog.attach("tcp", "tcp_probe")?;

        // Attach tcp:tcp_retransmit_skb
        let prog: &mut TracePoint = ebpf.program_mut("tcp_retransmit_skb")
            .expect("tcp_retransmit_skb program")
            .try_into()?;
        prog.load()?;
        prog.attach("tcp", "tcp_retransmit_skb")?;

        // Take ownership of ring buffers
        let rtt_ring = RingBuf::try_from(
            ebpf.take_map("RTT_EVENTS").expect("RTT_EVENTS map")
        )?;
        let retransmit_ring = RingBuf::try_from(
            ebpf.take_map("RETRANSMIT_EVENTS").expect("RETRANSMIT_EVENTS map")
        )?;

        self.state = State::Running {
            _ebpf: ebpf,
            rtt_ring,
            retransmit_ring,
        };
        Ok(())
    }

    fn drain_rings(&mut self, reg: &Registry, now: Instant) {
        let State::Running { rtt_ring, retransmit_ring, .. } = &mut self.state else {
            return;
        };

        // Drain RTT events
        while let Some(item) = rtt_ring.next() {
            let bytes: &[u8] = &*item;
            if bytes.len() < core::mem::size_of::<TcpRttEvent>() { continue; }
            let event: TcpRttEvent = unsafe {
                core::ptr::read_unaligned(bytes.as_ptr() as *const TcpRttEvent)
            };
            if let Some(port) = service_port(event.sport, event.dport) {
                let id = self.rtt_ids.entry(port).or_insert_with(|| {
                    reg.register(
                        MetricDef::new("ebpf/tcp/rtt_us", Kind::Gauge)
                            .unit(Unit::None)
                            .labels(Labels::new().insert("port", port.to_string())),
                    )
                });
                reg.record_gauge(*id, now, event.srtt_us as u64);
            }
        }

        // Drain retransmit events
        while let Some(item) = retransmit_ring.next() {
            let bytes: &[u8] = &*item;
            if bytes.len() < core::mem::size_of::<TcpRetransmitEvent>() { continue; }
            let event: TcpRetransmitEvent = unsafe {
                core::ptr::read_unaligned(bytes.as_ptr() as *const TcpRetransmitEvent)
            };
            if let Some(port) = service_port(event.sport, event.dport) {
                *self.retrans_accum.entry(port).or_insert(0) += 1;
            }
        }

        // Flush accumulated retransmit counters to registry
        let ports_totals: Vec<(u16, u64)> = self.retrans_accum.iter().map(|(&p, &t)| (p, t)).collect();
        for (port, total) in ports_totals {
            let id = self.retrans_ids.entry(port).or_insert_with(|| {
                reg.register(
                    MetricDef::new("ebpf/tcp/retransmits", Kind::Counter)
                        .unit(Unit::Count)
                        .labels(Labels::new().insert("port", port.to_string())),
                )
            });
            reg.record_counter(*id, now, total);
        }
    }
}

#[async_trait::async_trait]
impl Sampler for EbpfSampler {
    fn name(&self) -> &str { "ebpf" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "ebpf: failed to load programs, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!("ebpf: tcp_probe + tcp_retransmit_skb attached");
            }
            State::Running { .. } => {}
        }

        self.drain_rings(reg, now);
        Ok(())
    }
}
