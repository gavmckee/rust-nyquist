//! Live smoke test: verifies GSTRINGS/GSTATS still work after the
//! slack-buffer TOCTOU fix. Passes trivially on hosts without physical NICs.
#[test]
fn get_driver_stats_returns_entries_on_physical_nic() {
    let Ok(dir) = std::fs::read_dir("/sys/class/net") else { return };
    let mut physical = Vec::new();
    for e in dir.flatten() {
        let name = e.file_name().into_string().unwrap_or_default();
        if !e.path().join("device").exists() { continue; }
        let stats = nyquist_sysconfig::get_driver_stats(&name);
        eprintln!("{name}: {} stats", stats.len());
        if !stats.is_empty() {
            return; // at least one NIC produced stats — pass
        }
        physical.push(name);
    }
    assert!(physical.is_empty(), "physical NICs {physical:?} but zero driver stats");
}
