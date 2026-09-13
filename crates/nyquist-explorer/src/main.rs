mod data;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{header, StatusCode},
    response::Html,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Semaphore;

#[derive(Parser)]
#[command(about = "Explore Nyquist Parquet recordings in your browser")]
struct Cli {
    /// Directory containing server recordings (optional; uploads always work).
    #[arg(long)]
    recordings: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:9101")]
    listen: String,
}

#[derive(Clone)]
struct App {
    root: Option<PathBuf>,
    readers: Arc<Semaphore>,
}

type ApiError = (StatusCode, Json<serde_json::Value>);
fn error(message: impl ToString) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": message.to_string()})),
    )
}

fn router(app: App) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../static/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../static/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../static/style.css"),
                )
            }),
        )
        .route("/api/files", get(files))
        .route("/api/recording", get(recording))
        .route("/api/upload", post(upload))
        .layer(DefaultBodyLimit::max(data::MAX_BYTES))
        .with_state(app)
}

#[derive(Serialize)]
struct FileInfo {
    name: String,
    bytes: u64,
}
async fn files(State(app): State<App>) -> Result<Json<Vec<FileInfo>>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        if let Some(root) = app.root {
            for entry in std::fs::read_dir(&root).map_err(error)? {
                let entry = entry.map_err(error)?;
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "parquet") && path.is_file() {
                    if let Some(name) = entry.file_name().to_str() {
                        if resolve(&root, name).is_err() {
                            continue;
                        }
                        files.push(FileInfo {
                            name: name.into(),
                            bytes: entry.metadata().map_err(error)?.len(),
                        });
                    }
                }
            }
        }
        files.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(Json(files))
    })
    .await
    .map_err(error)?
}

fn resolve(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        Path::new(name).file_name().and_then(|n| n.to_str()) == Some(name),
        "Choose a file in the recording directory"
    );
    anyhow::ensure!(
        Path::new(name).extension().is_some_and(|e| e == "parquet"),
        "Choose a .parquet file"
    );
    let path = root.join(name).canonicalize()?;
    anyhow::ensure!(
        path.starts_with(root) && path.is_file(),
        "File is outside the recording directory"
    );
    Ok(path)
}

#[derive(Deserialize)]
struct FileQuery {
    name: String,
}
async fn recording(
    State(app): State<App>,
    Query(query): Query<FileQuery>,
) -> Result<Json<data::Recording>, ApiError> {
    let permit = app.readers.clone().acquire_owned().await.map_err(error)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let root = app.root.as_ref().ok_or_else(|| {
            error("Server directory is not configured; upload a recording instead")
        })?;
        let path = resolve(root, &query.name).map_err(error)?;
        let file = std::fs::File::open(path).map_err(error)?;
        if file.metadata().map_err(error)?.len() > data::MAX_BYTES as u64 {
            return Err(error("File exceeds 128 MiB"));
        }
        data::read_recording(file).map(Json).map_err(error)
    })
    .await
    .map_err(error)?
}

async fn upload(State(app): State<App>, bytes: Bytes) -> Result<Json<data::Recording>, ApiError> {
    let permit = app.readers.clone().acquire_owned().await.map_err(error)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // Parquet's Bytes reader avoids storing uploaded files on the server.
        read_bytes(bytes).map(Json).map_err(error)
    })
    .await
    .map_err(error)?
}

fn read_bytes(bytes: Bytes) -> anyhow::Result<data::Recording> {
    data::read_recording(bytes)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let root = cli.recordings.map(std::fs::canonicalize).transpose()?;
    if let Some(root) = &root {
        anyhow::ensure!(root.is_dir(), "--recordings must be a directory");
    }
    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    println!("Nyquist Explorer: http://{}", listener.local_addr()?);
    axum::serve(
        listener,
        router(App {
            root,
            readers: Arc::new(Semaphore::new(2)),
        }),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use nyquist_core::{Kind, Labels, MetricSnapshot, Unit};
    use nyquist_recorder::{schema::RowAccumulator, writer::ParquetWriter};
    use tower::ServiceExt;

    fn fixture(buckets: Vec<(u64, u64)>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = RowAccumulator::new();
        rows.push(
            1_700_000_000_000,
            &MetricSnapshot {
                name: "network/receive/bytes".into(),
                kind: Kind::Counter,
                unit: Unit::Bytes,
                labels: Labels::new().insert("iface", "eth0"),
                raw: u64::MAX,
                buckets,
            },
        );
        let mut writer =
            ParquetWriter::new(dir.path().into(), std::time::Duration::from_secs(3600)).unwrap();
        writer.flush(&mut rows).unwrap();
        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        (dir, path)
    }
    fn app(root: Option<PathBuf>) -> Router {
        router(App {
            root,
            readers: Arc::new(Semaphore::new(2)),
        })
    }

    #[tokio::test]
    async fn recorder_file_roundtrips_through_upload_and_server() {
        let (dir, path) = fixture(vec![(100, 90), (1000, 10)]);
        let bytes = std::fs::read(&path).unwrap();
        let response = app(None)
            .oneshot(
                Request::post("/api/upload")
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let uploaded = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&uploaded).unwrap();
        assert_eq!(json["rows"][0]["raw"], u64::MAX.to_string());
        assert_eq!(json["rows"][0]["labels"]["iface"], "eth0");
        assert_eq!(json["rows"][0]["p50"], "100");
        assert_eq!(json["rows"][0]["p99"], "1000");
        let name = path.file_name().unwrap().to_str().unwrap();
        let response = app(Some(dir.path().canonicalize().unwrap()))
            .oneshot(
                Request::get(format!("/api/recording?name={name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            uploaded
        );
    }

    #[test]
    fn missing_histogram_is_missing_not_zero_and_bad_bounds_are_rejected() {
        let (_dir, path) = fixture(vec![]);
        let result = data::read_recording(std::fs::File::open(path).unwrap()).unwrap();
        assert!(result.rows[0].p99.is_none());
        let (_dir, path) = fixture(vec![(1000, 1), (100, 1)]);
        assert!(data::read_recording(std::fs::File::open(path).unwrap()).is_err());
        let (_dir, path) = fixture(vec![(100, u64::MAX), (1000, 1)]);
        assert!(data::read_recording(std::fs::File::open(path).unwrap()).is_err());
    }

    #[tokio::test]
    async fn invalid_file_and_paths_return_errors() {
        let response = app(None)
            .oneshot(
                Request::post("/api/upload")
                    .body(Body::from("not parquet"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let (dir, _) = fixture(vec![]);
        assert!(resolve(dir.path(), "../secret.parquet").is_err());
        assert!(resolve(dir.path(), "/secret.parquet").is_err());
        assert!(resolve(dir.path(), "file.txt").is_err());
        #[cfg(unix)]
        {
            let (_outside, path) = fixture(vec![]);
            std::os::unix::fs::symlink(path, dir.path().join("outside.parquet")).unwrap();
            assert!(resolve(&dir.path().canonicalize().unwrap(), "outside.parquet").is_err());
        }
    }

    #[tokio::test]
    async fn list_only_completed_parquet_files() {
        let (dir, _) = fixture(vec![]);
        std::fs::write(dir.path().join("incomplete.parquet.tmp"), b"temp").unwrap();
        let response = app(Some(dir.path().into()))
            .oneshot(Request::get("/api/files").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(json.as_array().unwrap().len(), 1);
    }
}
