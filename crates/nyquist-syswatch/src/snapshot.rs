use std::collections::HashMap;
use nyquist_sysconfig::SysConfig;

pub struct Change {
    pub key:       String,
    pub old_value: String,
    pub new_value: String,
    pub pid:       u32,
    pub comm:      String,
}

/// One drained BPF event, reduced to what attribution needs.
pub struct EventAttr {
    pub pid:   u32,
    pub comm:  String,
    pub scope: AttrScope,
}

/// What part of the config namespace an event can legitimately claim.
/// Attribution is per-key: stamping a whole diff batch with the last drained
/// event let an unrelated rtnetlink event (e.g. lldpd touching link state in
/// the same 50 ms drain window) claim a sysctl write.
pub enum AttrScope {
    /// A /proc/sys write; `leaf` is the written file's name (e.g. "tcp_rmem").
    /// Claims only `sysctl.*` keys derived from that leaf.
    Sysctl { leaf: String },
    /// An ethtool SET on a specific interface. Claims any key whose interface
    /// segment matches (ring.*, channels.*, coalesce.*, rss.*, mtu.*, msix.*).
    Iface { name: String },
    /// An ethtool-netlink SET whose interface could not be recovered (the
    /// ops_begin cache missed). Claims ethtool-domain keys on ANY interface —
    /// still far better than "unknown", and two concurrent ethtool writers in
    /// one drain window is rare.
    EthtoolAny,
    /// An rtnetlink link change (iface unknown from the event). Claims only
    /// link-level keys (mtu.*), never sysctl.* keys.
    Link,
    /// An inotify (fswatch) hit on a steering file. inotify carries no
    /// writer identity, so these claim only steering keys (rps./xps./irq.)
    /// and surface as comm="fswatch" — sub-second detection, no attribution.
    Steering,
}

/// Find the event that can claim `key`, most specific scope first.
fn resolve<'a>(key: &str, events: &'a [EventAttr]) -> Option<&'a EventAttr> {
    if let Some(rest) = key.strip_prefix("sysctl.") {
        // sysctl keys can only ever be claimed by sysctl events. Flattened
        // names either derive from the leaf (tcp_rmem → sysctl.tcp_rmem_max)
        // or nest it (conf.<iface>.rp_filter ends with the written leaf).
        return events.iter().find(|e| {
            matches!(&e.scope, AttrScope::Sysctl { leaf }
                if !leaf.is_empty()
                    && (rest.starts_with(leaf.as_str())
                        || rest.ends_with(&format!(".{leaf}"))))
        });
    }
    // Interface-scoped keys: "<class>.<iface>" or "<class>.<iface>.<field>".
    let iface_of_key = key.split('.').nth(1);
    let ethtool_domain = ["ring.", "channels.", "coalesce.", "rss.", "msix."]
        .iter()
        .any(|p| key.starts_with(p));
    events
        .iter()
        .find(|e| {
            matches!(&e.scope, AttrScope::Iface { name }
                if Some(name.as_str()) == iface_of_key)
        })
        .or_else(|| {
            events.iter().find(|e| {
                matches!(e.scope, AttrScope::EthtoolAny) && ethtool_domain
            })
        })
        .or_else(|| {
            events.iter().find(|e| {
                matches!(e.scope, AttrScope::Link) && key.starts_with("mtu.")
            })
        })
        .or_else(|| {
            let steering_key = ["rps.", "xps.", "irq."].iter().any(|p| key.starts_with(p));
            events.iter().find(|e| {
                matches!(e.scope, AttrScope::Steering) && steering_key
            })
        })
}

/// Current known configuration state. Updated on every BPF/inotify event.
pub struct Snapshot {
    state: HashMap<String, String>,
}

impl Snapshot {
    /// Populate from a freshly collected SysConfig baseline.
    pub fn from_sysconfig(cfg: &SysConfig) -> Self {
        let mut state = HashMap::new();
        flatten_into(cfg, &mut state);
        Snapshot { state }
    }

