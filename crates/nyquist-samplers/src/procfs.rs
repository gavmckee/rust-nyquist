use std::collections::HashMap;

pub struct NetDevEntry {
    pub iface:      String,
    pub rx_bytes:   u64,
    pub rx_errors:  u64,
    pub rx_dropped: u64,
    pub tx_bytes:   u64,
    pub tx_errors:  u64,
    pub tx_dropped: u64,
}

pub struct PsiSnapshot {
    pub some_avg10: u64,
    pub some_avg60: u64,
    pub some_avg300: u64,
    pub full_avg10: u64,
    pub full_avg60: u64,
    pub full_avg300: u64,
    pub has_full: bool,
}

pub struct SockstatSnapshot {
    pub tcp_inuse: u64,
    pub tcp_tw: u64,
    pub tcp_orphan: u64,
    pub udp_inuse: u64,
}

pub fn parse_proc_stat(text: &str) -> Vec<(String, [u64; 7])> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.starts_with("cpu") { continue; }
        let mut it = line.split_whitespace();
        let label = match it.next() { Some(l) => l.to_string(), None => continue };
        let vals: Vec<u64> = it.take(7).filter_map(|v| v.parse().ok()).collect();
        if vals.len() == 7 {
            let mut arr = [0u64; 7];
            arr.copy_from_slice(&vals);
            out.push((label, arr));
        }
    }
    out
}

pub fn parse_meminfo(text: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split(':');
        let key = match it.next() { Some(k) => k.trim().to_string(), None => continue };
        let rest = match it.next() { Some(r) => r.trim(), None => continue };
        let mut parts = rest.split_whitespace();
        if let Some(num) = parts.next().and_then(|n| n.parse::<u64>().ok()) {
            let bytes = if rest.ends_with("kB") { num * 1024 } else { num };
            out.push((key, bytes));
        }
    }
    out
}

pub fn parse_net_dev(text: &str) -> Vec<NetDevEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(colon) = line.find(':') else { continue };
        let iface = line[..colon].trim().to_string();
        if iface.is_empty() || iface.contains('|') { continue; }
        let nums: Vec<u64> = line[colon + 1..].split_whitespace()
            .filter_map(|n| n.parse().ok()).collect();
        if nums.len() >= 12 {
            out.push(NetDevEntry {
                iface,
                rx_bytes:   nums[0],
                rx_errors:  nums[2],
                rx_dropped: nums[3],
                tx_bytes:   nums[8],
                tx_errors:  nums[10],
                tx_dropped: nums[11],
            });
        }
    }
    out
}

/// Parse `/proc/softirqs`. Returns map of softirq type → total count across all CPUs.
pub fn parse_softirqs(text: &str) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("CPU") { continue; }
        let Some(colon) = trimmed.find(':') else { continue };
        let name = trimmed[..colon].trim().to_string();
        let total: u64 = trimmed[colon + 1..].split_whitespace()
            .filter_map(|n| n.parse::<u64>().ok()).sum();
        if !name.is_empty() { out.insert(name, total); }
    }
    out
}

pub fn parse_diskstats(text: &str) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 { continue; }
        let device = f[2].to_string();
        let nums: Vec<u64> = f[3..].iter().filter_map(|n| n.parse().ok()).collect();
        if nums.len() >= 7 {
            out.push((device, nums[2], nums[6]));
        }
    }
    out
}

pub fn parse_net_snmp(text: &str) -> HashMap<String, HashMap<String, u64>> {
    let mut out: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let Some(colon) = line.find(':') else { continue };
        let proto = line[..colon].trim().to_string();
        let rest: Vec<&str> = line[colon + 1..].split_whitespace().collect();
        let is_data = rest.first().map(|t| t.parse::<i64>().is_ok()).unwrap_or(false);
        if is_data {
            if let Some(names) = headers.get(&proto) {
                let mut fields = HashMap::new();
                for (name, val) in names.iter().zip(rest.iter()) {
                    if let Ok(v) = val.parse::<u64>() {
                        fields.insert(name.clone(), v);
                    }
                }
                out.insert(proto, fields);
            }
        } else {
            headers.insert(proto, rest.iter().map(|s| s.to_string()).collect());
        }
    }
    out
}

/// Parse `/proc/loadavg`. Returns (load1, load5, load15) scaled ×100 as u64.
/// A load of 0.15 is returned as 15.
pub fn parse_loadavg(text: &str) -> Option<(u64, u64, u64)> {
    let mut it = text.split_whitespace();
    let l1  = it.next()?.parse::<f64>().ok()?;
    let l5  = it.next()?.parse::<f64>().ok()?;
    let l15 = it.next()?.parse::<f64>().ok()?;
    Some(((l1 * 100.0).round() as u64, (l5 * 100.0).round() as u64, (l15 * 100.0).round() as u64))
}

/// Parse a single PSI pressure file (e.g. `/proc/pressure/cpu`).
/// Float values are scaled ×100 as u64. `has_full` is true when a "full" line is present.
pub fn parse_psi(text: &str) -> PsiSnapshot {
    let mut snap = PsiSnapshot {
        some_avg10: 0, some_avg60: 0, some_avg300: 0,
        full_avg10: 0, full_avg60: 0, full_avg300: 0,
        has_full: false,
    };
    for line in text.lines() {
        let (is_some, is_full) = (line.starts_with("some"), line.starts_with("full"));
        if !is_some && !is_full { continue; }
        let mut a10 = 0u64; let mut a60 = 0u64; let mut a300 = 0u64;
        for field in line.split_whitespace() {
            let Some((k, v)) = field.split_once('=') else { continue };
            let val = (v.parse::<f64>().unwrap_or(0.0) * 100.0).round() as u64;
            match k { "avg10" => a10 = val, "avg60" => a60 = val, "avg300" => a300 = val, _ => {} }
        }
        if is_some { snap.some_avg10 = a10; snap.some_avg60 = a60; snap.some_avg300 = a300; }
        if is_full { snap.full_avg10 = a10; snap.full_avg60 = a60; snap.full_avg300 = a300; snap.has_full = true; }
    }
    snap
}

