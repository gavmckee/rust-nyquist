pub mod libbpf;

/// Touch the BPF sampler modules so their `#[distributed_slice(SAMPLERS)]`
/// entries are linked into the final binary.
pub fn registered() -> usize { nyquist_core::registration::all_sampler_names().len() }
