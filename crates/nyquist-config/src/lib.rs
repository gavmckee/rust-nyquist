use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing config: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general:          General,
    pub samplers:         BTreeMap<String, SamplerConfig>,
    pub recorder:         RecorderConfig,
    pub clickhouse:       ClickHouseConfig,
    pub syswatch:         SysWatchConfig,
    pub perf:             PerfConfig,
    pub victoria_metrics: VictoriaMetricsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PerfConfig {
    pub enabled:    bool,
    pub max_cpus:   usize,
    pub hw_enabled: bool,
    pub sw_enabled: bool,
}

impl Default for PerfConfig {
    fn default() -> Self {
        PerfConfig { enabled: false, max_cpus: 32, hw_enabled: true, sw_enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RecorderConfig {
    pub enabled: bool,
    pub output_dir: String,
    #[serde(with = "humantime_serde")]
    pub rotation_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub flush_interval: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClickHouseConfig {
    pub enabled: bool,
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
    #[serde(with = "humantime_serde")]
    pub insert_interval: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    pub listen: String,
    #[serde(with = "humantime_serde")]
    pub default_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub window: Duration,
    pub percentiles: Vec<f64>,
    pub fault_tolerant: bool,
    pub log: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SamplerConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde::option")]
    pub interval: Option<Duration>,
}

impl Default for General {
    fn default() -> Self {
        General {
            listen: "0.0.0.0:9100".to_string(),
            default_interval: Duration::from_millis(10),
            window: Duration::from_secs(60),
            percentiles: vec![50.0, 90.0, 99.0, 99.9],
            fault_tolerant: true,
            log: "info".to_string(),
        }
    }
}

impl Default for SamplerConfig {
    fn default() -> Self { SamplerConfig { enabled: true, interval: None } }
}

impl Default for RecorderConfig {
    fn default() -> Self {
        RecorderConfig {
            enabled:           false,
            output_dir:        "/var/lib/nyquist/parquet".to_string(),
            rotation_interval: Duration::from_secs(3600),
            flush_interval:    Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VictoriaMetricsConfig {
    pub enabled: bool,
    pub url: String,
    #[serde(with = "humantime_serde")]
    pub push_interval: Duration,
}

impl Default for VictoriaMetricsConfig {
    fn default() -> Self {
        VictoriaMetricsConfig {
            enabled:       false,
            url:           "http://localhost:8428".to_string(),
            push_interval: Duration::from_secs(10),
        }
    }
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        ClickHouseConfig {
            enabled:         false,
            url:             "http://localhost:8123".to_string(),
            database:        "nyquist".to_string(),
            username:        "default".to_string(),
            password:        String::new(),
            insert_interval: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SysWatchConfig {
    pub enabled: bool,
}

impl Default for SysWatchConfig {
    fn default() -> Self { SysWatchConfig { enabled: false } }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            general:          General::default(),
            samplers:         BTreeMap::new(),
            recorder:         RecorderConfig::default(),
            clickhouse:       ClickHouseConfig::default(),
            perf:             PerfConfig::default(),
            victoria_metrics: VictoriaMetricsConfig::default(),
            syswatch:         SysWatchConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }
    pub fn sampler(&self, name: &str) -> SamplerConfig {
        self.samplers.get(name).cloned().unwrap_or_default()
    }
}
