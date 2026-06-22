use std::time::Duration;
use nyquist_core::sink::{Sink, SinkError};
use nyquist_core::snapshot::RegistrySnapshot;
use crate::format::to_prometheus;

/// Sink that pushes snapshots to VictoriaMetrics (or any Prometheus-compatible
/// remote write endpoint) using the text-format import API.
pub struct VictoriaMetricsSink {
    url: String,
    client: reqwest::Client,
}

impl VictoriaMetricsSink {
    pub fn new(base_url: impl Into<String>) -> Self {
        let base = base_url.into();
        let url = format!("{}/api/v1/import/prometheus", base.trim_end_matches('/'));
        VictoriaMetricsSink {
            url,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
        }
    }
}

#[async_trait::async_trait]
impl Sink for VictoriaMetricsSink {
    async fn export(&mut self, snapshot: &RegistrySnapshot) -> Result<(), SinkError> {
        if snapshot.metrics.is_empty() { return Ok(()); }
        let body = to_prometheus(snapshot);
        self.client
            .post(&self.url)
            .header("Content-Type", "text/plain")
            .body(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}
