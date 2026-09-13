//! inotify (fswatch) coverage for steering files BPF cannot intercept:
//!   /proc/irq/<n>/smp_affinity_list                 — IRQ CPU affinity
//!   /sys/class/net/<if>/queues/rx-*/rps_cpus        — RPS steering
//!   /sys/class/net/<if>/queues/tx-*/xps_cpus        — XPS steering
//!
//! IN_CLOSE_WRITE is a generic VFS-level hook (fsnotify on fput), so it
//! fires for sysfs and procfs files despite their virtual nature — the
//! `echo mask > rps_cpus` open/write/close pattern is exactly what trips it.
//!
//! inotify carries no writer identity: events synthesize an SwEvent with
//! SW_SRC_FSWATCH / comm="fswatch" that triggers a refresh, and the diff
//! identifies the changed keys. Sub-second detection, no attribution —
//! strictly better than the 60s poll it replaces for these surfaces.
//!
//! The watch list is rescanned every 60s (idempotent adds; watches on
//! deleted files self-remove), so queue reconfigs and NIC hotplug heal.

use std::sync::mpsc::SyncSender;
use crate::event::SwEvent;

pub struct InotifyWatcher;

impl InotifyWatcher {
    /// Spawn the self-contained watcher thread. Failure to create the
    /// inotify instance is fatal to the watcher only, not the caller.
    #[cfg(target_os = "linux")]
    pub fn spawn(tx: SyncSender<SwEvent>, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> anyhow::Result<()> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        anyhow::ensure!(fd >= 0, "inotify_init1: {}", std::io::Error::last_os_error());
        std::thread::Builder::new()
            .name("nyquist-fswatch".into())
            .spawn(move || run(fd, tx, stop))?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn spawn(_tx: SyncSender<SwEvent>, _stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn run(fd: i32, tx: SyncSender<SwEvent>, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use crate::event::SW_SRC_FSWATCH;
    use std::time::{Duration, Instant};

    let mut buf = [0u8; 4096];
    let mut last_scan: Option<Instant> = None;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        if last_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            refresh_watches(fd);
            last_scan = Some(Instant::now());
        }

        // Bounded poll so the watch-list rescan runs even when idle.
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let rc = unsafe { libc::poll(&mut pfd, 1, 200) };
        if rc <= 0 { continue; }

        // Drain everything queued; N rapid writes still mean ONE refresh.
        let mut saw_event = false;
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 { break; }
            saw_event = true;
        }
        if saw_event {
            // Which file changed doesn't matter: the refresh + snapshot diff
            // identifies the exact keys.
            let mut ev: SwEvent = unsafe { std::mem::zeroed() };
            ev.src = SW_SRC_FSWATCH;
            ev.comm[..7].copy_from_slice(b"fswatch");
            let _ = tx.try_send(ev);
        }
    }
    unsafe { libc::close(fd); }
}

/// (Re-)add watches for every current steering file. inotify_add_watch is
/// idempotent per path; failures (file vanished mid-scan) are fine.
#[cfg(target_os = "linux")]
fn refresh_watches(fd: i32) {
    let mut count = 0usize;
    for path in watch_paths() {
        let Ok(c) = std::ffi::CString::new(path) else { continue };
        let wd = unsafe { libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_CLOSE_WRITE) };
        if wd >= 0 { count += 1; }
    }
    tracing::debug!(watches = count, "fswatch: steering watch list refreshed");
}

#[cfg(target_os = "linux")]
fn watch_paths() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/sys/class/net") else { return out };
    for e in dir.flatten() {
        let Ok(iface) = e.file_name().into_string() else { continue };
        // Physical interfaces only — veth churn would burn watches on noise.
        if !e.path().join("device").exists() { continue; }

        if let Ok(queues) = std::fs::read_dir(e.path().join("queues")) {
            for q in queues.flatten() {
                let qname = q.file_name().into_string().unwrap_or_default();
                if qname.starts_with("rx-") {
                    out.push(q.path().join("rps_cpus").to_string_lossy().into_owned());
                } else if qname.starts_with("tx-") {
                    out.push(q.path().join("xps_cpus").to_string_lossy().into_owned());
                }
            }
        }
        if let Ok(irqs) = std::fs::read_dir(format!("/sys/class/net/{iface}/device/msi_irqs")) {
            for irq in irqs.flatten() {
                if let Ok(n) = irq.file_name().into_string().unwrap_or_default().parse::<u32>() {
                    out.push(format!("/proc/irq/{n}/smp_affinity_list"));
                }
            }
        }
    }
    out
}
