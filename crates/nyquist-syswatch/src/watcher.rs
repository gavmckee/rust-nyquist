use std::sync::mpsc;
use std::time::Duration;
use nyquist_core::pending::Pending;
use tokio::sync::watch;
use tracing::{info, warn};
use crate::event::{SW_SRC_ETHTOOL, SW_SRC_FSWATCH, SW_SRC_RTNETLINK, SW_SRC_SYSCTL, SwEvent};
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

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut sink = Sink::new(&self.ch_url, &self.ch_db, &self.ch_user, &self.ch_pass);
        if !initialize(&mut sink, &mut shutdown).await { return Ok(()); }
        let mut pending = Pending::new(10_000);
        // Dropping this guard on errors/abort also stops the polling thread.
        let stop = StopPolling(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));

        // Take an initial snapshot so we have old_values for the first diff.
        let initial = tokio::task::spawn_blocking(nyquist_sysconfig::collect).await?;
        let mut snapshot = Snapshot::from_sysconfig(&initial);
        info!("syswatch: baseline snapshot taken ({} interfaces)", initial.interfaces.len());

        // Channel: BPF polling thread + fswatch thread → async event loop.
        let (tx, rx) = mpsc::sync_channel::<SwEvent>(512);

        // fswatch (inotify) covers steering files BPF cannot: RPS/XPS masks
        // and IRQ affinities. Failure is non-fatal — the 60s poll remains.
        if let Err(e) = InotifyWatcher::spawn(tx.clone(), stop.0.clone()) {
            warn!(error = %e, "syswatch: fswatch unavailable — steering changes fall to the 60s poll");
        }

        // Try to load BPF programs. Failure is non-fatal — we still run the
        // 60s polling fallback so config changes are never silently missed.
        #[cfg(target_os = "linux")]
        let bpf_task = match crate::bpf::BpfState::load(tx) {
            Ok((state, report)) => {
                let fmt = |r: &Result<(), String>| match r {
                    Ok(()) => "ok",
                    Err(_) => "failed",
                };
                info!(
                    sysctl    = fmt(&report.sysctl),
                    ethtool   = fmt(&report.ethtool),
                    ethnl     = fmt(&report.ethnl),
                    rtnetlink = fmt(&report.rtnetlink),
                    "syswatch: BPF hooks loaded"
                );
                if let Err(e) = &report.sysctl    { warn!(error = %e, "syswatch: sysctl hook"); }
                if let Err(e) = &report.ethtool   { warn!(error = %e, "syswatch: ethtool hook"); }
                if let Err(e) = &report.ethnl     { warn!(error = %e, "syswatch: ethnl hook"); }
                if let Err(e) = &report.rtnetlink { warn!(error = %e, "syswatch: rtnetlink hook"); }
                let stop_flag = stop.0.clone();
                Some(tokio::task::spawn_blocking(move || poll_until_stopped(stop_flag, || {
                    if let Err(e) = state.poll(Duration::from_millis(200)) {
                        warn!(error = %e, "syswatch: ring buffer poll error");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                })))
            }
            Err(e) => {
                warn!(error = %e, "syswatch: BPF load failed — running poll-only fallback");
                None
            }
        };

        #[cfg(not(target_os = "linux"))]
        let bpf_task: Option<tokio::task::JoinHandle<()>> = None;

        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await; // discard the immediate first tick

        let mut retry = tokio::time::interval(Duration::from_secs(5));
        loop {
            if *shutdown.borrow() { break; }
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = retry.tick() => flush(&mut sink, &mut pending).await,
                _ = refresh.tick() => {
                    // Periodic full re-read: catches anything missed (inotify
                    // paths not yet watched, events during BPF load window).
                    let cfg = tokio::task::spawn_blocking(nyquist_sysconfig::collect).await?;
                    let changes = snapshot.diff_and_update(&cfg, 0, "poll");
                    for change in changes { pending.push((unix_ms(), change)); }
                    flush(&mut sink, &mut pending).await;
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
                                // ethcmd 0 = the netlink path; ifname can be
                                // empty there if the ops_begin cache missed.
                                let scope = if iface.is_empty() {
                                    AttrScope::EthtoolAny
                                } else {
                                    AttrScope::Iface { name: iface }
                                };
                                events.push(EventAttr {
                                    pid:   ev.pid,
                                    comm:  ev.comm_str().to_string(),
                                    scope,
                                });
                            }
                            SW_SRC_FSWATCH => {
                                tracing::debug!("syswatch: steering file write detected");
                                events.push(EventAttr {
                                    pid:   0,
                                    comm:  "fswatch".to_string(),
                                    scope: AttrScope::Steering,
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
                        for change in changes { pending.push((unix_ms(), change)); }
                        flush(&mut sink, &mut pending).await;
                    } else {
                        // No BPF events this tick — yield briefly to avoid busy-looping.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
        stop.0.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(task) = bpf_task { task.await?; }
        flush(&mut sink, &mut pending).await;
        Ok(())
    }
}

async fn initialize(sink: &mut Sink, shutdown: &mut watch::Receiver<bool>) -> bool {
    loop {
        if *shutdown.borrow() { return false; }
        tokio::select! {
            _ = shutdown.changed() => return false,
            result = tokio::time::timeout(Duration::from_secs(3), sink.ensure_tables()) => {
                if matches!(result, Ok(Ok(()))) { return true; }
                warn!(?result, "syswatch: table init failed, retrying");
            }
        }
        tokio::select! {
            _ = shutdown.changed() => return false,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}

async fn flush(sink: &mut Sink, pending: &mut Pending<(i64, Change)>) {
    if pending.entries().is_empty() { return; }
    match tokio::time::timeout(Duration::from_secs(3), sink.insert_changes(pending.entries())).await {
        Ok(Ok(())) => pending.acknowledge(),
        result => warn!(?result, retained = pending.entries().len(), dropped_total = pending.dropped(),
            "syswatch: insert failed; retaining changes for retry"),
    }
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis() as i64
}

struct StopPolling(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for StopPolling {
    fn drop(&mut self) { self.0.store(true, std::sync::atomic::Ordering::Relaxed); }
}

fn poll_until_stopped(stop: std::sync::Arc<std::sync::atomic::AtomicBool>, mut poll: impl FnMut()) {
    while !stop.load(std::sync::atomic::Ordering::Relaxed) { poll(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn mock_clickhouse() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let app = axum::Router::new().route("/", axum::routing::post(move |body: String| {
            let seen = seen.clone();
            async move {
                let mut requests = seen.lock().unwrap();
                requests.push(body);
                if requests.len() == 1 { (axum::http::StatusCode::SERVICE_UNAVAILABLE, "temporary failure") }
                else { (axum::http::StatusCode::OK, "") }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        (url, requests, task)
    }

    #[tokio::test]
    async fn startup_retries_and_creates_database_before_table() {
        let (url, requests, server) = mock_clickhouse().await;
        let mut sink = Sink::new(&url, "test", "default", "");
        let (_tx, mut rx) = watch::channel(false);
        assert!(tokio::time::timeout(Duration::from_secs(10), initialize(&mut sink, &mut rx)).await.unwrap());
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("CREATE DATABASE"));
        assert!(requests[1].contains("CREATE DATABASE"));
        assert!(requests[2].contains("CREATE TABLE"));
        server.abort();
    }

    #[tokio::test]
    async fn failed_insert_retries_original_events_and_timestamps() {
        let (url, requests, server) = mock_clickhouse().await;
        let mut sink = Sink::new(&url, "test", "default", "");
        let mut pending = Pending::new(10);
        pending.push((1234, Change { key: "mtu".into(), old_value: "1500".into(),
            new_value: "9000".into(), pid: 1, comm: "test".into() }));
        flush(&mut sink, &mut pending).await;
        assert_eq!(pending.entries().len(), 1);
        flush(&mut sink, &mut pending).await;
        assert!(pending.entries().is_empty());
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0], requests[1]);
        assert!(requests[1].contains("fromUnixTimestamp64Milli(1234"));
        server.abort();
    }

    #[tokio::test]
    async fn startup_retry_is_cancellable() {
        let mut sink = Sink::new("http://127.0.0.1:1", "test", "default", "");
        let (tx, mut rx) = watch::channel(false);
        let task = tokio::spawn(async move { initialize(&mut sink, &mut rx).await });
        tx.send(true).unwrap();
        assert!(!tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn blocking_poller_exits_when_owner_is_dropped() {
        let stop = StopPolling(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let flag = stop.0.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let mut tx = Some(tx);
            poll_until_stopped(flag, || {
                if let Some(tx) = tx.take() { let _ = tx.send(()); }
                std::thread::sleep(Duration::from_millis(1));
            });
        });
        rx.await.unwrap();
        drop(stop);
        tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
    }
}
