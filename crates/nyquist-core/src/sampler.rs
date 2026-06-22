use std::time::{Duration, Instant};
use crate::registry::Registry;

pub type SamplerError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait::async_trait]
pub trait Sampler: Send {
    fn name(&self) -> &str;
    fn interval(&self) -> Duration;
    async fn sample(&mut self, reg: &Registry, now: Instant) -> Result<(), SamplerError>;
}
