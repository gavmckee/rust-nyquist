use crate::snapshot::RegistrySnapshot;

pub type SinkError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait::async_trait]
pub trait Sink: Send {
    async fn export(&mut self, snapshot: &RegistrySnapshot) -> Result<(), SinkError>;

    /// Write out any internally buffered data. Called once at shutdown after
    /// the final export; stateless sinks keep the default no-op.
    async fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}
