const NAME: &str = "disk/blockio";

mod skel {
    include!(concat!(env!("OUT_DIR"), "/disk_blockio.bpf.rs"));
}

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use async_trait::async_trait;
use libbpf_rs::{MapCore, MapFlags};
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use memmap2::MmapOptions;
use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::libbpf::h2::{BPF_BUCKETS, buckets_from_counts};

const GROUP_WIDTH: usize = 8;
const MAX_DEVICES: usize = 64;
const COUNTERS_TOTAL: usize = MAX_DEVICES * GROUP_WIDTH;
const LATENCY_TOTAL: usize = MAX_DEVICES * 2 * BPF_BUCKETS;

struct DeviceIds {
    read_bytes:    MetricId,
    write_bytes:   MetricId,
    read_requests: MetricId,
    write_requests: MetricId,
    read_latency:  MetricId,
    write_latency: MetricId,
}

enum State {
    Uninit,
    Disabled,
    Running {
        _skel:       Box<skel::ModSkel<'static>>,
        counter_ptr: *const u64,
        latency_ptr: *const u64,
    },
}

unsafe impl Send for State {}

pub struct BlockIo {
    interval: Duration,
    state:    State,
    // slot → (device_name, ids)
    devices:  Vec<(String, DeviceIds)>,
}

impl BlockIo {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        let devices = enumerate_block_devices()
            .into_iter()
            .take(MAX_DEVICES)
            .map(|(name, _devts)| {
                let lbl = || Labels::new().insert("device", name.as_str());
                let ids = DeviceIds {
                    read_bytes:     reg.register(MetricDef::new("disk/read/bytes",     Kind::Counter).unit(Unit::Bytes).labels(lbl())),
                    write_bytes:    reg.register(MetricDef::new("disk/write/bytes",    Kind::Counter).unit(Unit::Bytes).labels(lbl())),
                    read_requests:  reg.register(MetricDef::new("disk/read/requests",  Kind::Counter).unit(Unit::Count).labels(lbl())),
                    write_requests: reg.register(MetricDef::new("disk/write/requests", Kind::Counter).unit(Unit::Count).labels(lbl())),
                    read_latency:   reg.register(MetricDef::new("disk/read/latency",   Kind::Distribution).unit(Unit::None).labels(lbl())),
                    write_latency:  reg.register(MetricDef::new("disk/write/latency",  Kind::Distribution).unit(Unit::None).labels(lbl())),
                };
                (name, ids)
            })
            .collect();
        BlockIo { interval, state: State::Uninit, devices }
    }

    fn try_init(&mut self) -> anyhow::Result<()> {
        use std::mem::MaybeUninit;
        let mut object = MaybeUninit::uninit();
        let open_skel = skel::ModSkelBuilder::default().open(&mut object)?;
        let mut loaded = open_skel.load()?;

        // Populate devt_to_slot before attaching so no events are missed.
        // Partition dev_ts map to the parent disk's slot so partition I/O is
        // attributed to the correct disk (most system I/O is to partitions).
        let devt_map = &loaded.maps.devt_to_slot;
        for (slot, (_name, all_devts)) in enumerate_block_devices().into_iter().take(MAX_DEVICES).enumerate() {
            for devt in all_devts {
                let key = (devt as u32).to_ne_bytes();
                let val = (slot as u32).to_ne_bytes();
                devt_map.update(&key, &val, MapFlags::ANY)?;
            }
        }

        loaded.attach()?;

        let counter_ptr = mmap_map_fd(
            loaded.maps.counters.as_fd().as_raw_fd(),
            COUNTERS_TOTAL,
        )?;
        let latency_ptr = mmap_map_fd(
            loaded.maps.latency.as_fd().as_raw_fd(),
            LATENCY_TOTAL,
        )?;

        let skel: Box<skel::ModSkel<'static>> =
            unsafe { std::mem::transmute(Box::new(loaded)) };
        self.state = State::Running { _skel: skel, counter_ptr, latency_ptr };
        Ok(())
    }
}

