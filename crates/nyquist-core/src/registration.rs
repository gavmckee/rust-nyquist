use std::time::Duration;
use linkme::distributed_slice;
use crate::registry::Registry;
use crate::sampler::Sampler;

/// A registered sampler: its stable config name plus a constructor.
/// Mirrors rezolus's `SamplerEntry` (design §3.3), adapted to nyquist's
/// `Sampler` trait (`name`/`interval`/`sample`).
pub struct SamplerEntry {
    pub name: &'static str,
    pub init: fn(reg: &Registry, interval: Duration) -> Box<dyn Sampler>,
}

/// Every sampler registers into this slice via `#[distributed_slice(SAMPLERS)]`.
#[distributed_slice]
pub static SAMPLERS: [SamplerEntry] = [..];

/// Names of all registered samplers (config knows these by name).
pub fn all_sampler_names() -> Vec<&'static str> {
    SAMPLERS.iter().map(|e| e.name).collect()
}

/// Build the enabled samplers by iterating the distributed slice.
pub fn build_enabled(
    reg: &Registry,
    default_interval: Duration,
    is_enabled: impl Fn(&str) -> bool,
    interval_for: impl Fn(&str) -> Option<Duration>,
) -> Vec<Box<dyn Sampler>> {
    let mut out: Vec<Box<dyn Sampler>> = Vec::new();
    for entry in SAMPLERS.iter() {
        if !is_enabled(entry.name) {
            continue;
        }
        let iv = interval_for(entry.name).unwrap_or(default_interval);
        out.push((entry.init)(reg, iv));
    }
    out
}