    /// Merge a new SysConfig reading, returning only keys whose values changed.
    /// Every change is uniformly stamped with `pid`/`comm` — used by the
    /// polling path where there is no per-key trigger information.
    pub fn diff_and_update(
        &mut self,
        cfg:  &SysConfig,
        pid:  u32,
        comm: &str,
    ) -> Vec<Change> {
        self.diff_raw(cfg)
            .into_iter()
            .map(|(key, old_value, new_value)| Change {
                key, old_value, new_value,
                pid,
                comm: comm.to_string(),
            })
            .collect()
    }

    /// Like `diff_and_update`, but each changed key is attributed to the
    /// drained BPF event whose scope claims it. Keys no event can claim get
    /// pid 0 / "unknown" — an honest gap beats a confident misattribution.
    pub fn diff_and_update_attributed(
        &mut self,
        cfg:    &SysConfig,
        events: &[EventAttr],
    ) -> Vec<Change> {
        self.diff_raw(cfg)
            .into_iter()
            .map(|(key, old_value, new_value)| {
                let (pid, comm) = match resolve(&key, events) {
                    Some(e) => (e.pid, e.comm.clone()),
                    None => (0, "unknown".to_string()),
                };
                Change { key, old_value, new_value, pid, comm }
            })
            .collect()
    }

    fn diff_raw(&mut self, cfg: &SysConfig) -> Vec<(String, String, String)> {
        let mut fresh: HashMap<String, String> = HashMap::new();
        flatten_into(cfg, &mut fresh);

        let mut changes = Vec::new();
        for (k, new_v) in &fresh {
            match self.state.get(k) {
                Some(old_v) if old_v == new_v => {}
                Some(old_v) => changes.push((k.clone(), old_v.clone(), new_v.clone())),
                // First time seeing this key (new interface appeared) — no change record.
                None => {}
            }
        }
        // Absorb the new state.
        self.state = fresh;
        changes
    }
}