fn mmap_map_fd(raw_fd: i32, len_entries: usize) -> anyhow::Result<*const u64> {
    let dup_fd = unsafe { libc::dup(raw_fd) };
    anyhow::ensure!(dup_fd >= 0, "dup failed: {}", std::io::Error::last_os_error());
    let file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
    let bytes = len_entries * std::mem::size_of::<u64>();
    let mmap = unsafe { MmapOptions::new().len(bytes).map(&file)? };
    let ptr = mmap.as_ptr() as *const u64;
    std::mem::forget(mmap);
    Ok(ptr)
}

#[async_trait]
impl Sampler for BlockIo {
    fn name(&self) -> &str { NAME }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        match &self.state {
            State::Disabled => return Ok(()),
            State::Uninit => {
                if let Err(e) = self.try_init() {
                    tracing::warn!(error = %e, "disk/blockio: load failed, disabling");
                    self.state = State::Disabled;
                    return Ok(());
                }
                tracing::info!(
                    devices = self.devices.len(),
                    "disk/blockio attached (raw_tp/block_rq_complete, mmap arrays)"
                );
            }
            State::Running { .. } => {}
        }

        if let State::Running { counter_ptr, latency_ptr, .. } = &self.state {
            let counters = unsafe { std::slice::from_raw_parts(*counter_ptr, COUNTERS_TOTAL) };
            let lat_slice = unsafe { std::slice::from_raw_parts(*latency_ptr, LATENCY_TOTAL) };

            for (slot, (_name, ids)) in self.devices.iter().enumerate() {
                let base = slot * GROUP_WIDTH;
                reg.record_counter(ids.read_bytes,     now, counters[base + 0]);
                reg.record_counter(ids.write_bytes,    now, counters[base + 1]);
                reg.record_counter(ids.read_requests,  now, counters[base + 2]);
                reg.record_counter(ids.write_requests, now, counters[base + 3]);

                let rbase = slot * 2 * BPF_BUCKETS;
                let read_counts  = &lat_slice[rbase..rbase + BPF_BUCKETS];
                let write_counts = &lat_slice[rbase + BPF_BUCKETS..rbase + 2 * BPF_BUCKETS];
                reg.record_distribution_buckets(ids.read_latency,  buckets_from_counts(read_counts));
                reg.record_distribution_buckets(ids.write_latency, buckets_from_counts(write_counts));
            }
        }
        Ok(())
    }
}

/// Return (device_name, [disk_devt, partition_devts…]) for all physical block
/// devices under /sys/block. Partition dev_ts share the parent's slot so that
/// I/O issued to any partition of a disk is attributed to the disk.
fn enumerate_block_devices() -> Vec<(String, Vec<u64>)> {
    let Ok(dir) = std::fs::read_dir("/sys/block") else { return Vec::new() };
    let mut devices = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().into_string().unwrap_or_default();
        if name.is_empty()
            || name.starts_with("loop")
            || name.starts_with("ram")
            || name.starts_with("zram")
        {
            continue;
        }
        let dev_path = format!("/sys/block/{name}/dev");
        let Ok(content) = std::fs::read_to_string(&dev_path) else { continue };
        let Ok(disk_devt) = parse_devt(content.trim()) else { continue };

        let mut all_devts = vec![disk_devt];
        // Include partition dev_ts so block_rq_complete events for partitions
        // (the common case for filesystem I/O) resolve to the parent disk slot.
        if let Ok(subdir) = std::fs::read_dir(format!("/sys/block/{name}")) {
            for sub in subdir.flatten() {
                let sub_name = sub.file_name().into_string().unwrap_or_default();
                if !sub_name.starts_with(name.as_str()) { continue; }
                let part_dev = format!("/sys/block/{name}/{sub_name}/dev");
                if let Ok(c) = std::fs::read_to_string(&part_dev) {
                    if let Ok(pdevt) = parse_devt(c.trim()) {
                        all_devts.push(pdevt);
                    }
                }
            }
        }

        devices.push((name, all_devts));
    }
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    devices
}

fn parse_devt(s: &str) -> Result<u64, ()> {
    let (maj, min) = s.split_once(':').ok_or(())?;
    let major: u64 = maj.parse().map_err(|_| ())?;
    let minor: u64 = min.parse().map_err(|_| ())?;
    // Linux dev_t: MKDEV(major, minor) = (major << 20) | minor (blkdev extended)
    Ok((major << 20) | minor)
}

use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry {
    name: NAME,
    init: |reg, iv| Box::new(BlockIo::new(reg, iv)),
};