/// Parse `/proc/net/sockstat` for TCP and UDP socket counts.
pub fn parse_sockstat(text: &str) -> SockstatSnapshot {
    let mut snap = SockstatSnapshot { tcp_inuse: 0, tcp_tw: 0, tcp_orphan: 0, udp_inuse: 0 };
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.first().copied() {
            Some("TCP:") => {
                let mut it = fields[1..].chunks(2);
                while let Some(pair) = it.next() {
                    if pair.len() < 2 { break; }
                    let val: u64 = pair[1].parse().unwrap_or(0);
                    match pair[0] {
                        "inuse"  => snap.tcp_inuse  = val,
                        "tw"     => snap.tcp_tw     = val,
                        "orphan" => snap.tcp_orphan = val,
                        _ => {}
                    }
                }
            }
            Some("UDP:") => {
                let mut it = fields[1..].chunks(2);
                while let Some(pair) = it.next() {
                    if pair.len() < 2 { break; }
                    if pair[0] == "inuse" { snap.udp_inuse = pair[1].parse().unwrap_or(0); }
                }
            }
            _ => {}
        }
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aggregate_and_per_cpu_lines() {
        let text = include_str!("../tests/fixtures/proc_stat");
        let parsed = parse_proc_stat(text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "cpu");
        assert_eq!(parsed[0].1[0], 100);
        assert_eq!(parsed[1].0, "cpu0");
        assert_eq!(parsed[1].1[2], 30);
    }

    #[test]
    fn parses_meminfo_kb_to_bytes() {
        let text = include_str!("../tests/fixtures/proc_meminfo");
        let parsed = parse_meminfo(text);
        let free = parsed.iter().find(|(k, _)| k == "MemFree").unwrap().1;
        assert_eq!(free, 8192000 * 1024);
    }

    #[test]
    fn parses_rx_tx_bytes_per_iface() {
        let text = include_str!("../tests/fixtures/proc_net_dev");
        let parsed = parse_net_dev(text);
        assert_eq!(parsed.len(), 2);
        let eth0 = parsed.iter().find(|e| e.iface == "eth0").unwrap();
        assert_eq!(eth0.rx_bytes, 1000000);
        assert_eq!(eth0.tx_bytes, 2000000);
        assert_eq!(eth0.rx_errors, 0);
        assert_eq!(eth0.rx_dropped, 0);
    }

    #[test]
    fn parses_sectors_read_written_per_device() {
        let text = include_str!("../tests/fixtures/proc_diskstats");
        let parsed = parse_diskstats(text);
        assert_eq!(parsed.len(), 2);
        let sda = parsed.iter().find(|(d, _, _)| d == "sda").unwrap();
        assert_eq!((sda.1, sda.2), (8000, 16000));
    }

    #[test]
    fn parses_tcp_and_udp_fields() {
        let text = include_str!("../tests/fixtures/proc_net_snmp");
        let parsed = parse_net_snmp(text);
        assert_eq!(parsed["Tcp"]["ActiveOpens"], 100);
        assert_eq!(parsed["Tcp"]["InSegs"], 12345);
        assert_eq!(parsed["Udp"]["InDatagrams"], 1000);
        assert_eq!(parsed["Udp"]["NoPorts"], 5);
    }

    #[test]
    fn parses_loadavg_scaled_x100() {
        let text = include_str!("../tests/fixtures/proc_loadavg");
        let (l1, l5, l15) = parse_loadavg(text).unwrap();
        assert_eq!(l1,  15);
        assert_eq!(l5,  45);
        assert_eq!(l15, 120);
    }

    #[test]
    fn parses_psi_some_only() {
        let text = include_str!("../tests/fixtures/proc_pressure_cpu");
        let snap = parse_psi(text);
        assert_eq!(snap.some_avg10, 12);
        assert_eq!(snap.some_avg60, 34);
        assert_eq!(snap.some_avg300, 56);
        assert!(!snap.has_full);
    }

    #[test]
    fn parses_psi_some_and_full() {
        let text = include_str!("../tests/fixtures/proc_pressure_memory");
        let snap = parse_psi(text);
        assert_eq!(snap.some_avg60, 12);
        assert!(snap.has_full);
        assert_eq!(snap.full_avg60, 5);
    }

    #[test]
    fn parses_sockstat_tcp_udp() {
        let text = include_str!("../tests/fixtures/proc_sockstat");
        let snap = parse_sockstat(text);
        assert_eq!(snap.tcp_inuse,  12);
        assert_eq!(snap.tcp_tw,     5);
        assert_eq!(snap.tcp_orphan, 0);
        assert_eq!(snap.udp_inuse,  3);
    }

    #[test]
    fn parses_softirqs_sums_across_cpus() {
        let text = include_str!("../tests/fixtures/proc_softirqs");
        let parsed = parse_softirqs(text);
        assert_eq!(parsed["NET_RX"], 15_000_000);
        assert_eq!(parsed["NET_TX"], 30_000);
        assert!(parsed.contains_key("TIMER"));
        assert!(parsed.contains_key("SCHED"));
    }
}
