use std::sync::mpsc;
use std::time::Duration;
use anyhow::Context;
use tracing::{info, warn};
use crate::event::{SW_SRC_ETHTOOL, SW_SRC_RTNETLINK, SW_SRC_SYSCTL, SwEvent};
use crate::inotify::InotifyWatcher;
use crate::sink::Sink;
use crate::snapshot::{AttrScope, Change, EventAttr, Snapshot};

pub struct SysWatcher {
    ch_url:  String,
    ch_db:   String,
    ch_user: String,
    ch_pass: String,
}

impl SysWatcher {
    pub fn new(url: &str, db: &str, user: &str, pass: &str) -> Self {
        SysWatcher {
            ch_url:  url.to_string(),
            ch_db:   db.to_string(),
            ch_user: user.to_string(),
            ch_pass: pass.to_string(),
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let mut sink = Sink::new(&self.ch_url, &self.ch_db, &self.ch_user, &self.ch_pass);
        sink.ensure_tables().await.context("syswatch: ClickHouse table init")?;

        // Take an initial snapshot so we have old_values for the first diff.
        let initial = tokio::task::spawn_blocking(nyquist_sysconfig::collect).await?;
        let mut snapshot = Snapshot::from_sysconfig(&initial);
        info!("syswatch: baseline snapshot taken ({} interfaces)", initial.interfaces.len());

        // Channel: BPF polling thread → async event loop.
        let (tx, rx) = mpsc::sync_channel::<SwEvent>(512);

        // Try to load BPF programs. Failure is non-fatal — we still run the
        // 60s polling fallback so config changes are never silently missed.
        #[cfg(target_os = "linux")]
        let _bpf_active = match crate::bpf::BpfState::load(tx) {
            Ok((state, report)) => {
                let fmt = |r: &Result<(), String>| match r {
                    Ok(()) => "ok",
                    Err(_) => "failed",
                };
                info!(
                    sysctl    = fmt(&report.sysctl),
                    ethtool   = fmt(&report.ethtool),
                    rtnetlink = fmt(&report.rtnetlink),
                    "syswatch: BPF hooks loaded"
                );
                if let Err(e) = &report.sysctl    { warn!(error = %e, "syswatch: sysctl hook"); }
                if let Err(e) = &report.ethtool   { warn!(error = %e, "syswatch: ethtool hook"); }
                if let Err(e) = &report.rtnetlink { warn!(error = %e, "syswatch: rtnetlink hook"); }
                tokio::task::spawn_blocking(move || loop {
                    if let Err(e) = state.poll(Duration::from_millis(200)) {
                        warn!(error = %e, "syswatch: ring buffer poll error");
                    }
                });
                true
            }
            Err(e) => {
                warn!(error = %e, "syswatch: BPF load failed — running poll-only fallback");
                false
            }
        };

        #[cfg(not(target_os = "linux"))]
        let bpf_active = false;

        let _inotify = InotifyWatcher::new()?;

        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await; // discard the immediate first tick

        loop {
            tokio::select! {
                _ = refresh.tick() => {
                    // Periodic full re-read: catches anything missed (inotify
                    // paths not yet watched, events during BPF load window).
                    let cfg = tokio::task::spawn_blocking(nyquist_sysconfig::collect).await?;
                    let changes = snapshot.diff_and_update(&cfg, 0, "poll");
                    flush(&mut sink, &changes).await;
                }

                // Drain BPF events without blocking the async executor.
                // try_recv is non-blocking; the select arm completes immediately.
                _ = tokio::task::yield_now() => {
                    // Each event is kept with its own scope so the diff can
                    // attribute per key. Stamping the whole batch with the
                    // last-drained event let an unrelated rtnetlink event
                    // (e.g. lldpd) claim a sysctl write landing in the same
                    // drain window.
                    let mut events: Vec<EventAttr> = Vec::new();

                    while let Ok(ev) = rx.try_recv() {
                        match ev.src {
                            SW_SRC_SYSCTL => {
                                let key = ev.key_str();
                                // Convert /proc/sys/net/ipv4/tcp_rmem → full sysconfig refresh.
                                // A targeted single-key read would be faster but nyquist_sysconfig
                                // doesn't expose per-key reads yet; full collect() is fine given
                                // the low frequency of sysctl changes.
                                let leaf = key.rsplit('/').next().unwrap_or("").to_string();
                                tracing::debug!(key, pid = ev.pid, comm = ev.comm_str(), "syswatch: sysctl write detected");
                                events.push(EventAttr {
                                    pid:   ev.pid,
                                    comm:  ev.comm_str().to_string(),
                                    scope: AttrScope::Sysctl { leaf },
                                });
                            }
                            SW_SRC_ETHTOOL => {
                                let iface = ev.ifname_str().to_string();
                                tracing::debug!(iface, ethcmd = ev.ethcmd, comm = ev.comm_str(), "syswatch: ethtool SET detected");
                                events.push(EventAttr {
                                    pid:   ev.pid,
                                    comm:  ev.comm_str().to_string(),
                                    scope: AttrScope::Iface { name: iface },
                                });
                            }
                            SW_SRC_RTNETLINK => {
                                tracing::debug!(nlmsg_type = ev.nlmsg_type, comm = ev.comm_str(), "syswatch: rtnetlink change detected");
                                events.push(EventAttr {
                                    pid:   ev.pid,
                                    comm:  ev.comm_str().to_string(),
                                    scope: AttrScope::Link,
                                });
                            }
                            _ => {}
                        }
                    }

                    if !events.is_empty() {
                        let cfg = tokio::task::spawn_blocking(nyquist_sysconfig::collect).await?;
                        let changes = snapshot.diff_and_update_attributed(&cfg, &events);
                        if !changes.is_empty() {
                            info!(
                                count = changes.len(),
                                keys  = %changes.iter().map(|c| c.key.as_str()).collect::<Vec<_>>().join(", "),
                                "syswatch: config change detected"
                            );
                        }
                        flush(&mut sink, &changes).await;
                    } else {
                        // No BPF events this tick — yield briefly to avoid busy-looping.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
}

async fn flush(sink: &mut Sink, changes: &[Change]) {
    if changes.is_empty() { return; }
    if let Err(e) = sink.insert_changes(changes).await {
        warn!(error = %e, count = changes.len(), "syswatch: failed to insert changes");
    }
}
