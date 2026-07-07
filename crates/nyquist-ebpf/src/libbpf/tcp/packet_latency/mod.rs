const NAME: &str = "tcp/packet_latency";
const METRIC: &str = "tcp/rtt_us";

mod skel {
    include!(concat!(env!("OUT_DIR"), "/tcp_packet_latency.bpf.rs"));
}

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use async_trait::async_trait;
use memmap2::MmapOptions;
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::libbpf::h2::{BPF_BUCKETS, buckets_from_counts};

enum State {
    Uninit,
    Disabled,
    Running {
        _skel: Box<skel::ModSkel<'static>>,
        ptr: *const u64,
        dropped_ptr: *const u64,
    },
}

// SAFETY: ptr stays valid as long as _skel + the mmap (both in Running).
unsafe impl Send for State {}

pub struct PacketLatency {
    interval: Duration,
    state: State,
    metric_id: Option<MetricId>,
    dropped_id: MetricId,
}

impl PacketLatency {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        // source="ebpf" keeps this distribution distinct from the tcpinfo
        // sampler's tcp_rtt_us{port} gauge series (same base name, different
        // origin and units). Mirrors cpu/usage.
        let id = reg.register(
            MetricDef::new(METRIC, Kind::Distribution)
                .unit(Unit::None)
                .labels(Labels::new().insert("source", "ebpf")),
        );
        // Self-metric: samples lost because the flow-tracking map was full.
        // Nonzero here means the latency distribution is biased against the
        // busiest periods — alert on it rather than trusting quiet percentiles.
        let dropped_id = reg.register(
            MetricDef::new("nyquist/bpf/dropped_samples", Kind::Counter)
                .unit(Unit::Count)
                .labels(Labels::new().insert("sampler", NAME)),
        );
        PacketLatency { interval, state: State::Uninit, metric_id: Some(id), dropped_id }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use std::mem::MaybeUninit;
        use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
        // Leaked so the skeleton's borrow of the OpenObject is genuinely 'static.
        let object = Box::leak(Box::new(MaybeUninit::uninit()));
        let open_skel = skel::ModSkelBuilder::default().open(object)?;
        let mut loaded = open_skel.load()?;
        loaded.attach()?;

        // dup each map fd and mmap it; leak the mmaps so they live with the skel.
        let mmap_fd = |raw_fd: i32, entries: usize| -> anyhow::Result<*const u64> {
            let dup_fd = unsafe { libc::dup(raw_fd) };
            anyhow::ensure!(dup_fd >= 0, "dup failed: {}", std::io::Error::last_os_error());
            let file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
            let bytes = entries * std::mem::size_of::<u64>();
            let mmap = unsafe { MmapOptions::new().len(bytes).map(&file)? };
            let ptr = mmap.as_ptr() as *const u64;
            std::mem::forget(mmap); // intentionally leaked; lives with the skel
            Ok(ptr)
        };
        let ptr = mmap_fd(loaded.maps.latency.as_fd().as_raw_fd(), BPF_BUCKETS)?;
        let dropped_ptr = mmap_fd(loaded.maps.dropped.as_fd().as_raw_fd(), 8)?;

        let skel: Box<skel::ModSkel<'static>> = Box::new(loaded);
        self.state = State::Running { _skel: skel, ptr, dropped_ptr };
        Ok(())
    }

    fn read_counts(&self) -> Vec<u64> {
        if let State::Running { ptr, .. } = &self.state {
            // SAFETY: ptr valid and len=BPF_BUCKETS while Running.
            unsafe { std::slice::from_raw_parts(*ptr, BPF_BUCKETS) }.to_vec()
        } else {
            Vec::new()
        }
    }
}

#[async_trait]
impl Sampler for PacketLatency {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "tcp/packet_latency: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!("tcp/packet_latency attached (raw_tp, mmap histogram)");
            }
            State::Running { .. } => {}
        }
        let counts = self.read_counts();
        if let Some(id) = self.metric_id {
            reg.record_distribution_buckets(id, now, buckets_from_counts(&counts));
        }
        if let State::Running { dropped_ptr, .. } = &self.state {
            // SAFETY: slot 0 of the 8-entry mmapable dropped array.
            let dropped = unsafe { std::ptr::read_volatile(*dropped_ptr) };
            reg.record_counter(self.dropped_id, now, dropped);
        }
        Ok(())
    }
}

use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry {
    name: NAME,
    init: |reg, iv| Box::new(PacketLatency::new(reg, iv)),
};
