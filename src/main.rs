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

    let reg = Arc::new(Registry::new(
        std::time::Duration::from_millis(100),
        config.general.window,
    ));

    let cfg = config.clone();
    let samplers = build_enabled(
        &reg,
        config.general.default_interval,
        |name| cfg.sampler(name).enabled,
        |name| cfg.sampler(name).interval,
    );

    let mut handles = Vec::new();
    for s in samplers {
        handles.push(spawn_sampler(s, reg.clone(), config.general.fault_tolerant));
    }

    let perf_cfg = nyquist_perf::PerfConfig {
        enabled:    config.perf.enabled,
        max_cpus:   config.perf.max_cpus,
        hw_enabled: config.perf.hw_enabled,
        sw_enabled: config.perf.sw_enabled,
    };
    for s in build_perf_enabled(&reg, config.general.default_interval, &perf_cfg) {
        handles.push(spawn_sampler(s, reg.clone(), config.general.fault_tolerant));
    }

    if config.recorder.enabled {
        let sink = nyquist_recorder::sink::RecorderSink::new(
            PathBuf::from(&config.recorder.output_dir),
            config.recorder.rotation_interval,
            config.recorder.flush_interval,
        )?;
        handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            config.recorder.flush_interval,
            config.general.fault_tolerant,
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
            ch.insert_interval,
        );
        handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            ch.insert_interval,
            config.general.fault_tolerant,
        ));
        // Config watcher: polls sysconfig every 5s, writes sysconfig_values for
        // plotting and sysconfig_changes as Grafana annotation events.
        let watcher = nyquist_clickhouse::config_watcher::ConfigWatcher::new(
            &ch.url,
            &ch.database,
            &ch.username,
            &ch.password,
            std::time::Duration::from_secs(5),
        );
        tokio::spawn(async move { watcher.run().await });
        tracing::info!(url = %ch.url, database = %ch.database, "ClickHouse sink + config watcher enabled");
    }

    tracing::info!(registered = nyquist_ebpf::registered(), "samplers registered (incl. BPF)");

    if config.victoria_metrics.enabled {
        let sink = VictoriaMetricsSink::new(&config.victoria_metrics.url, config.general.percentiles.clone());
        handles.push(spawn_sink(
            Box::new(sink),
            reg.clone(),
            config.victoria_metrics.push_interval,
            config.general.fault_tolerant,
        ));
        tracing::info!(url = %config.victoria_metrics.url, "VictoriaMetrics push enabled");
    }

    let server = HttpServer::new(reg.clone(), config.general.percentiles.clone());
    server.serve(&config.general.listen).await?;
    Ok(())
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
