use std::collections::HashMap;
use std::time::{Duration, Instant};

use nyquist_core::model::{Kind, Labels, MetricId, Unit};
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_core::sampler::{Sampler, SamplerError};

use crate::inet_diag::{query_tcp_connections, TcpStats};

/// Service port heuristic: the non-ephemeral port (< 49152) is the server side.
/// If both are non-ephemeral, use the lower (server ports are typically smaller).
/// Returns None if both are ephemeral (direct peer-to-peer, uncommon).
fn service_port(sport: u16, dport: u16) -> Option<u16> {
    match (sport < 49152, dport < 49152) {
        (true, false)  => Some(sport),
        (false, true)  => Some(dport),
        (true, true)   => Some(sport.min(dport)),
        (false, false) => None,
    }
}

pub struct TcpInfoSampler {
    interval: Duration,
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
            let Some(port) = service_port(c.sport, c.dport) else { continue };
            seen_inodes.insert(c.inode);

            // RTT histogram
            if c.rtt_us > 0 {
                let id = self.rtt_id(reg, port);
                reg.record_gauge(id, now, c.rtt_us as u64);
            }

            // Retransmit delta tracking
            let delta = if let Some(&(prev_port, prev_count)) = self.prev_retrans.get(&c.inode) {
                if prev_port == port && c.total_retrans >= prev_count {
                    c.total_retrans - prev_count
                } else {
                    // connection reused inode or port mismatch: treat as fresh
                    c.total_retrans
                }
            } else {
                // New connection: count its full history on first sight
                c.total_retrans
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
                // EPERM/EACCES: disable permanently rather than spamming errors
                if raw == libc::EPERM || raw == libc::EACCES {
                    tracing::warn!(
                        error = %e,
                        "tcpinfo: INET_DIAG query failed (no permission), disabling sampler"
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
        assert_eq!(service_port(80, 54321), Some(80));    // server at 80
        assert_eq!(service_port(54321, 443), Some(443));  // client to 443
        assert_eq!(service_port(22, 9100), Some(22));     // both registered, lower wins
        assert_eq!(service_port(54321, 60000), None);     // both ephemeral
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

        // Total retransmits for port 80: initial(5+0) + delta(3+1) = 9
        let id = s.retrans_id(&reg, 80);
        assert_eq!(reg.raw(id), 9);
    }
}

#[linkme::distributed_slice(nyquist_core::registration::SAMPLERS)]
static ENTRY: nyquist_core::registration::SamplerEntry = nyquist_core::registration::SamplerEntry {
    name: "tcpinfo",
    init: |reg, iv| Box::new(TcpInfoSampler::new(reg, iv)),
};
