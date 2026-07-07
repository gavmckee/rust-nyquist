use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::{parse_net_snmp, parse_net_snmp6};

const SNMP_PATH: &str = "/proc/net/snmp";
// IPv6 counters live in a separate file with a different format (one
// Key/Value pair per line). Absent when IPv6 is disabled — tolerated.
const SNMP6_PATH: &str = "/proc/net/snmp6";

// IP fragmentation, reassembly, and drop counters from the Ip: row.
// FragFails (DF bit set, can't fragment) and ReasmFails are the clearest
// indicators of PMTUD/MTU misconfiguration in the data path.
const IP_METRICS: &[(&str, &str)] = &[
    ("ip/reasm/requests",  "ReasmReqds"),
    ("ip/reasm/ok",        "ReasmOKs"),
    ("ip/reasm/failures",  "ReasmFails"),
    ("ip/frag/ok",         "FragOKs"),
    ("ip/frag/failures",   "FragFails"),
    ("ip/frag/created",    "FragCreates"),
    ("ip/in_discards",     "InDiscards"),
    ("ip/out_discards",    "OutDiscards"),
    ("ip/out_no_routes",   "OutNoRoutes"),
    ("ip/in_hdr_errors",   "InHdrErrors"),
];

// ICMP counters. InDestUnreachs includes type-3/code-4 "fragmentation needed"
// responses, which is the PMTUD signal visible at the IP layer.
const ICMP_METRICS: &[(&str, &str)] = &[
    ("icmp/in_dest_unreachable", "InDestUnreachs"),
    ("icmp/in_time_exceeded",    "InTimeExcds"),
    ("icmp/out_dest_unreachable","OutDestUnreachs"),
];

// IPv6 mirrors of IP_METRICS. Note the TCP MIB is family-agnostic in Linux
// (the Tcp: row of /proc/net/snmp covers v4+v6), so there is no tcp6 table.
const IP6_METRICS: &[(&str, &str)] = &[
    ("ip6/reasm/requests", "Ip6ReasmReqds"),
    ("ip6/reasm/ok",       "Ip6ReasmOKs"),
    ("ip6/reasm/failures", "Ip6ReasmFails"),
    ("ip6/frag/ok",        "Ip6FragOKs"),
    ("ip6/frag/failures",  "Ip6FragFails"),
    ("ip6/frag/created",   "Ip6FragCreates"),
    ("ip6/in_discards",    "Ip6InDiscards"),
    ("ip6/out_discards",   "Ip6OutDiscards"),
    ("ip6/out_no_routes",  "Ip6OutNoRoutes"),
    ("ip6/in_hdr_errors",  "Ip6InHdrErrors"),
];

// Icmp6InPktTooBigs is the IPv6 PMTUD signal (v6 routers never fragment;
// Packet Too Big is the only path-MTU feedback).
const ICMP6_METRICS: &[(&str, &str)] = &[
    ("icmp6/in_pkt_too_big",      "Icmp6InPktTooBigs"),
    ("icmp6/in_dest_unreachable", "Icmp6InDestUnreachs"),
    ("icmp6/in_time_exceeded",    "Icmp6InTimeExcds"),
    ("icmp6/out_dest_unreachable","Icmp6OutDestUnreachs"),
];

const UDP6_METRICS: &[(&str, &str)] = &[
    ("udp6/in_datagrams",  "Udp6InDatagrams"),
    ("udp6/out_datagrams", "Udp6OutDatagrams"),
    ("udp6/in_errors",     "Udp6InErrors"),
    ("udp6/no_ports",      "Udp6NoPorts"),
    ("udp6/rcvbuf_errors", "Udp6RcvbufErrors"),
    ("udp6/sndbuf_errors", "Udp6SndbufErrors"),
];

const TCP_METRICS: [(&str, &str); 5] = [
    ("tcp/active_opens",  "ActiveOpens"),
    ("tcp/passive_opens", "PassiveOpens"),
    ("tcp/in_segs",       "InSegs"),
    ("tcp/out_segs",      "OutSegs"),
    // Total retransmitted segments — all types (RTO + fast + SYN).
    // More inclusive than TCPFastRetrans (SACK-only) and TCPSynRetrans;
    // correlates with per-socket tcpi_retransmits reported by tools like xfr.
    ("tcp/retrans/segs",  "RetransSegs"),
];
const UDP_METRICS: [(&str, &str); 6] = [
    ("udp/in_datagrams",   "InDatagrams"),
    ("udp/out_datagrams",  "OutDatagrams"),
    ("udp/in_errors",      "InErrors"),
    ("udp/no_ports",       "NoPorts"),
    ("udp/rcvbuf_errors",  "RcvbufErrors"),
    ("udp/sndbuf_errors",  "SndbufErrors"),
];

