const NAME: &str = "network/traffic";

// 4 of 6 procfs /proc/net/dev metrics are covered here.
// rx_errors and tx_errors are driver-level hardware counters with no generic
// BPF hook; they remain in the procfs sampler as a documented gap.
// rx_dropped is approximate: only RX-path kfree_skb reasons are counted.
// tx_dropped is exact: net_dev_xmit rc != 0.
mod skel {
    include!(concat!(env!("OUT_DIR"), "/network_traffic.bpf.rs"));
}

use std::collections::HashMap;
use std::time::{Duration, Instant};
use async_trait::async_trait;
use libbpf_rs::{MapCore, MapFlags};
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct IfaceCounters {
    rx_bytes:   u64,
    tx_bytes:   u64,
    rx_dropped: u64,
    tx_dropped: u64,
}

enum State {
    Uninit,
    Disabled,
    Running {
        skel: Box<skel::ModSkel<'static>>,
    },
}

unsafe impl Send for State {}

struct IfaceIds {
    rx_bytes:   MetricId,
    tx_bytes:   MetricId,
    rx_dropped: MetricId,
    tx_dropped: MetricId,
}

pub struct NetworkTraffic {
    interval: Duration,
    state:    State,
    // ifindex → registered MetricIds; populated lazily as interfaces appear.
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
        let skel: Box<skel::ModSkel<'static>> = unsafe { std::mem::transmute(Box::new(loaded)) };
        self.state = State::Running { skel };
        Ok(())
    }

    fn iface_name(ifindex: u32) -> Option<String> {
        let mut buf = [0u8; libc::IF_NAMESIZE];
        let p = unsafe { libc::if_indextoname(ifindex, buf.as_mut_ptr() as *mut libc::c_char) };
        if p.is_null() {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8(buf[..end].to_vec()).ok()
    }

    fn ensure_ids(ids: &mut HashMap<u32, IfaceIds>, reg: &Registry, ifindex: u32) {
        if ids.contains_key(&ifindex) { return; }
        let Some(name) = Self::iface_name(ifindex) else { return };
        let lbl = || Labels::new().insert("iface", name.as_str());
        let entry = IfaceIds {
            rx_bytes:   reg.register(MetricDef::new("network/receive/bytes",    Kind::Counter).unit(Unit::Bytes).labels(lbl())),
            tx_bytes:   reg.register(MetricDef::new("network/transmit/bytes",   Kind::Counter).unit(Unit::Bytes).labels(lbl())),
            rx_dropped: reg.register(MetricDef::new("network/receive/dropped",  Kind::Counter).unit(Unit::Count).labels(lbl())),
            tx_dropped: reg.register(MetricDef::new("network/transmit/dropped", Kind::Counter).unit(Unit::Count).labels(lbl())),
        };
        ids.insert(ifindex, entry);
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
                tracing::info!("network/traffic attached (raw_tp+kfree_skb, percpu_hash)");
            }
            State::Running { .. } => {}
        }

        let State::Running { skel } = &self.state else { return Ok(()) };
        let map = &skel.maps.iface_counters_map;
        let cpu_count = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } as usize;

        for key in map.keys() {
            let Some(ifindex) = key.as_slice().try_into().ok().map(u32::from_ne_bytes)
            else { continue };

            let Ok(Some(per_cpu)) = map.lookup_percpu(&key, MapFlags::ANY) else { continue };

            let mut total = IfaceCounters::default();
            for cpu_val in per_cpu.iter().take(cpu_count) {
                if cpu_val.len() >= std::mem::size_of::<IfaceCounters>() {
                    let c: IfaceCounters = unsafe {
                        std::ptr::read_unaligned(cpu_val.as_ptr() as *const IfaceCounters)
                    };
                    total.rx_bytes   += c.rx_bytes;
                    total.tx_bytes   += c.tx_bytes;
                    total.rx_dropped += c.rx_dropped;
                    total.tx_dropped += c.tx_dropped;
                }
            }

            Self::ensure_ids(&mut self.ids, reg, ifindex);
            if let Some(ids) = self.ids.get(&ifindex) {
                reg.record_counter(ids.rx_bytes,   now, total.rx_bytes);
                reg.record_counter(ids.tx_bytes,   now, total.tx_bytes);
                reg.record_counter(ids.rx_dropped, now, total.rx_dropped);
                reg.record_counter(ids.tx_dropped, now, total.tx_dropped);
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
