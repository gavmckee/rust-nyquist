//! RPS/XPS steering masks and IRQ CPU affinities for physical interfaces.
//! These are the config surfaces the syswatch fswatch (inotify) path covers:
//! a mistuned rps_cpus mask or an irqbalance daemon silently rewriting
//! affinities changes packet-steering behavior with nothing else visible.

/// Per-interface steering state. Empty for virtual interfaces.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Steering {
    /// (rx queue, normalized rps_cpus hex mask)
    pub rps: Vec<(u32, String)>,
    /// (tx queue, normalized xps_cpus hex mask)
    pub xps: Vec<(u32, String)>,
    /// (irq number, smp_affinity_list, e.g. "0-191")
    pub irq_affinity: Vec<(u32, String)>,
}

pub fn read_for(iface: &str) -> Steering {
    // Physical interfaces only: veth/bridge churn (docker) would add
    // hundreds of meaningless tracked keys and inotify watches.
    if !std::path::Path::new(&format!("/sys/class/net/{iface}/device")).exists() {
        return Steering::default();
    }

    let mut s = Steering::default();
    if let Ok(dir) = std::fs::read_dir(format!("/sys/class/net/{iface}/queues")) {
        for e in dir.flatten() {
            let name = e.file_name().into_string().unwrap_or_default();
            if let Some(q) = name.strip_prefix("rx-").and_then(|n| n.parse().ok()) {
                if let Ok(v) = std::fs::read_to_string(e.path().join("rps_cpus")) {
                    s.rps.push((q, normalize_mask(v.trim())));
                }
            } else if let Some(q) = name.strip_prefix("tx-").and_then(|n| n.parse().ok()) {
                if let Ok(v) = std::fs::read_to_string(e.path().join("xps_cpus")) {
                    s.xps.push((q, normalize_mask(v.trim())));
                }
            }
        }
    }
    if let Ok(dir) = std::fs::read_dir(format!("/sys/class/net/{iface}/device/msi_irqs")) {
        for e in dir.flatten() {
            if let Ok(irq) = e.file_name().into_string().unwrap_or_default().parse::<u32>() {
                if let Ok(v) = std::fs::read_to_string(format!("/proc/irq/{irq}/smp_affinity_list")) {
                    s.irq_affinity.push((irq, v.trim().to_string()));
                }
            }
        }
    }
    // Stable ordering so serialized snapshots and diffs are deterministic.
    s.rps.sort_unstable();
    s.xps.sort_unstable();
    s.irq_affinity.sort_unstable();
    s
}

/// Collapse a kernel cpumask ("00000000,00000000,0000f000") to compact hex
/// ("f000"; all-zeros → "0") so change rows are readable.
pub fn normalize_mask(raw: &str) -> String {
    let hex: String = raw.chars().filter(|c| *c != ',').collect();
    let trimmed = hex.trim_start_matches('0');
    if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_normalize_compactly() {
        assert_eq!(normalize_mask("00000000,00000000"), "0");
        assert_eq!(normalize_mask("00000000,0000f000"), "f000");
        assert_eq!(normalize_mask("ffffffff,ffffffff"), "ffffffffffffffff");
        assert_eq!(normalize_mask("0"), "0");
    }
}
