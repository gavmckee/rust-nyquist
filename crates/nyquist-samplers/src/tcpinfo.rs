use std::collections::HashMap;
use std::time::{Duration, Instant};

use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

use crate::inet_diag::{query_tcp_connections, TcpStats};

/// Service port heuristic: the non-ephemeral port is the server side.
/// If both are non-ephemeral, use the lower (server ports are typically smaller).
/// Returns None if both are ephemeral (direct peer-to-peer, uncommon).
///
/// `ephemeral_floor` comes from net.ipv4.ip_local_port_range (default 32768).
/// The previous hardcoded 49152 (the IANA dynamic-range start) misclassified
/// half of Linux's actual ephemeral range as service ports, minting up to
/// ~16k junk per-port series on busy client hosts.
fn service_port(sport: u16, dport: u16, ephemeral_floor: u16) -> Option<u16> {
    match (sport < ephemeral_floor, dport < ephemeral_floor) {
        (true, false)  => Some(sport),
        (false, true)  => Some(dport),
        (true, true)   => Some(sport.min(dport)),
        (false, false) => None,
    }
}

/// Lower bound of the kernel's ephemeral port range.
fn ephemeral_floor() -> u16 {
    std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|t| t.parse().ok()))
        .unwrap_or(32768)
}

pub struct TcpInfoSampler {
    interval: Duration,
    ephemeral_floor: u16,
    rtt_ids:     HashMap<u16, MetricId>,
    retrans_ids: HashMap<u16, MetricId>,
    // inode → (service_port, total_retrans at last sample)
    prev_retrans: HashMap<u32, (u16, u32)>,
    // service_port → cumulative retransmit delta (only increases)
    retrans_accum: HashMap<u16, u64>,
    disabled: bool,
}

impl TcpInfoSampler {
    pub fn new(_reg: &Registry, interval: Duration) -> Self {
        TcpInfoSampler {
            interval,
            ephemeral_floor: ephemeral_floor(),
            rtt_ids: HashMap::new(),
            retrans_ids: HashMap::new(),
            prev_retrans: HashMap::new(),
            retrans_accum: HashMap::new(),
            disabled: false,
        }
    }

    fn rtt_id(&mut self, reg: &Registry, port: u16) -> MetricId {
        *self.rtt_ids.entry(port).or_insert_with(|| {
            reg.register(
                MetricDef::new("tcp/rtt_us", Kind::Gauge)
                    .unit(Unit::None)
                    .labels(Labels::new().insert("port", port.to_string())),
            )
        })
    }

    fn retrans_id(&mut self, reg: &Registry, port: u16) -> MetricId {
        *self.retrans_ids.entry(port).or_insert_with(|| {
            reg.register(
                MetricDef::new("tcp/retransmits", Kind::Counter)
                    .unit(Unit::Count)
                    .labels(Labels::new().insert("port", port.to_string())),
            )
        })
    }

    fn process(&mut self, reg: &Registry, now: Instant, conns: Vec<TcpStats>) {
        let mut seen_inodes = std::collections::HashSet::new();

        for c in &conns {
            // TIME_WAIT/SYN sockets report inode 0 and carry no meaningful
            // tcp_info; they would all share one key in prev_retrans.
            if c.inode == 0 { continue; }
            let Some(port) = service_port(c.sport, c.dport, self.ephemeral_floor) else { continue };
            seen_inodes.insert(c.inode);

            // RTT histogram
            if c.rtt_us > 0 {
                let id = self.rtt_id(reg, port);
                reg.record_gauge(id, now, c.rtt_us as u64);
            }

            // Retransmit delta tracking. First sight of a connection (agent
            // start, or inode reuse) only baselines: booking its lifetime
            // total_retrans as one interval's delta produced a bogus rate
            // spike at every agent restart.
            let delta = match self.prev_retrans.get(&c.inode) {
                Some(&(prev_port, prev_count))
                    if prev_port == port && c.total_retrans >= prev_count =>
                {
                    c.total_retrans - prev_count
                }
                _ => 0,
            };
            self.prev_retrans.insert(c.inode, (port, c.total_retrans));

            if delta > 0 {
                let acc = self.retrans_accum.entry(port).or_insert(0);
                *acc += delta as u64;
            }
        }

        // Flush retransmit counters to registry (collect first to release the borrow)
        let ports_totals: Vec<(u16, u64)> = self.retrans_accum.iter().map(|(&p, &t)| (p, t)).collect();
        for (port, total) in ports_totals {
            let id = self.retrans_id(reg, port);
            reg.record_counter(id, now, total);
        }

        // Prune stale inodes so our prev_retrans map doesn't grow unbounded
        self.prev_retrans.retain(|inode, _| seen_inodes.contains(inode));
    }
}

