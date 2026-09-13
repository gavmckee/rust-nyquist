//! Core data model and oversampling engine for nyquist.

pub mod model;
pub use model::{Kind, Labels, MetricId, Unit, metric_id};

pub mod hist;

pub mod registry;
pub use registry::{MetricDef, Registry};

pub mod percentiles;
pub mod coverage;

pub mod snapshot;
pub mod sink;
pub use snapshot::{MetricSnapshot, RegistrySnapshot};
pub use sink::{Sink, SinkError};

pub mod sampler;
pub mod scheduler;
pub mod registration;
pub use sampler::{Sampler, SamplerError};
pub use scheduler::spawn_sampler;

pub mod pending;
