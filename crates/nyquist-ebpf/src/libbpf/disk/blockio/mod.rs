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
        let devices = list_disks()
            .into_iter()
            .take(MAX_DEVICES)
            .map(|name| {
                // source="ebpf" so these since-agent-start counters do NOT
                // collide with the procfs disk sampler's identically-named
                // since-boot series. Without it both hash to the same MetricId
                // and the alternating origins produce huge false deltas that
                // pin the rate histogram at 2^39-1. Mirrors cpu/usage.
                let lbl = || Labels::new()
                    .insert("device", name.as_str())
                    .insert("source", "ebpf");
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
        // Leaked so the skeleton's borrow of the OpenObject is genuinely
        // 'static. A stack-local here + transmute leaves the skeleton holding
        // a dangling reference into a dead frame (UB, use-after-free on drop).
        let object = Box::leak(Box::new(MaybeUninit::uninit()));
        let open_skel = skel::ModSkelBuilder::default().open(object)?;
        let mut loaded = open_skel.load()?;

        // Populate name_to_slot before attaching so no events are missed.
        // Keys are gendisk names (the BPF program reads rq->part->bd_disk->
        // disk_name, which names the whole disk even for partition I/O).
        // Slots come from the SAME device list registration used, so names
        // and MetricIds can never skew (a second enumeration here used to
        // race device hotplug).
        let name_map = &loaded.maps.name_to_slot;
        for (slot, (name, _ids)) in self.devices.iter().enumerate() {
            name_map.update(&disk_name_key(name), &(slot as u32).to_ne_bytes(), MapFlags::ANY)?;
        }
        // NVMe native multipath: real I/O completes on HIDDEN per-controller
        // component disks (nvme0c0n1) rather than the visible head (nvme0n1).
        // Alias each component name to its head's slot so those completions
        // are attributed to the device operators actually see.
        for (component, head) in hidden_multipath_components() {
            if let Some(slot) = self.devices.iter().position(|(n, _)| *n == head) {
                name_map.update(&disk_name_key(&component), &(slot as u32).to_ne_bytes(), MapFlags::ANY)?;
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

        let skel: Box<skel::ModSkel<'static>> = Box::new(loaded);
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
                // Skip the attach tick — stale `now` inflates the next dt
                // (mirrors network/traffic).
                return Ok(());
            }
            State::Running { .. } => {}
        }

        if let State::Running { counter_ptr, latency_ptr, .. } = &self.state {
            let counters = unsafe { std::slice::from_raw_parts(*counter_ptr, COUNTERS_TOTAL) };
            let lat_slice = unsafe { std::slice::from_raw_parts(*latency_ptr, LATENCY_TOTAL) };

            for (slot, (_name, ids)) in self.devices.iter().enumerate() {
                let base = slot * GROUP_WIDTH;
                reg.record_counter(ids.read_bytes,     now, counters[base]);
                reg.record_counter(ids.write_bytes,    now, counters[base + 1]);
                reg.record_counter(ids.read_requests,  now, counters[base + 2]);
                reg.record_counter(ids.write_requests, now, counters[base + 3]);

                let rbase = slot * 2 * BPF_BUCKETS;
                let read_counts  = &lat_slice[rbase..rbase + BPF_BUCKETS];
                let write_counts = &lat_slice[rbase + BPF_BUCKETS..rbase + 2 * BPF_BUCKETS];
                reg.record_distribution_buckets_with_interval(ids.read_latency,  now, buckets_from_counts(read_counts), self.interval);
                reg.record_distribution_buckets_with_interval(ids.write_latency, now, buckets_from_counts(write_counts), self.interval);
            }
        }
        Ok(())
    }
}

/// Zero-padded 32-byte key matching the BPF program's `struct disk_name`
/// (kernel DISK_NAME_LEN = 32, always NUL-terminated).
fn disk_name_key(name: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    let b = name.as_bytes();
    let n = b.len().min(31);
    key[..n].copy_from_slice(&b[..n]);
    key
}

/// Visible physical disks under /sys/block (entries with a `dev` file),
/// sorted for stable slot assignment.
fn list_disks() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/sys/block") else { return Vec::new() };
    let mut disks: Vec<String> = dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.is_empty()
                || name.starts_with("loop")
                || name.starts_with("ram")
                || name.starts_with("zram")
            {
                return None;
            }
            // Hidden devices (NVMe multipath components) have no `dev` file;
            // they are aliased to their head, not given slots of their own.
            entry.path().join("dev").exists().then_some(name)
        })
        .collect();
    disks.sort();
    disks
}

/// Hidden NVMe multipath component disks and the visible head each belongs
/// to: nvme{ctrl}c{path}n{ns} → nvme{ctrl}n{ns}.
fn hidden_multipath_components() -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir("/sys/block") else { return Vec::new() };
    dir.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let head = nvme_multipath_head(&name)?;
            Some((name, head))
        })
        .collect()
}

/// Parse an NVMe multipath component name (nvme0c0n1) into its head (nvme0n1).
fn nvme_multipath_head(name: &str) -> Option<String> {
    let rest = name.strip_prefix("nvme")?;
    let (ctrl, rest) = rest.split_at(rest.find('c')?);
    let rest = &rest[1..];
    let (path, rest) = rest.split_at(rest.find('n')?);
    let ns = &rest[1..];
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if all_digits(ctrl) && all_digits(path) && all_digits(ns) {
        Some(format!("nvme{ctrl}n{ns}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipath_component_names_map_to_heads() {
        assert_eq!(nvme_multipath_head("nvme0c0n1").as_deref(), Some("nvme0n1"));
        assert_eq!(nvme_multipath_head("nvme12c3n45").as_deref(), Some("nvme12n45"));
        // Visible head / other devices are not components.
        assert_eq!(nvme_multipath_head("nvme0n1"), None);
        assert_eq!(nvme_multipath_head("sda"), None);
        assert_eq!(nvme_multipath_head("nvme0cXn1"), None);
    }

    #[test]
    fn disk_name_keys_are_zero_padded_and_bounded() {
        let k = disk_name_key("nvme0n1");
        assert_eq!(&k[..7], b"nvme0n1");
        assert!(k[7..].iter().all(|&b| b == 0));
        // 32+ char names truncate with a terminating NUL.
        let long = "x".repeat(40);
        let k = disk_name_key(&long);
        assert_eq!(k[31], 0);
    }
}

use linkme::distributed_slice;
use nyquist_core::registration::{SamplerEntry, SAMPLERS};
#[distributed_slice(SAMPLERS)]
static ENTRY: SamplerEntry = SamplerEntry {
    name: NAME,
    init: |reg, iv| Box::new(BlockIo::new(reg, iv)),
};
