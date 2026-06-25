const NAME: &str = "network/traffic";

// rx_bytes and tx_bytes are intentionally NOT counted here.
// The procfs NetworkSampler already reads those from /proc/net/dev with accurate
// per-packet accounting (pre-GRO). The raw_tp/netif_receive_skb tracepoint fires
// for GRO-coalesced super-packets, producing a bursty distribution that exceeds
// the physical link capacity at high percentiles (p999 > 2× wire speed observed
// on mlx5/200G). Procfs gives the correct values with richer labels {driver,mtu}.
//
// rx_dropped is approximate: only RX-path kfree_skb reasons are counted.
// tx_dropped is exact: net_dev_xmit rc != 0.
// rx_errors and tx_errors are driver-level hardware counters with no generic
// BPF hook; they remain in the procfs sampler as a documented gap.
//
// Map layout: counters[ifindex * GROUP_WIDTH + slot] — mmap-direct u64 array.
// Principle 2 (zero-syscall reads) and Principle 8 (arrays over hashmaps) met.
mod skel {
    include!(concat!(env!("OUT_DIR"), "/network_traffic.bpf.rs"));
}

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use async_trait::async_trait;
use memmap2::MmapOptions;
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

const COUNTER_GROUP_WIDTH: usize = 8;
const MAX_IFINDEX: usize = 512;

// Slot assignments within each ifindex group (must match mod.bpf.c).
// Slots 0 (rx_bytes) and 1 (tx_bytes) are unused — see comment at top of file.
const RX_DROPPED: usize = 2;
const TX_DROPPED: usize = 3;

enum State {
    Uninit,
    Disabled,
    Running {
        _skel: Box<skel::ModSkel<'static>>,
        ptr:   *const u64,
    },
}

unsafe impl Send for State {}

struct IfaceIds {
    rx_dropped: MetricId,
    tx_dropped: MetricId,
}

pub struct NetworkTraffic {
    interval: Duration,
    state:    State,
    ids:      HashMap<u32, IfaceIds>,
}

impl NetworkTraffic {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        NetworkTraffic { interval, state: State::Uninit, ids: HashMap::new() }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use std::mem::MaybeUninit;
        use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
        let mut object = MaybeUninit::uninit();
        let open_skel = skel::ModSkelBuilder::default().open(&mut object)?;
        let mut loaded = open_skel.load()?;
        loaded.attach()?;

        let raw_fd = loaded.maps.counters.as_fd().as_raw_fd();
        let dup_fd = unsafe { libc::dup(raw_fd) };
        anyhow::ensure!(dup_fd >= 0, "dup failed: {}", std::io::Error::last_os_error());
        let file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        let bytes = MAX_IFINDEX * COUNTER_GROUP_WIDTH * std::mem::size_of::<u64>();
        let mmap = unsafe { MmapOptions::new().len(bytes).map(&file)? };
        let ptr = mmap.as_ptr() as *const u64;
        std::mem::forget(mmap);

        let skel: Box<skel::ModSkel<'static>> = unsafe { std::mem::transmute(Box::new(loaded)) };
        self.state = State::Running { _skel: skel, ptr };
        Ok(())
    }

    fn ensure_ids(ids: &mut HashMap<u32, IfaceIds>, reg: &Registry, ifindex: u32, name: &str) {
        if ids.contains_key(&ifindex) { return; }
        let lbl = || Labels::new().insert("iface", name);
        ids.insert(ifindex, IfaceIds {
            rx_dropped: reg.register(MetricDef::new("network/receive/dropped",  Kind::Counter).unit(Unit::Count).labels(lbl())),
            tx_dropped: reg.register(MetricDef::new("network/transmit/dropped", Kind::Counter).unit(Unit::Count).labels(lbl())),
        });
    }

    // Enumerate live interfaces via if_nameindex(3).
    // Returns only ifindexes that fit within MAX_IFINDEX.
    fn live_interfaces() -> Vec<(u32, String)> {
        let mut result = Vec::new();
        let head = unsafe { libc::if_nameindex() };
        if head.is_null() { return result; }
        let mut p = head;
        loop {
            let entry = unsafe { &*p };
            if entry.if_index == 0 { break; }
            let ifindex = entry.if_index;
            if (ifindex as usize) < MAX_IFINDEX && !entry.if_name.is_null() {
                let name = unsafe { std::ffi::CStr::from_ptr(entry.if_name) }
                    .to_string_lossy()
                    .into_owned();
                if !name.is_empty() {
                    result.push((ifindex, name));
                }
            }
            p = unsafe { p.add(1) };
        }
        unsafe { libc::if_freenameindex(head) };
        result
    }
}

#[async_trait]
impl Sampler for NetworkTraffic {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "network/traffic: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!("network/traffic attached (kfree_skb+net_dev_xmit, mmap array)");
                // Skip reading counters on the init tick: `now` was captured by the
                // scheduler before try_init() ran, so using it as prev would inflate
                // dt on the next tick by ~50ms and produce a spuriously low first rate.
                return Ok(());
            }
            State::Running { .. } => {}
        }

        let State::Running { ptr, .. } = &self.state else { return Ok(()) };
        let slice = unsafe {
            std::slice::from_raw_parts(*ptr, MAX_IFINDEX * COUNTER_GROUP_WIDTH)
        };

        for (ifindex, name) in Self::live_interfaces() {
            let base = ifindex as usize * COUNTER_GROUP_WIDTH;
            Self::ensure_ids(&mut self.ids, reg, ifindex, &name);
            if let Some(ids) = self.ids.get(&ifindex) {
                reg.record_counter(ids.rx_dropped, now, slice[base + RX_DROPPED]);
                reg.record_counter(ids.tx_dropped, now, slice[base + TX_DROPPED]);
            }
        }
        Ok(())
    }
}

use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry {
    name: NAME,
    init: |reg, iv| Box::new(NetworkTraffic::new(reg, iv)),
};
