use axum::{
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sanitize_filename::sanitize;
use serde::Serialize;
use std::{net::SocketAddr, path::{Path as FsPath, PathBuf}, sync::Arc};
use tokio::{fs, io::AsyncWriteExt};
use tower_http::{
    cors::CorsLayer,
    services::ServeDir,
    trace::TraceLayer,
};
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};
use flate2::read::GzDecoder;
use std::io::Read;

#[derive(Clone)]
struct AppState {
    cfg: Arc<AppConfig>,
}

#[derive(Debug, Clone)]
struct AppConfig {
    bind_addr: String,
    upload_dir: String,
}

impl AppConfig {
    fn load() -> anyhow::Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .build()?;

        Ok(Self {
            bind_addr: cfg.get_string("bind_addr")
                .unwrap_or_else(|_| "127.0.0.1:8080".to_string()),
            upload_dir: cfg.get_string("upload_dir")
                .unwrap_or_else(|_| "uploads".to_string()),
        })
    }
}

#[derive(Serialize)]
struct FileInfo { name: String, size: u64 }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::from_default_env().
                add_directive("info".parse().unwrap())
        ).init();

    let cfg = Arc::new(AppConfig::load()?);
    fs::create_dir_all(&cfg.upload_dir).await?;

    let state = AppState { cfg };

    let public = ServeDir::new("public");
    let files = ServeDir::new(state.cfg.upload_dir.clone());

    let app = Router::new()
        .nest_service("/", public)
        .nest_service("/files", files)
        .route("/api/upload", post(upload))
        .route("/api/list", get(list_files))
        .route("/api/delete/:name", post(delete_file))
        .with_state(state.clone())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = state.cfg.bind_addr.parse()?;
    info!("Listening on {}", addr);

    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

async fn upload(
    State(state): State<AppState>,
    mut mp: Multipart,
) -> impl IntoResponse {
    while let Ok(Some(field)) = mp.next_field().await {
        if field.name() != Some("file") { continue; }

        let filename = field.file_name()
            .map(|s| s.to_string())
            .unwrap_or("upload.bin".into());
        
        let bytes = match field.bytes().await {
            Ok(b) => b,
            Err(e) => {
                error!("Error reading file: {}", e);
                return (
                    StatusCode::BAD_REQUEST,
                    "Error reading file"
                ).into_response();
            }
        };

        let (final_bytes, final_filename) = if filename.ends_with(".gz") || is_gzipped(&bytes) {
            match decompress_gzip(&bytes) {
                Ok(decompressed) => {
                    let base_name = filename.trim_end_matches(".gz");
                    (decompressed, base_name.to_string())
                }
                Err(e) => {
                    error!("Failed to decompress gzip: {}", e);
                    return (
                        StatusCode::BAD_REQUEST,
                        "Failed to decompress gzipped file"
                    ).into_response();
                }
            }
        } else {
            (bytes.to_vec(), filename)
        };

        let safe = sanitize(&final_filename);
        let mut path = PathBuf::from(&state.cfg.upload_dir);
        path.push(&safe);

        let tmp = path.with_extension("part");
        if let Err(e) = write_atomic(&tmp, &path, &final_bytes).await {
            error!(?e, "save failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error saving file"
            ).into_response();
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            "Location",
            format!("/files/{}", safe).parse().unwrap()
        );

        return (
            StatusCode::CREATED,
            headers,
            format!("saved to {}", path.display())
        ).into_response();
    }

    (StatusCode::OK, "OK").into_response()
}

async fn write_atomic(
    tmp: &PathBuf,
    final_path: &PathBuf,
    bytes: &[u8])
-> anyhow::Result<()> {
    let mut file = fs::File::create(tmp).await?;
    file.write_all(bytes).await?;
    file.flush().await?;
    drop(file);
    fs::rename(tmp, final_path).await?;
    Ok(())
}

async fn list_files(State(state): State<AppState>) -> impl IntoResponse {
    let mut out = Vec::<FileInfo>::new();
    let mut rd = match fs::read_dir(&state.cfg.upload_dir).await {
        Ok(r) => r,
        Err(_) => return Json(out)
    };

    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }
        let md = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_file() {
            out.push(FileInfo { name, size: md.len() });
        }
    }

    Json(out)
}

async fn delete_file(
    State(state): State<AppState>,
    Path(name): Path<String>
) -> impl IntoResponse {
    let safe = sanitize(&name);
    let p = PathBuf::from(&state.cfg.upload_dir).join(&safe);
    if !p.starts_with(FsPath::new(&state.cfg.upload_dir)) {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    match fs::remove_file(p).await {
        Ok(_) => (StatusCode::OK, "deleted").into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn is_gzipped(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

fn decompress_gzip(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    Ok(decompressed)
}


