use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

pub struct ProcessSampler {
    interval: Duration,
    cpu_user_id:   MetricId,
    cpu_system_id: MetricId,
    rss_id:        MetricId,
    vm_size_id:    MetricId,
    threads_id:    MetricId,
    fd_count_id:   MetricId,
}

impl ProcessSampler {
    pub fn new(reg: &Registry, interval: Duration) -> Self {
        let cpu_user_id   = reg.register(MetricDef::new("process/cpu/user",   Kind::Counter).unit(Unit::Count));
        let cpu_system_id = reg.register(MetricDef::new("process/cpu/system", Kind::Counter).unit(Unit::Count));
        let rss_id        = reg.register(MetricDef::new("process/memory/rss",     Kind::Gauge).unit(Unit::Bytes));
        let vm_size_id    = reg.register(MetricDef::new("process/memory/vm_size", Kind::Gauge).unit(Unit::Bytes));
        let threads_id    = reg.register(MetricDef::new("process/threads",  Kind::Gauge).unit(Unit::Count));
        let fd_count_id   = reg.register(MetricDef::new("process/fd_count", Kind::Gauge).unit(Unit::Count));
        Self { interval, cpu_user_id, cpu_system_id, rss_id, vm_size_id, threads_id, fd_count_id }
    }

    fn sample_cpu(&self, reg: &Registry, now: Instant) -> Option<()> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        // comm field is wrapped in parens and may contain spaces — find the last ')'
        let after = stat.rfind(')')? + 2;
        let fields: Vec<&str> = stat[after..].split_whitespace().collect();
        // from after ')': index 11=utime, 12=stime (0-based)
        let utime: u64 = fields.get(11)?.parse().ok()?;
        let stime: u64 = fields.get(12)?.parse().ok()?;
        reg.record_counter(self.cpu_user_id,   now, utime);
        reg.record_counter(self.cpu_system_id, now, stime);
        Some(())
    }

    fn sample_status(&self, reg: &Registry, now: Instant) -> Option<()> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(v) = line.strip_prefix("VmRSS:") {
                if let Ok(kb) = v.trim().trim_end_matches("kB").trim().parse::<u64>() {
                    reg.record_gauge(self.rss_id, now, kb * 1024);
                }
            } else if let Some(v) = line.strip_prefix("VmSize:") {
                if let Ok(kb) = v.trim().trim_end_matches("kB").trim().parse::<u64>() {
                    reg.record_gauge(self.vm_size_id, now, kb * 1024);
                }
            } else if let Some(v) = line.strip_prefix("Threads:") {
                if let Ok(t) = v.trim().parse::<u64>() {
                    reg.record_gauge(self.threads_id, now, t);
                }
            }
        }
        Some(())
    }

    fn sample_fds(&self, reg: &Registry, now: Instant) {
        if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
            let count = dir.count() as u64;
            // subtract 1 for the fd opened by read_dir itself
            reg.record_gauge(self.fd_count_id, now, count.saturating_sub(1));
        }
    }
}

#[async_trait::async_trait]
impl Sampler for ProcessSampler {
    fn name(&self) -> &str { "process" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        self.sample_cpu(reg, now);
        self.sample_status(reg, now);
        self.sample_fds(reg, now);
        Ok(())
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "process",
    init: |reg, iv| Box::new(ProcessSampler::new(reg, iv)),
};
