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
        scaled: ScaledCounts,
        counts: Vec<u64>,
        pub running_percent: u64,
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
            Ok(PerfGroup { group, members, scaled: ScaledCounts::default(), counts: Vec::new(), running_percent: 0 })
        }

        /// Read every member with one syscall; `out` is refilled in the
        /// order of the `events` slice passed to `open`.
        pub fn read_into(&mut self, out: &mut Vec<u64>) -> Result<(), PerfError> {
            let data = self.group.read().map_err(classify)?;
            let enabled = data.time_enabled().unwrap_or_default().as_nanos() as u64;
            let running = data.time_running().unwrap_or_default().as_nanos() as u64;
            self.counts.clear();
            self.counts.extend(self.members.iter()
                .map(|m| data.get(m).map(|entry| entry.value()).unwrap_or(0)));
            self.running_percent = self.scaled.update(&self.counts, enabled, running, out);
            if self.running_percent == 0 { warn_unscheduled_once(self.members.len()); }
            Ok(())
        }
    }

    #[derive(Default)]
    struct ScaledCounts {
        previous: Vec<u64>,
        totals: Vec<u64>,
        enabled: u64,
        running: u64,
    }
    impl ScaledCounts {
        fn update(&mut self, counts: &[u64], enabled: u64, running: u64, out: &mut Vec<u64>) -> u64 {
            let reset = enabled < self.enabled || running < self.running || counts.len() != self.previous.len();
            let de = if reset { enabled } else { enabled - self.enabled };
            let dr = if reset { running } else { running - self.running };
            self.totals.resize(counts.len(), 0);
            for (i, &value) in counts.iter().enumerate() {
                let delta = if reset { value } else { value.saturating_sub(self.previous[i]) };
                if dr > 0 {
                    let scaled = (delta as u128 * de as u128 / dr as u128).min(u64::MAX as u128) as u64;
                    self.totals[i] = self.totals[i].saturating_add(scaled);
                }
            }
            self.previous.clear();
            self.previous.extend_from_slice(counts);
            self.enabled = enabled;
            self.running = running;
            out.clear();
            out.extend_from_slice(&self.totals);
            if de == 0 { 0 } else { (dr as u128 * 100 / de as u128).min(100) as u64 }
        }
    }

    fn warn_unscheduled_once(members: usize) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                members,
                "perf group did not run during this interval; rate estimate unavailable"
            );
        }
    }
    #[cfg(test)]
    mod scaling_tests {
        use super::*;
        #[test]
        fn changing_multiplex_ratio_scales_only_new_events() {
            let mut s = ScaledCounts::default();
            let mut out = Vec::new();
            assert_eq!(s.update(&[50], 100, 50, &mut out), 50);
            assert_eq!(out, [100]);
            assert_eq!(s.update(&[150], 200, 150, &mut out), 100);
            assert_eq!(out, [200]);
            assert_eq!(s.update(&[150], 300, 150, &mut out), 0);
            assert_eq!(out, [200]);
            s.update(&[200], 400, 200, &mut out);
            assert_eq!(out, [300]);
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
