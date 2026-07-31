use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use log::info;

use crate::config::Config;
use crate::registry::SharedSessionRegistry;
use crate::rpc::ws_handler::ws_handler_fn;

// ── Embedded HTML ─────────────────────────────────────────────────────────

static INDEX_HTML: &str = include_str!("../../index.html");

// ── App state ─────────────────────────────────────────────────────────────

/// Shared state for the HTTP+WS server.
#[derive(Clone)]
pub struct AppState {
    pub registry:     SharedSessionRegistry,
    /// When `Some`, re-read HTML from disk on every request (dev mode).
    pub dev_html_dir: Option<Arc<PathBuf>>,
}

// ── Router ────────────────────────────────────────────────────────────────

fn build_router(registry: SharedSessionRegistry, dev_html_dir: Option<PathBuf>) -> Router {
    let state = AppState {
        registry,
        dev_html_dir: dev_html_dir.map(Arc::new),
    };

    Router::new()
        .route("/", get(serve_index))
        .route("/ws/control", get(ws_handler_fn))
        .with_state(state)
}

// ── HTTP server ───────────────────────────────────────────────────────────

pub async fn run(
    config: &Config,
    registry: SharedSessionRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_router(registry, config.dev_html.clone());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.http_port));
    info!("HTTP + WebSocket server listening on http://{}", addr);

    axum::serve(tokio::net::TcpListener::bind(&addr).await?, app).await?;
    Ok(())
}

// ── Page handlers ─────────────────────────────────────────────────────────

async fn serve_index(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    serve_html(&state.dev_html_dir, "index.html", INDEX_HTML).await
}

/// Serve an HTML page: from `dev_html_dir/{filename}` when configured and
/// the file exists, otherwise fall back to the embedded `fallback`.
pub async fn serve_html(
    dev_dir: &Option<Arc<PathBuf>>,
    filename: &str,
    fallback: &str,
) -> Response {
    if let Some(dir) = dev_dir {
        let path = dir.join(filename);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    content,
                )
                    .into_response();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Not in dev dir — fall through to embedded copy.
            }
            Err(e) => {
                log::warn!("dev-html: failed to read {}: {e}", path.display());
            }
        }
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        fallback.to_owned(),
    )
        .into_response()
}