/// Flatten a SysConfig into (key, string_value) pairs using the same
/// key namespace as nyquist_clickhouse::config_watcher for compatibility.
fn flatten_into(cfg: &SysConfig, out: &mut HashMap<String, String>) {
    let s = &cfg.sysctl;
    out.insert("sysctl.tcp_rmem_max".into(),              s.tcp_rmem[2].to_string());
    out.insert("sysctl.tcp_wmem_max".into(),              s.tcp_wmem[2].to_string());
    out.insert("sysctl.rmem_max".into(),                  s.rmem_max.to_string());
    out.insert("sysctl.wmem_max".into(),                  s.wmem_max.to_string());
    out.insert("sysctl.netdev_max_backlog".into(),        s.netdev_max_backlog.to_string());
    out.insert("sysctl.tcp_slow_start_after_idle".into(), s.tcp_slow_start_after_idle.to_string());
    out.insert("sysctl.tcp_congestion_control".into(),    s.tcp_congestion_control.clone());

    for (iface, info) in &cfg.interfaces {
        out.insert(format!("ring.{iface}.rx"),           info.ring_rx.to_string());
        out.insert(format!("ring.{iface}.tx"),           info.ring_tx.to_string());
        out.insert(format!("ring.{iface}.rx_max"),       info.ring_rx_max.to_string());
        out.insert(format!("mtu.{iface}"),               info.mtu.to_string());
        out.insert(format!("channels.{iface}.rx"),       info.rx_queues.to_string());
        out.insert(format!("channels.{iface}.tx"),       info.tx_queues.to_string());
        out.insert(format!("channels.{iface}.combined"), info.combined_queues.to_string());
        out.insert(format!("rss.{iface}.table_size"),    info.rss_table_size.to_string());
        out.insert(format!("msix.{iface}.vectors"),      info.msix_vectors.to_string());
        out.insert(format!("coalesce.{iface}.rx_usecs"), info.coalesce_rx_usecs.to_string());
        out.insert(format!("coalesce.{iface}.tx_usecs"), info.coalesce_tx_usecs.to_string());
        out.insert(format!("sysctl.conf.{iface}.rp_filter"), info.rp_filter.to_string());
        for (q, mask) in &info.steering.rps {
            out.insert(format!("rps.{iface}.rx-{q}"), mask.clone());
        }
        for (q, mask) in &info.steering.xps {
            out.insert(format!("xps.{iface}.tx-{q}"), mask.clone());
        }
        for (irq, aff) in &info.steering.irq_affinity {
            out.insert(format!("irq.{iface}.{irq}"), aff.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(pid: u32, comm: &str, scope: AttrScope) -> EventAttr {
        EventAttr { pid, comm: comm.to_string(), scope }
    }

    #[test]
    fn sysctl_key_claimed_only_by_matching_sysctl_event() {
        // Regression for the observed quirk: an lldpd rtnetlink event in the
        // same drain window must never claim a sysctl change.
        let events = vec![
            ev(200, "lldpd", AttrScope::Link),
            ev(100, "sysctl", AttrScope::Sysctl { leaf: "tcp_rmem".into() }),
        ];
        let hit = resolve("sysctl.tcp_rmem_max", &events).unwrap();
        assert_eq!(hit.comm, "sysctl");
        // A sysctl event for a different leaf doesn't claim it either.
        let other = vec![ev(1, "x", AttrScope::Sysctl { leaf: "rmem_max".into() })];
        assert!(resolve("sysctl.tcp_rmem_max", &other).is_none());
        // rmem_max leaf claims sysctl.rmem_max but not sysctl.tcp_rmem_max.
        assert!(resolve("sysctl.rmem_max", &other).is_some());
    }

    #[test]
    fn iface_keys_prefer_ethtool_then_link_events() {
        let events = vec![
            ev(300, "lldpd", AttrScope::Link),
            ev(400, "ethtool", AttrScope::Iface { name: "ens1f0np0".into() }),
        ];
        // ethtool event claims its interface's ring key.
        assert_eq!(resolve("ring.ens1f0np0.rx", &events).unwrap().comm, "ethtool");
        // A different interface's key falls through to the Link event only
        // for link-level (mtu.*) keys...
        assert_eq!(resolve("mtu.ens1f1np1", &events).unwrap().comm, "lldpd");
        // ...but not for ethtool-domain keys like channels.
        assert!(resolve("channels.ens1f1np1.rx", &events).is_none());
    }

    #[test]
    fn ethnl_event_without_iface_claims_ethtool_domain_only() {
        let events = vec![ev(500, "ethtool", AttrScope::EthtoolAny)];
        // Claims ethtool-domain keys on any interface...
        assert_eq!(resolve("ring.ens1f0np0.rx", &events).unwrap().comm, "ethtool");
        assert_eq!(resolve("channels.eth9.combined", &events).unwrap().comm, "ethtool");
        // ...but never sysctl or link-level keys.
        assert!(resolve("sysctl.tcp_rmem_max", &events).is_none());
        assert!(resolve("mtu.eth0", &events).is_none());
        // A specific Iface event beats EthtoolAny for its own interface.
        let both = vec![
            ev(1, "generic", AttrScope::EthtoolAny),
            ev(2, "specific", AttrScope::Iface { name: "eth0".into() }),
        ];
        assert_eq!(resolve("ring.eth0.rx", &both).unwrap().comm, "specific");
    }

    #[test]
    fn steering_events_claim_only_steering_keys() {
        let events = vec![ev(9, "fswatch", AttrScope::Steering)];
        assert_eq!(resolve("rps.ens1f0np0.rx-3", &events).unwrap().comm, "fswatch");
        assert_eq!(resolve("xps.ens1f0np0.tx-0", &events).unwrap().comm, "fswatch");
        assert_eq!(resolve("irq.ens1f0np0.211", &events).unwrap().comm, "fswatch");
        assert!(resolve("ring.ens1f0np0.rx", &events).is_none());
        assert!(resolve("sysctl.rmem_max", &events).is_none());
    }

    #[test]
    fn unclaimed_keys_resolve_to_none() {
        assert!(resolve("sysctl.tcp_congestion_control", &[]).is_none());
        let only_sysctl = vec![ev(1, "sysctl", AttrScope::Sysctl { leaf: "rp_filter".into() })];
        assert!(resolve("ring.eth0.rx", &only_sysctl).is_none());
    }
}
