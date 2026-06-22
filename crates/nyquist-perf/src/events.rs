//! CPU detection and PerfCounter abstraction.

pub fn num_cpus() -> usize {
    if let Ok(s) = std::fs::read_to_string("/sys/devices/system/cpu/present") {
        if let Some(end) = s.trim().split('-').last() {
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
        #[error("perf_event_open permission denied — requires CAP_PERFMON or perf_event_paranoid <= 1")]
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
            let counter = Builder::new()
                .kind(kind)
                .one_cpu(cpu)
                .any_pid()
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
