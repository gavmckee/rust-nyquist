//! libbpf-rs based BPF samplers (design §3). Linux-only; a no-op elsewhere.
#[cfg(target_os = "linux")]
pub mod h2;
#[cfg(target_os = "linux")]
pub mod tcp;
