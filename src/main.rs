use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use nyquist_config::Config;
use nyquist_core::registry::Registry;
use nyquist_core::scheduler::{spawn_sampler, spawn_sink};
use nyquist_exposition::{HttpServer, VictoriaMetricsSink};
use nyquist_samplers::inventory::build_enabled;
use nyquist_perf::build_perf_enabled;

#[derive(Parser)]
#[command(name = "nyquist", about = "High-resolution oversampling telemetry agent")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Raise the open-file soft limit to the hard limit early, before any
    // samplers open fds. The perf sampler opens 1 fd per CPU per event
    // (128 CPUs × 11 events = 1408 fds); the default soft limit of 1024
    // is not enough and causes "Too many open files" across all samplers.
    raise_nofile_limit();

    let cli = Cli::parse();
    let config = match &cli.config {
        Some(path) => Config::load(path)?,
        None => Config::default(),
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.general.log.clone().into()),
        )
        .init();

    let reg = Arc::new(Registry::with_min_interval(
        std::time::Duration::from_millis(100),
        config.general.window,
        config.min_sampler_interval(),
    ));

    let cfg = config.clone();
    let samplers = build_enabled(
        &reg,
        config.general.default_interval,
        |name| cfg.sampler(name).enabled,
        |name| cfg.sampler(name).interval,
    );

    // Sinks watch this channel: on shutdown they run one final export+flush
    // (the Parquet recorder would otherwise lose its buffered rows).
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let mut sampler_handles = Vec::new();
    let mut sink_handles = Vec::new();
    for s in samplers {
        sampler_handles.push(spawn_sampler(s, reg.clone(), config.general.fault_tolerant));
    }

    let perf_cfg = nyquist_perf::PerfConfig {
        enabled:    config.perf.enabled,
        max_cpus:   config.perf.max_cpus,
        hw_enabled: config.perf.hw_enabled,
        sw_enabled: config.perf.sw_enabled,
    };
    let perf_iv_cfg = config.clone();
    for s in build_perf_enabled(
        &reg,
        config.general.default_interval,
        &perf_cfg,
        |name| perf_iv_cfg.sampler(name).interval,
    ) {
        sampler_handles.push(spawn_sampler(s, reg.clone(), config.general.fault_tolerant));
    }

    if config.recorder.enabled {
        let sink = nyquist_recorder::sink::RecorderSink::new(
            PathBuf::from(&config.recorder.output_dir),
            config.recorder.rotation_interval,
        )?;
        sink_handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            config.recorder.flush_interval,
            config.general.fault_tolerant,
            shutdown_rx.clone(),
        ));
        tracing::info!(
            output_dir = %config.recorder.output_dir,
            "Parquet recorder enabled"
        );
    }

    if config.clickhouse.enabled {
        let ch = &config.clickhouse;
        let sink = nyquist_clickhouse::sink::ClickHouseSink::new(
            &ch.url,
            &ch.database,
            &ch.username,
            &ch.password,
        );
        sink_handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            ch.insert_interval,
            config.general.fault_tolerant,
            shutdown_rx.clone(),
        ));
        // Polling config watcher: only runs when syswatch BPF watcher is disabled.
        // syswatch supersedes it with event-driven detection; running both would
        // produce duplicate rows in sysconfig_changes.
        if !config.syswatch.enabled {
            let watcher = nyquist_clickhouse::config_watcher::ConfigWatcher::new(
                &ch.url,
                &ch.database,
                &ch.username,
                &ch.password,
                std::time::Duration::from_secs(5),
            );
            let shutdown = shutdown_rx.clone();
            sink_handles.push(tokio::spawn(async move { watcher.run(shutdown).await }));
        }
        tracing::info!(url = %ch.url, database = %ch.database, "ClickHouse sink enabled");
    }

    if config.syswatch.enabled {
        if config.clickhouse.enabled {
            let ch = &config.clickhouse;
            let watcher = nyquist_syswatch::SysWatcher::new(
                &ch.url, &ch.database, &ch.username, &ch.password,
            );
            let shutdown = shutdown_rx.clone();
            sink_handles.push(tokio::spawn(async move {
                if let Err(e) = watcher.run(shutdown).await {
                    tracing::error!(error = %e, "syswatch exited");
                }
            }));
            tracing::info!("syswatch enabled (BPF hooks + 60s poll fallback)");
        } else {
            tracing::warn!("syswatch requires [clickhouse] enabled = true");
        }
    }

    tracing::info!(registered = nyquist_ebpf::registered(), "samplers registered (incl. BPF)");

    if config.victoria_metrics.enabled {
        let sink = VictoriaMetricsSink::new(&config.victoria_metrics.url, config.general.percentiles.clone());
        sink_handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            config.victoria_metrics.push_interval,
            config.general.fault_tolerant,
            shutdown_rx.clone(),
        ));
        tracing::info!(url = %config.victoria_metrics.url, "VictoriaMetrics push enabled");
    }

    // Supervise all worker tasks: with fault_tolerant=true they never exit, so
    // any completion before shutdown is a panic or a fault_tolerant=false
    // death — previously swallowed while the agent kept serving frozen
    // metrics. Sampler abort handles are kept so shutdown can stop them.
    let sampler_aborts: Vec<_> = sampler_handles.iter().map(|h| h.abort_handle()).collect();
    let mut tasks: tokio::task::JoinSet<Result<(), tokio::task::JoinError>> =
        tokio::task::JoinSet::new();
    for h in sampler_handles { tasks.spawn(h); }
    for h in sink_handles { tasks.spawn(h); }

    let server = HttpServer::new(reg.clone(), config.general.percentiles.clone());
    let serve_fut = server.serve(&config.general.listen);
    tokio::pin!(serve_fut);

    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    let fatal: Option<anyhow::Error> = tokio::select! {
        res = &mut serve_fut => Some(match res {
            Ok(()) => anyhow::anyhow!("http server exited unexpectedly"),
            Err(e) => anyhow::anyhow!("http server failed: {e}"),
        }),
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT received, shutting down");
            None
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down");
            None
        }
        Some(res) = tasks.join_next() => Some(anyhow::anyhow!(
            "worker task exited unexpectedly (panic or fault_tolerant=false): {res:?}"
        )),
    };

    // Graceful shutdown: stop samplers, then let every sink run its final
    // export + flush (bounded — a hung sink must not block termination).
    for a in &sampler_aborts { a.abort(); }
    let _ = shutdown_tx.send(true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, tasks.join_next()).await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => {
                tracing::warn!("shutdown timed out; some sink flushes may be incomplete");
                break;
            }
        }
    }
    tracing::info!("shutdown complete");
    match fatal {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn raise_nofile_limit() {
    use std::mem::MaybeUninit;
    unsafe {
        let mut rlim = MaybeUninit::<libc::rlimit>::uninit();
        if libc::getrlimit(libc::RLIMIT_NOFILE, rlim.as_mut_ptr()) != 0 {
            return;
        }
        let mut rlim = rlim.assume_init();
        let target = if rlim.rlim_max == libc::RLIM_INFINITY { 65536 } else { rlim.rlim_max };
        if rlim.rlim_cur < target {
            rlim.rlim_cur = target;
            libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
        }
    }
}
