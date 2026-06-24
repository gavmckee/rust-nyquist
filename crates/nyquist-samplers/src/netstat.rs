use std::time::{Duration, Instant};
use nyquist_core::model::{Kind, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};
use crate::procfs::parse_net_snmp;

// Key TCP health signals from /proc/net/netstat (TcpExt section).
// Fields absent in older kernels are silently skipped.
const TCPEXT_METRICS: &[(&str, &str)] = &[
    // Retransmission breakdown — distinguishes congestion from loss
    ("tcp/retrans/fast",         "TCPFastRetrans"),
    ("tcp/retrans/slow_start",   "TCPSlowStartRetrans"),
    ("tcp/retrans/syn",          "TCPSynRetrans"),
    ("tcp/retrans/fail",         "TCPRetransFail"),
    ("tcp/retrans/spurious_rto", "TCPSpuriousRTOs"),
    // Window pressure — receiver backpressure and sender stall
    ("tcp/window/zero_sent",     "TCPToZeroWindowAdv"),
    ("tcp/window/zero_recv",     "TCPFromZeroWindowAdv"),
    // Drop events — accept queue and receive queue saturation
    ("tcp/drop/listen",          "ListenDrops"),
    ("tcp/drop/listen_overflow", "ListenOverflows"),
    ("tcp/drop/backlog",         "TCPBacklogDrop"),
    ("tcp/drop/rcvq",            "TCPRcvQDrop"),
    ("tcp/drop/ofo",             "TCPOFODrop"),
    // Connection abort — hard failures and timeouts
    ("tcp/abort/timeout",        "TCPAbortOnTimeout"),
    ("tcp/abort/close",          "TCPAbortOnClose"),
    // Out-of-order queue — receiver-side reordering; elevated OFO indicates
    // packet reordering or loss in the fabric, not a sender problem
    ("tcp/ofo/queued",           "TCPOFOQueue"),
    ("tcp/ofo/merged",           "TCPOFOMerge"),
    // Receive buffer collapse — kernel reclaimed rcv queue space under pressure;
    // correlates with tcp_rmem_max being too small for the connection's BDP
    ("tcp/rcv_collapsed",        "TCPRcvCollapsed"),
    // DSACK — receiver sent duplicate SACK; high values mean spurious retransmits
    ("tcp/dsack/old_sent",       "TCPDSACKOldSent"),
    ("tcp/dsack/ofo_sent",       "TCPDSACKOfoSent"),
    ("tcp/dsack/recv",           "TCPDSACKRecv"),
    // Memory pressure — socket buffer allocator ran low
    ("tcp/mem_pressure/events",  "TCPMemoryPressures"),
];

pub struct NetstatSampler { interval: Duration }

impl NetstatSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self { NetstatSampler { interval } }
}

#[async_trait::async_trait]
impl Sampler for NetstatSampler {
    fn name(&self) -> &str { "netstat" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        let text = std::fs::read_to_string("/proc/net/netstat")?;
        let parsed = parse_net_snmp(&text);
        let Some(fields) = parsed.get("TcpExt") else { return Ok(()) };
        for (name, snmp_field) in TCPEXT_METRICS {
            if let Some(&value) = fields.get(*snmp_field) {
                let id = reg.register(MetricDef::new(*name, Kind::Counter).unit(Unit::Count));
                reg.record_counter(id, now, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_tcpext_counters() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let text = include_str!("../tests/fixtures/proc_net_netstat");
        let parsed = parse_net_snmp(text);
        let fields = parsed.get("TcpExt").unwrap();
        let now = std::time::Instant::now();
        for (name, snmp_field) in TCPEXT_METRICS {
            if let Some(&value) = fields.get(*snmp_field) {
                let id = reg.register(MetricDef::new(*name, Kind::Counter).unit(Unit::Count));
                reg.record_counter(id, now, value);
            }
        }
        // TCPBacklogDrop = 42 in fixture
        let backlog_id = reg.register(
            MetricDef::new("tcp/drop/backlog", Kind::Counter).unit(Unit::Count)
        );
        assert_eq!(reg.raw(backlog_id), 42);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "netstat",
    init: |reg, iv| Box::new(NetstatSampler::new(reg, iv)),
};
