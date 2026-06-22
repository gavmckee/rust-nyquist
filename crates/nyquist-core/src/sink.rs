use crate::snapshot::RegistrySnapshot;

pub type SinkError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait::async_trait]
pub trait Sink: Send {
    async fn export(&mut self, snapshot: &RegistrySnapshot) -> Result<(), SinkError>;
}