fn ingest(reg: &Registry, now: Instant, text: &str, proto: &str, metrics: &[(&str, &str)]) {
    let parsed = parse_net_snmp(text);
    let Some(fields) = parsed.get(proto) else { return };
    for (name, snmp_field) in metrics {
        if let Some(&value) = fields.get(*snmp_field) {
            let id = reg.register(MetricDef::new(*name, Kind::Counter).unit(Unit::Count));
            reg.record_counter(id, now, value);
        }
    }
}

fn record_snmp6(
    reg: &Registry,
    now: Instant,
    fields: &std::collections::HashMap<String, u64>,
    metrics: &[(&str, &str)],
) {
    for (name, snmp6_field) in metrics {
        if let Some(&value) = fields.get(*snmp6_field) {
            let id = reg.register(MetricDef::new(*name, Kind::Counter).unit(Unit::Count));
            reg.record_counter(id, now, value);
        }
    }
}

pub struct IpSampler { interval: Duration }
impl IpSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self { IpSampler { interval } }
}
#[async_trait::async_trait]
impl Sampler for IpSampler {
    fn name(&self) -> &str { "ip" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(SNMP_PATH)?;
        ingest(reg, now, &text, "Ip",   IP_METRICS);
        ingest(reg, now, &text, "Icmp", ICMP_METRICS);
        // snmp6 is absent when IPv6 is disabled — skip silently, don't error.
        if let Ok(text6) = std::fs::read_to_string(SNMP6_PATH) {
            let fields = parse_net_snmp6(&text6);
            record_snmp6(reg, now, &fields, IP6_METRICS);
            record_snmp6(reg, now, &fields, ICMP6_METRICS);
        }
        Ok(())
    }
}

pub struct TcpSampler { interval: Duration }
impl TcpSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self { TcpSampler { interval } }
}
#[async_trait::async_trait]
impl Sampler for TcpSampler {
    fn name(&self) -> &str { "tcp" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(SNMP_PATH)?;
        ingest(reg, now, &text, "Tcp", &TCP_METRICS);
        Ok(())
    }
}

pub struct UdpSampler { interval: Duration }
impl UdpSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self { UdpSampler { interval } }
}
#[async_trait::async_trait]
impl Sampler for UdpSampler {
    fn name(&self) -> &str { "udp" }
    fn interval(&self) -> Duration { self.interval }
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string(SNMP_PATH)?;
        ingest(reg, now, &text, "Udp", &UDP_METRICS);
        if let Ok(text6) = std::fs::read_to_string(SNMP6_PATH) {
            let fields = parse_net_snmp6(&text6);
            record_snmp6(reg, now, &fields, UDP6_METRICS);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_and_udp_ingest_register_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let text = include_str!("../tests/fixtures/proc_net_snmp");
        let now = Instant::now();
        ingest(&reg, now, text, "Tcp", &TCP_METRICS);
        ingest(&reg, now, text, "Udp", &UDP_METRICS);
        assert_eq!(reg.metric_ids().len(), 11);
    }

    #[test]
    fn snmp6_ingest_registers_ipv6_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let text = include_str!("../tests/fixtures/proc_net_snmp6");
        let now = Instant::now();
        let fields = parse_net_snmp6(text);
        record_snmp6(&reg, now, &fields, IP6_METRICS);
        record_snmp6(&reg, now, &fields, ICMP6_METRICS);
        record_snmp6(&reg, now, &fields, UDP6_METRICS);
        // Fixture covers all 20 mapped fields.
        assert_eq!(reg.metric_ids().len(), 20);
        // Spot-check a raw value survives the pipeline (frag failures from fixture).
        let id = reg.register(MetricDef::new("ip6/frag/failures", Kind::Counter).unit(Unit::Count));
        assert_eq!(reg.raw(id), 6);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static IP_ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "ip",
    init: |reg, iv| Box::new(IpSampler::new(reg, iv)),
};

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static TCP_ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "tcp",
    init: |reg, iv| Box::new(TcpSampler::new(reg, iv)),
};

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static UDP_ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "udp",
    init: |reg, iv| Box::new(UdpSampler::new(reg, iv)),
};
