use std::time::Duration;
use nyquist_core::registry::Registry;
use nyquist_core::sampler::Sampler;
use crate::cpu::CpuSampler;
use crate::memory::MemorySampler;
use crate::network::NetworkSampler;
use crate::disk::DiskSampler;
use crate::snmp::{IpSampler, TcpSampler, UdpSampler};
use crate::loadavg::LoadAvgSampler;
use crate::psi::PsiSampler;
use crate::sockstat::SockstatSampler;
use crate::netstat::NetstatSampler;
use crate::softirqs::SoftirqSampler;
use crate::tcpinfo::TcpInfoSampler;
use crate::nic_stats::NicStatsSampler;

pub fn all_sampler_names() -> &'static [&'static str] {
    &["cpu", "memory", "network", "disk", "ip", "tcp", "udp", "loadavg", "psi", "sockstat", "netstat", "softirqs", "tcpinfo", "nic_stats"]
}

pub fn build_enabled(
    reg: &Registry,
    default_interval: Duration,
    is_enabled: impl Fn(&str) -> bool,
    interval_for: impl Fn(&str) -> Option<Duration>,
) -> Vec<Box<dyn Sampler>> {
    let mut out: Vec<Box<dyn Sampler>> = Vec::new();
    for name in all_sampler_names() {
        if !is_enabled(name) { continue; }
        let iv = interval_for(name).unwrap_or(default_interval);
        let s: Box<dyn Sampler> = match *name {
            "cpu"      => Box::new(CpuSampler::new(reg, iv)),
            "memory"   => Box::new(MemorySampler::new(reg, iv)),
            "network"  => Box::new(NetworkSampler::new(reg, iv)),
            "disk"     => Box::new(DiskSampler::new(reg, iv)),
            "ip"       => Box::new(IpSampler::new(reg, iv)),
            "tcp"      => Box::new(TcpSampler::new(reg, iv)),
            "udp"      => Box::new(UdpSampler::new(reg, iv)),
            "loadavg"  => Box::new(LoadAvgSampler::new(reg, iv)),
            "psi"      => Box::new(PsiSampler::new(reg, iv)),
            "sockstat"  => Box::new(SockstatSampler::new(reg, iv)),
            "netstat"   => Box::new(NetstatSampler::new(reg, iv)),
            "softirqs"  => Box::new(SoftirqSampler::new(reg, iv)),
            "tcpinfo"   => Box::new(TcpInfoSampler::new(reg, iv)),
            "nic_stats" => Box::new(NicStatsSampler::new(reg, iv)),
            _ => continue,
        };
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_enabled_flag() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let samplers = build_enabled(
            &reg,
            Duration::from_millis(10),
            |name| name != "network",
            |_| None,
        );
        let names: Vec<_> = samplers.iter().map(|s| s.name().to_string()).collect();
        assert_eq!(names, vec!["cpu", "memory", "disk", "ip", "tcp", "udp", "loadavg", "psi", "sockstat", "netstat", "softirqs", "tcpinfo", "nic_stats"]);
    }
}
