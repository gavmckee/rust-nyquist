use nyquist_config::Config;
use std::time::Duration;

#[test]
fn defaults_are_sane() {
    let c = Config::default();
    assert_eq!(c.general.listen, "0.0.0.0:9100");
    assert_eq!(c.general.default_interval, Duration::from_millis(10));
    assert_eq!(c.general.window, Duration::from_secs(60));
    assert!(c.general.fault_tolerant);
}

#[test]
fn parses_toml_with_sampler_override() {
    let toml = r#"
        [general]
        listen = "127.0.0.1:1234"
        default_interval = "5ms"
        window = "30s"
        percentiles = [50.0, 99.9]
        fault_tolerant = false
        log = "debug"

        [samplers.network]
        enabled = true
        interval = "2ms"

        [samplers.disk]
        enabled = false
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert_eq!(c.general.default_interval, Duration::from_millis(5));
    assert_eq!(c.sampler("network").interval, Some(Duration::from_millis(2)));
    assert!(!c.sampler("disk").enabled);
    assert!(c.sampler("cpu").enabled);
    assert_eq!(c.sampler("cpu").interval, None);
}

#[test]
fn perf_config_defaults_to_disabled() {
    let c = Config::default();
    assert!(!c.perf.enabled);
    assert_eq!(c.perf.max_cpus, 32);
    assert!(c.perf.hw_enabled);
    assert!(c.perf.sw_enabled);
}

#[test]
fn perf_config_parses_from_toml() {
    let toml = r#"
        [perf]
        enabled    = true
        max_cpus   = 8
        hw_enabled = true
        sw_enabled = false
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert!(c.perf.enabled);
    assert_eq!(c.perf.max_cpus, 8);
    assert!(!c.perf.sw_enabled);
}
