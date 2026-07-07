//! CPU detection and PerfCounter abstraction.

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
pub use linux::PerfCounter;

#[cfg(target_os = "linux")]
pub mod linux {
    use perf_event::Builder;

    #[derive(Debug, thiserror::Error)]
    pub enum PerfError {
        #[error("perf_event_open permission denied — system-wide counters require CAP_PERFMON or perf_event_paranoid <= 0")]
        Permission,
        #[error("perf_event_open error: {0}")]
        Io(#[from] std::io::Error),
    }

    pub struct PerfCounter {
        pub counter: perf_event::Counter,
    }

    impl PerfCounter {
        pub fn open(
            cpu: usize,
            kind: impl Into<perf_event::events::Event>,
        ) -> Result<Self, PerfError> {
            // Builder defaults to disabled(1) + exclude_kernel(1)/exclude_hv(1):
            // without enabled(true) the counter never counts (reads stay 0), and
            // for a system-wide agent kernel time is most of what we want to see.
            let mut builder = Builder::new().kind(kind).one_cpu(cpu).any_pid();
            builder.enabled(true).include_kernel().include_hv();
            let counter = builder
                .build()
                .map_err(|e| {
                    if e.raw_os_error() == Some(libc::EPERM)
                        || e.raw_os_error() == Some(libc::EACCES)
                    {
                        PerfError::Permission
                    } else {
                        PerfError::Io(e)
                    }
                })?;
            Ok(PerfCounter { counter })
        }

        pub fn read(&mut self) -> Result<u64, PerfError> {
            Ok(self.counter.read()?)
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
