use std::sync::Arc;
use std::time::{Duration, Instant};
use axum::{routing::get, Router, extract::State, response::IntoResponse, http::header};
use tokio::sync::Mutex;
use nyquist_core::registry::Registry;
use nyquist_core::snapshot::RegistrySnapshot;
use crate::format::{to_json, to_prometheus};

#[derive(Clone)]
struct AppState {
    reg: Arc<Registry>,
    percentiles: Arc<Vec<f64>>,
    cache: Arc<Mutex<Option<(Instant, RegistrySnapshot)>>>,
    context: Arc<Mutex<Option<(Instant, nyquist_sysconfig::SysConfig)>>>,
}

impl AppState {
    async fn snapshot(&self) -> RegistrySnapshot {
        let mut cache = self.cache.lock().await;
        if let Some((t, snap)) = cache.as_ref() {
            if t.elapsed() < Duration::from_millis(100) {
                return snap.clone();
            }
        }
        let snap = self.reg.snapshot(Instant::now(), &self.percentiles);
        *cache = Some((Instant::now(), snap.clone()));
        snap
    }
}

pub struct HttpServer { state: AppState }

impl HttpServer {
    pub fn new(reg: Arc<Registry>, percentiles: Vec<f64>) -> Self {
        HttpServer {
            state: AppState {
                reg,
                percentiles: Arc::new(percentiles),
                cache: Arc::new(Mutex::new(None)),
                context: Arc::new(Mutex::new(None)),
            },
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(|| async { "nyquist\n" }))
            .route("/metrics", get(metrics))
            .route("/metrics.json", get(metrics_json))
            .route("/context", get(context))
            .with_state(self.state.clone())
    }

    pub async fn serve(self, listen: &str) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(listen).await?;
        tracing::info!(%listen, "serving metrics");
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

async fn metrics(State(s): State<AppState>) -> impl IntoResponse {
    let snap = s.snapshot().await;
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], to_prometheus(&snap))
}

async fn metrics_json(State(s): State<AppState>) -> impl IntoResponse {
    let snap = s.snapshot().await;
    ([(header::CONTENT_TYPE, "application/json")], to_json(&snap))
}

async fn context(State(s): State<AppState>) -> impl IntoResponse {
    const TTL: Duration = Duration::from_secs(60);
    let mut cache = s.context.lock().await;
    let stale = cache.as_ref().map(|(t, _)| t.elapsed() > TTL).unwrap_or(true);
    if stale {
        let cfg = tokio::task::spawn_blocking(nyquist_sysconfig::collect).await.unwrap();
        *cache = Some((Instant::now(), cfg));
    }
    let json = serde_json::to_string_pretty(&cache.as_ref().unwrap().1).unwrap();
    ([(header::CONTENT_TYPE, "application/json")], json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::Kind;
    use nyquist_core::registry::{MetricDef, Registry};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_text() {
        let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
        let id = reg.register(MetricDef::new("cpu/usage", Kind::Counter));
        let t0 = Instant::now();
        for i in 1..=20u64 { reg.record_counter(id, t0 + Duration::from_millis(i * 10), i * 1000); }

        let server = HttpServer::new(reg.clone(), vec![50.0, 99.0]);
        let app = server.router();

        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let resp = app.oneshot(
            Request::builder().uri("/metrics").body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("# TYPE cpu_usage counter"), "{body}");
    }
}
