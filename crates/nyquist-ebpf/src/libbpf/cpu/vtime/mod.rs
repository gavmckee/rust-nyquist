const NAME: &str = "cpu/vtime";

// States tracked by this sampler (index into the per-CPU mmap slot).
// These complement cpu/usage (user/nice/system via cpuacct_account_field) by
// covering the states that vtime kernels don't route through that kprobe.
const STATES: [(&str, usize); 4] = [
    ("idle",    0),
    ("iowait",  1),
    ("irq",     2),
    ("softirq", 3),
];

const GROUP_WIDTH: usize = 4;
const MAX_CPUS: usize = 1024;

mod skel {
    include!(concat!(env!("OUT_DIR"), "/cpu_vtime.bpf.rs"));
}

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use async_trait::async_trait;
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use memmap2::MmapOptions;
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

enum State {
    Uninit,
    Disabled,
    Running {
        _skel: Box<skel::ModSkel<'static>>,
        ptr:   *const u64,
    },
}

unsafe impl Send for State {}

pub struct CpuVtime {
    interval:   Duration,
    state:      State,
    cpu_count:  usize,
    // ids[cpu][state_slot] — 4 states × cpu_count CPUs.
    ids:        Vec<[MetricId; 4]>,
}

fn cpu_count_online() -> usize {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n <= 0 { 1 } else { n as usize }
}

impl CpuVtime {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        let cpu_count = cpu_count_online();
        let ids = (0..cpu_count).map(|cpu| {
            let mut row = [MetricId(0); 4];
            for (slot, &(field, _idx)) in STATES.iter().enumerate() {
                // source="ebpf" keeps this kprobe/tracepoint series (nanoseconds)
                // from colliding with the procfs cpu sampler's identically-named
                // jiffies series for idle/iowait/irq/softirq. Same MetricId without
                // it → mixed units overflow the rate histogram. Mirrors cpu/usage.
                let labels = Labels::new()
                    .insert("cpu", format!("cpu{cpu}"))
                    .insert("source", "ebpf");
                row[slot] = reg.register(
                    MetricDef::new(format!("cpu/usage/{field}"), Kind::Counter)
                        .unit(Unit::Count)
                        .labels(labels),
                );
            }
            row
        }).collect();
        CpuVtime { interval, state: State::Uninit, cpu_count, ids }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use std::mem::MaybeUninit;
        // Leaked so the skeleton's borrow of the OpenObject is genuinely 'static.
        let object = Box::leak(Box::new(MaybeUninit::uninit()));
        let open_skel = skel::ModSkelBuilder::default().open(object)?;
        let mut loaded = open_skel.load()?;
        loaded.attach()?;

        let raw_fd = loaded.maps.cpu_state.as_fd().as_raw_fd();
        let dup_fd = unsafe { libc::dup(raw_fd) };
        anyhow::ensure!(dup_fd >= 0, "dup failed: {}", std::io::Error::last_os_error());
        let file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        let bytes = MAX_CPUS * GROUP_WIDTH * std::mem::size_of::<u64>();
        let mmap = unsafe { MmapOptions::new().len(bytes).map(&file)? };
        let ptr = mmap.as_ptr() as *const u64;
        std::mem::forget(mmap);

        let skel: Box<skel::ModSkel<'static>> = Box::new(loaded);
        self.state = State::Running { _skel: skel, ptr };
        Ok(())
    }
}

#[async_trait]
impl Sampler for CpuVtime {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "cpu/vtime: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!(
                    cpus = self.cpu_count,
                    "cpu/vtime attached (sched_switch + irq/softirq tracepoints, mmap counters)"
                );
                // Skip the attach tick — stale `now` inflates the next dt
                // (mirrors network/traffic).
                return Ok(());
            }
            State::Running { .. } => {}
        }

        if let State::Running { ptr, .. } = &self.state {
            let slice = unsafe {
                std::slice::from_raw_parts(*ptr, MAX_CPUS * GROUP_WIDTH)
            };
            for (cpu, row) in self.ids.iter().enumerate() {
                for (slot, &(_field, state_idx)) in STATES.iter().enumerate() {
                    let val = slice[cpu * GROUP_WIDTH + state_idx];
                    reg.record_counter(row[slot], now, val);
                }
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
    init: |reg, iv| Box::new(CpuVtime::new(reg, iv)),
};