#[async_trait::async_trait]
impl Sampler for TcpInfoSampler {
    fn name(&self) -> &str { "tcpinfo" }
    fn interval(&self) -> Duration { self.interval }

    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError> {
        if self.disabled { return Ok(()); }

        match query_tcp_connections() {
            Ok(conns) => {
                self.process(reg, now, conns);
                Ok(())
            }
            Err(e) => {
                let raw = e.raw_os_error().unwrap_or(0);
                // Permanent conditions: no permission, inet_diag/tcp_diag
                // module absent (minimal container kernels), family
                // unsupported. Disable rather than warn-spamming every tick.
                if matches!(
                    raw,
                    libc::EPERM | libc::EACCES | libc::ENOENT
                        | libc::EPROTONOSUPPORT | libc::EAFNOSUPPORT
                ) {
                    tracing::warn!(
                        error = %e,
                        "tcpinfo: INET_DIAG query failed permanently, disabling sampler"
                    );
                    self.disabled = true;
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_port_heuristic() {
        const FLOOR: u16 = 32768; // Linux default ip_local_port_range lower bound
        assert_eq!(service_port(80, 54321, FLOOR), Some(80));    // server at 80
        assert_eq!(service_port(54321, 443, FLOOR), Some(443));  // client to 443
        assert_eq!(service_port(22, 9100, FLOOR), Some(22));     // both registered, lower wins
        assert_eq!(service_port(54321, 60000, FLOOR), None);     // both ephemeral
        // Regression: 32768-49151 is ephemeral on Linux; the old hardcoded
        // 49152 floor classified 40000 as a service port.
        assert_eq!(service_port(40000, 443, FLOOR), Some(443));
        assert_eq!(service_port(40000, 45000, FLOOR), None);
    }

    #[test]
    fn retransmit_accumulation() {
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = TcpInfoSampler::new(&reg, Duration::from_millis(10));

        let now = Instant::now();
        let conns = vec![
            TcpStats { sport: 80, dport: 54000, inode: 1, rtt_us: 1000, total_retrans: 5 },
            TcpStats { sport: 80, dport: 54001, inode: 2, rtt_us: 2000, total_retrans: 0 },
        ];
        s.process(&reg, now, conns);

        // Second sample: inode 1 gained 3 more retransmits
        let now2 = now + Duration::from_millis(10);
        let conns2 = vec![
            TcpStats { sport: 80, dport: 54000, inode: 1, rtt_us: 1100, total_retrans: 8 },
            TcpStats { sport: 80, dport: 54001, inode: 2, rtt_us: 1900, total_retrans: 1 },
        ];
        s.process(&reg, now2, conns2);

        // First sight only baselines (inode 1's pre-existing 5 retransmits are
        // history, not this window's rate); deltas after that count: 3 + 1 = 4.
        let id = s.retrans_id(&reg, 80);
        assert_eq!(reg.raw(id), 4);
    }

    #[test]
    fn inode_zero_sockets_are_skipped() {
        // TIME_WAIT/SYN entries all report inode 0; they must not alias into
        // one shared retransmit-tracking slot or record bogus RTT.
        let reg = Registry::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut s = TcpInfoSampler::new(&reg, Duration::from_millis(10));
        let now = Instant::now();
        s.process(&reg, now, vec![
            TcpStats { sport: 80, dport: 54000, inode: 0, rtt_us: 1000, total_retrans: 5 },
        ]);
        assert!(reg.metric_ids().is_empty(), "inode-0 socket registered metrics");
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "tcpinfo",
    init: |reg, iv| Box::new(TcpInfoSampler::new(reg, iv)),
};
