//! CPU detection and PerfGroup abstraction.

pub fn num_cpus() -> usize {
    if let Ok(s) = std::fs::read_to_string("/sys/devices/system/cpu/present") {
        if let Some(end) = s.trim().split('-').next_back() {
            if let Ok(n) = end.parse::<usize>() {
                return n + 1;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(target_os = "linux")]
pub use linux::PerfGroup;

#[cfg(target_os = "linux")]
pub mod linux {
    use perf_event::events::Event;
    use perf_event::{Builder, Counter, Group};

    #[derive(Debug, thiserror::Error)]
    pub enum PerfError {
        #[error("perf_event_open permission denied — system-wide counters require CAP_PERFMON or perf_event_paranoid <= 0")]
        Permission,
        #[error("perf_event_open error: {0}")]
        Io(#[from] std::io::Error),
    }

    fn classify(e: std::io::Error) -> PerfError {
        if e.raw_os_error() == Some(libc::EPERM) || e.raw_os_error() == Some(libc::EACCES) {
            PerfError::Permission
        } else {
            PerfError::Io(e)
        }
    }

    /// A group of counters pinned to one CPU, counting all processes with
    /// kernel time included, read with a SINGLE read() syscall.
    ///
    /// The predecessor read each counter individually: events x CPUs
    /// syscalls per tick (~2k/tick on a 224-CPU host — measured at whole
    /// cores of system time at a 10 ms tick). The group leader is a
    /// software DUMMY event, so the leader itself never competes for a
    /// hardware PMU slot; callers must still keep hardware groups small
    /// enough to co-schedule (see HW event grouping in hw.rs).
    pub struct PerfGroup {
        group: Group,
        members: Vec<Counter>,
    }

    impl PerfGroup {
        /// Open a group on `cpu` for `events`, in order.
        pub fn open<E: Event + Copy>(cpu: usize, events: &[E]) -> Result<Self, PerfError> {
            let mut gb = Group::builder();
            gb.one_cpu(cpu).any_pid();
            let mut group = gb.build_group().map_err(classify)?;

            let mut members = Vec::with_capacity(events.len());
            for &ev in events {
                let mut b = Builder::new(ev);
                // Scope must match the group's; count kernel + hypervisor
                // time (a system-wide agent wants kernel time most of all).
                b.one_cpu(cpu).any_pid().include_kernel().include_hv();
                members.push(group.add(&b).map_err(classify)?);
            }
            group.enable().map_err(classify)?;
            Ok(PerfGroup { group, members })
        }

        /// Read every member with one syscall; `out` is refilled in the
        /// order of the `events` slice passed to `open`.
        pub fn read_into(&mut self, out: &mut Vec<u64>) -> Result<(), PerfError> {
            let data = self.group.read().map_err(classify)?;
            // time_running == 0 with time_enabled > 0 means the kernel never
            // scheduled this group — a hardware group too large for the PMU.
            // Counts would read as frozen zeros; make that loud, once.
            if data.time_enabled().is_some_and(|e| !e.is_zero())
                && data.time_running().is_none_or(|r| r.is_zero())
            {
                warn_unscheduled_once(self.members.len());
            }
            out.clear();
            for m in &self.members {
                out.push(data.get(m).map(|entry| entry.value()).unwrap_or(0));
            }
            Ok(())
        }
    }

    fn warn_unscheduled_once(members: usize) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                members,
                "perf group enabled but never scheduled — hardware group too large for this PMU? counters will read 0"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_cpus_returns_positive() {
        let n = num_cpus();
        assert!(n >= 1, "num_cpus returned {n}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn perm_error_is_distinct() {
        use linux::PerfError;
        let e = PerfError::Permission;
        let s = e.to_string();
        assert!(s.contains("CAP_PERFMON"), "{s}");
    }
}
