const NAME: &str = "cpu/usage";

// state_index in kernel's cpu_usage_stat enum → metric name suffix.
//
// cpuacct_account_field is reliably called for user/nice/system on all kernels.
// Idle, iowait, and irq use a separate vtime path on VIRT_CPU_ACCOUNTING_GEN
// kernels (common on modern distros) and do NOT fire this kprobe — emitting
// zeros there would be misleading. Softirq/irq/idle coverage is a follow-on
// that requires additional sched_switch and irq tracepoint probes.
const STATES: [(&str, usize); 3] = [
    ("user",   0), // CPUTIME_USER
    ("nice",   1), // CPUTIME_NICE
    ("system", 2), // CPUTIME_SYSTEM
];

const CPU_USAGE_GROUP_WIDTH: usize = 16;
const MAX_CPUS: usize = 1024;

mod skel {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
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

pub struct CpuUsage {
    interval: Duration,
    state: State,
    cpu_count: usize,
    // ids[cpu][state_slot] — registered eagerly, BPF loads lazily.
    ids: Vec<[MetricId; 3]>,
}

fn cpu_count_online() -> usize {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n <= 0 { 1 } else { n as usize }
}

impl CpuUsage {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        let cpu_count = cpu_count_online();
        let ids = (0..cpu_count).map(|cpu| {
            let mut row = [MetricId(0); 3];
            for (slot, &(field, _state_idx)) in STATES.iter().enumerate() {
                // Match procfs label format: "cpu0", "cpu1", etc.
                let labels = Labels::new().insert("cpu", &format!("cpu{cpu}"));
                row[slot] = reg.register(
                    MetricDef::new(format!("cpu/usage/{field}"), Kind::Counter)
                        .unit(Unit::Count)
                        .labels(labels),
                );
            }
            row
        }).collect();
        CpuUsage { interval, state: State::Uninit, cpu_count, ids }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use std::mem::MaybeUninit;
        use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
        let mut object = MaybeUninit::uninit();
        let open_skel = skel::ModSkelBuilder::default().open(&mut object)?;
        let mut loaded = open_skel.load()?;
        loaded.attach()?;

        let raw_fd = loaded.maps.cpu_usage.as_fd().as_raw_fd();
        let dup_fd = unsafe { libc::dup(raw_fd) };
        anyhow::ensure!(dup_fd >= 0, "dup failed: {}", std::io::Error::last_os_error());
        let file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        let bytes = MAX_CPUS * CPU_USAGE_GROUP_WIDTH * std::mem::size_of::<u64>();
        let mmap = unsafe { MmapOptions::new().len(bytes).map(&file)? };
        let ptr = mmap.as_ptr() as *const u64;
        std::mem::forget(mmap);

        let skel: Box<skel::ModSkel<'static>> = unsafe { std::mem::transmute(Box::new(loaded)) };
        self.state = State::Running { _skel: skel, ptr };
        Ok(())
    }
}

#[async_trait]
impl Sampler for CpuUsage {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "cpu/usage: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!(cpus = self.cpu_count, "cpu/usage attached (kprobe, mmap counters)");
            }
            State::Running { .. } => {}
        }

        if let State::Running { ptr, .. } = &self.state {
            let slice = unsafe {
                std::slice::from_raw_parts(*ptr, MAX_CPUS * CPU_USAGE_GROUP_WIDTH)
            };
            for (cpu, row) in self.ids.iter().enumerate() {
                for (slot, &(_field, state_idx)) in STATES.iter().enumerate() {
                    let val = slice[cpu * CPU_USAGE_GROUP_WIDTH + state_idx];
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
    init: |reg, iv| Box::new(CpuUsage::new(reg, iv)),
};
