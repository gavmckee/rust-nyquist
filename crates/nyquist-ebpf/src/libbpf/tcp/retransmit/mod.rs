const NAME: &str = "tcp/retransmit";
const METRIC: &str = "tcp/retransmits";
const COUNTER_GROUP_WIDTH: usize = 8;
const MAX_CPUS: usize = 1024;

mod skel {
    include!(concat!(env!("OUT_DIR"), "/tcp_retransmit.bpf.rs"));
}

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use async_trait::async_trait;
use memmap2::MmapOptions;
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

enum State {
    Uninit,
    Disabled,
    Running {
        _skel: Box<skel::ModSkel<'static>>,
        ptr: *const u64,
    },
}

unsafe impl Send for State {}

pub struct Retransmit {
    interval: Duration,
    state: State,
    id: Option<MetricId>,
}

impl Retransmit {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        // source="ebpf" keeps this segment counter distinct from the tcpinfo
        // sampler's tcp_retransmits{port} event counter (same base name,
        // different origin and units). Mirrors cpu/usage.
        let id = reg.register(
            MetricDef::new(METRIC, Kind::Counter)
                .unit(Unit::Count)
                .labels(Labels::new().insert("source", "ebpf")),
        );
        Retransmit { interval, state: State::Uninit, id: Some(id) }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use std::mem::MaybeUninit;
        use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
        // Leaked so the skeleton's borrow of the OpenObject is genuinely 'static.
        let object = Box::leak(Box::new(MaybeUninit::uninit()));
        let open_skel = skel::ModSkelBuilder::default().open(object)?;
        let mut loaded = open_skel.load()?;
        loaded.attach()?;

        let raw_fd = loaded.maps.counters.as_fd().as_raw_fd();
        let dup_fd = unsafe { libc::dup(raw_fd) };
        anyhow::ensure!(dup_fd >= 0, "dup failed: {}", std::io::Error::last_os_error());
        let file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        let bytes = MAX_CPUS * COUNTER_GROUP_WIDTH * std::mem::size_of::<u64>();
        let mmap = unsafe { MmapOptions::new().len(bytes).map(&file)? };
        let ptr = mmap.as_ptr() as *const u64;
        std::mem::forget(mmap);

        let skel: Box<skel::ModSkel<'static>> = Box::new(loaded);
        self.state = State::Running { _skel: skel, ptr };
        Ok(())
    }

    fn total(&self) -> u64 {
        if let State::Running { ptr, .. } = &self.state {
            let slice = unsafe { std::slice::from_raw_parts(*ptr, MAX_CPUS * COUNTER_GROUP_WIDTH) };
            (0..MAX_CPUS).map(|cpu| slice[cpu * COUNTER_GROUP_WIDTH]).sum()
        } else {
            0
        }
    }
}

#[async_trait]
impl Sampler for Retransmit {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "tcp/retransmit: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!("tcp/retransmit attached (kprobe, mmap counter)");
                // Skip the attach tick — stale `now` inflates the next dt
                // (mirrors network/traffic).
                return Ok(());
            }
            State::Running { .. } => {}
        }
        let total = self.total();
        if let Some(id) = self.id {
            reg.record_counter(id, now, total);
        }
        Ok(())
    }
}

use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry {
    name: NAME,
    init: |reg, iv| Box::new(Retransmit::new(reg, iv)),
};
