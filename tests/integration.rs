use std::sync::Arc;
use std::time::{Duration, Instant};
use nyquist_core::model::Kind;
use nyquist_core::registry::{MetricDef, Registry};
use nyquist_exposition::HttpServer;

#[tokio::test]
async fn end_to_end_metrics_pipeline() {
    let reg = Arc::new(Registry::new(Duration::from_millis(100), Duration::from_secs(1)));
    let id = reg.register(MetricDef::new("disk/read/bytes", Kind::Counter));
    let t0 = Instant::now();
    for i in 1..=30u64 {
        reg.record_counter(id, t0 + Duration::from_millis(i * 10), i * 4096);
    }
    let server = HttpServer::new(reg.clone(), vec![50.0, 99.0]);
    let app = server.router();

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let resp = app.oneshot(Request::builder().uri("/metrics").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("disk_read_bytes counter"), "{body}");
    assert!(body.contains("disk_read_bytes_rate"), "{body}");
}

#[tokio::test]
async fn recorder_sink_writes_parquet_file() {
    use tempfile::tempdir;
    use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
    use nyquist_core::model::{Labels, Unit};
    use nyquist_core::sink::Sink;
    use nyquist_recorder::sink::RecorderSink;
    use std::time::SystemTime;

    let dir = tempdir().unwrap();
    let mut sink = RecorderSink::new(
        dir.path().to_path_buf(),
        Duration::from_secs(3600),
        Duration::ZERO,
    ).unwrap();

    let snapshot = RegistrySnapshot {
        captured: SystemTime::UNIX_EPOCH + Duration::from_millis(2_000_000),
        metrics: vec![
            MetricSnapshot {
                name: "cpu/usage/user".to_string(),
                kind: Kind::Counter,
                unit: Unit::Count,
                labels: Labels::new(),
                raw: 55,
                buckets: vec![(20, 50), (40, 40), (50, 9), (55, 1)],
            },
        ],
    };
    sink.export(&snapshot).await.unwrap();

    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert_eq!(files.len(), 1, "one Parquet file from recorder sink");
    assert!(files[0].metadata().unwrap().len() > 0, "file is non-empty");
}
