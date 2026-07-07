//! inotify watches for sysfs paths that BPF cannot intercept:
//!   /proc/irq/*/smp_affinity_list   — IRQ CPU affinity
//!   /sys/class/net/*/queues/rx-*/rps_cpus — RPS steering
//!   /sys/class/net/*/queues/tx-*/xps_cpus — XPS steering
//!
//! These are ordinary files (not /proc/sys/ virtual files), so
//! IN_CLOSE_WRITE fires reliably when userspace tools update them.
//!
//! TODO: implement using libc::inotify_init1 + libc::inotify_add_watch,
//! wrapped in tokio::io::unix::AsyncFd for non-blocking integration.
//! The watcher.rs event loop has a placeholder select arm for inotify.

pub struct InotifyWatcher;

impl InotifyWatcher {
    /// Build the watcher and add watches for all currently-known paths.
    /// Returns Ok(Self) even if no paths are found (they may appear later).
    pub fn new() -> anyhow::Result<Self> {
        // TODO: implement glob + inotify_add_watch for the three path patterns above.
        Ok(InotifyWatcher)
    }

    /// Block until the next inotify event (or timeout), returning the changed path.
    /// Currently a no-op stub; the watcher loop treats None as "no inotify event".
    pub fn poll_event(&self) -> Option<String> {
        None
    }
}
