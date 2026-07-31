//! Axum web server: serves the UI and provides REST + WebSocket endpoints.
//!
//! Routes:
//!   GET  /                       → embedded `index.html` (or from `--dev-html` dir)
//!   GET  /manager                → embedded `manager.html` (or from `--dev-html` dir)
//!   GET  /api/info               → JSON server capabilities (raw_enabled, …)
//!   GET  /ws/sessions            → WebSocket — pushes sessions list on every change
//!   GET  /ws/{session_id}        → WebSocket — sends history then live log entries
//!
//! Instance management REST API:
//!   GET    /api/instances              → list all proxy instances
//!   GET    /api/instances/{id}         → get one instance
//!   POST   /api/instances              → create a new instance
//!   PUT    /api/instances/{id}         → update instance config
//!   DELETE /api/instances/{id}         → remove an instance
//!   POST   /api/instances/{id}/start   → start an instance
//!   POST   /api/instances/{id}/stop    → stop an instance

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use tokio::sync::broadcast;

use crate::instance_manager::{InstanceConfig, InstanceId, ProxyInstanceManager};
use crate::session_registry::{LogEntry, SessionId, SharedRegistry};

// ── Embedded assets ───────────────────────────────────────────────────────

static INDEX_HTML: &str = include_str!("../index.html");
static MANAGER_HTML: &str = include_str!("../manager.html");

// ── Shared manager alias ─────────────────────────────────────────────────

pub type SharedManager = Arc<tokio::sync::Mutex<ProxyInstanceManager>>;

// ── App state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    registry: SharedRegistry,
    manager: SharedManager,
    /// When `Some`, serve HTML files from this directory instead of
    /// the built-in copies.  Files are re-read on every request so
    /// changes take effect immediately (dev mode).
    dev_html_dir: Option<Arc<PathBuf>>,
}

// ── Router ────────────────────────────────────────────────────────────────

pub fn build_router(
    registry: SharedRegistry,
    manager: SharedManager,
    dev_html_dir: Option<PathBuf>,
) -> Router {
    let state = AppState {
        registry,
        manager,
        dev_html_dir: dev_html_dir.map(Arc::new),
    };
    Router::new()
        // Existing routes
        .route("/", get(serve_index))
        .route("/manager", get(serve_manager))
        .route("/api/info", get(api_info))
        .route("/ws/sessions", get(ws_sessions_handler))
        .route("/ws/{session_id}", get(ws_handler))
        // Instance management API
        .route("/api/instances", get(list_instances).post(create_instance))
        .route("/api/instances/{id}", get(get_instance).put(update_instance).delete(delete_instance))
        .route("/api/instances/{id}/start", post(start_instance))
        .route("/api/instances/{id}/stop", post(stop_instance))
        .with_state(state)
}

pub async fn run_server(
    registry: SharedRegistry,
    manager: SharedManager,
    addr: &str,
    dev_html_dir: Option<PathBuf>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    let app = build_router(registry, manager, dev_html_dir);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind web server");
    log::info!("Web UI listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("web server error");
    log::info!("Web UI server stopped");
}

// ── Page handlers ─────────────────────────────────────────────────────────

/// Serve an HTML page: from `dev_html_dir/{filename}` if the dev directory is
/// configured and the file exists, otherwise from the embedded `fallback`.
async fn serve_html(dev_dir: &Option<Arc<PathBuf>>, filename: &str, fallback: &str) -> Response<String> {
    if let Some(dir) = dev_dir {
        let path = dir.join(filename);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(content)
                    .unwrap();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File not in dev dir — fall through to embedded copy.
            }
            Err(e) => {
                log::error!("Failed to read {filename} from {}: {e}", path.display());
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(format!("Failed to read {filename}: {e}"))
                    .unwrap();
            }
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(fallback.to_string())
        .unwrap()
}

async fn serve_index(State(state): State<AppState>) -> Response<String> {
    serve_html(&state.dev_html_dir, "index.html", INDEX_HTML).await
}

async fn serve_manager(State(state): State<AppState>) -> Response<String> {
    serve_html(&state.dev_html_dir, "manager.html", MANAGER_HTML).await
}

async fn api_info(_state: State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({}))
}

// ── Instance management API ───────────────────────────────────────────────

/// `GET /api/instances` — list all proxy instances.
async fn list_instances(State(state): State<AppState>) -> impl IntoResponse {
    let mgr = state.manager.lock().await;
    let instances = mgr.list();
    Json(serde_json::json!({ "ok": true, "instances": instances }))
}

/// `GET /api/instances/{id}` — get a single instance.
async fn get_instance(
    State(state): State<AppState>,
    Path(id): Path<InstanceId>,
) -> impl IntoResponse {
    let mgr = state.manager.lock().await;
    match mgr.get(id) {
        Some(info) => Json(serde_json::json!({ "ok": true, "instance": info })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "error": format!("instance #{id} not found") })),
        ).into_response(),
    }
}

/// Request body for `POST /api/instances`.
#[derive(serde::Deserialize)]
struct CreateInstanceRequest {
    config: InstanceConfig,
    #[serde(default)]
    auto_start: bool,
}

/// `POST /api/instances` — create a new proxy instance.
async fn create_instance(
    State(state): State<AppState>,
    Json(body): Json<CreateInstanceRequest>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock().await;
    match mgr.add(body.config, body.auto_start) {
        Ok(id) => {
            let info = mgr.get(id);
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "ok": true, "id": id, "instance": info })),
            ).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ).into_response(),
    }
}

/// Request body for `PUT /api/instances/{id}`.
#[derive(serde::Deserialize)]
struct UpdateInstanceRequest {
    config: InstanceConfig,
    #[serde(default)]
    auto_start: bool,
}

/// `PUT /api/instances/{id}` — update instance configuration.
async fn update_instance(
    State(state): State<AppState>,
    Path(id): Path<InstanceId>,
    Json(body): Json<UpdateInstanceRequest>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock().await;
    match mgr.update(id, body.config, body.auto_start).await {
        Ok(restarted) => {
            let info = mgr.get(id);
            Json(serde_json::json!({ "ok": true, "restarted": restarted, "instance": info })).into_response()
        }
        Err(e) => {
            let status = if e.contains("not found") { StatusCode::NOT_FOUND }
                         else { StatusCode::BAD_REQUEST };
            (status, Json(serde_json::json!({ "ok": false, "error": e }))).into_response()
        }
    }
}

/// `DELETE /api/instances/{id}` — remove an instance (stops it if running).
async fn delete_instance(
    State(state): State<AppState>,
    Path(id): Path<InstanceId>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock().await;
    match mgr.remove(id).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ).into_response(),
    }
}

/// `POST /api/instances/{id}/start` — start an instance.
async fn start_instance(
    State(state): State<AppState>,
    Path(id): Path<InstanceId>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock().await;
    match mgr.start(id).await {
        Ok(()) => {
            let info = mgr.get(id);
            Json(serde_json::json!({ "ok": true, "instance": info })).into_response()
        }
        Err(e) => {
            let status = if e.contains("not found") { StatusCode::NOT_FOUND }
                         else if e.contains("already running") { StatusCode::CONFLICT }
                         else { StatusCode::BAD_REQUEST };
            (status, Json(serde_json::json!({ "ok": false, "error": e }))).into_response()
        }
    }
}

/// `POST /api/instances/{id}/stop` — stop an instance.
async fn stop_instance(
    State(state): State<AppState>,
    Path(id): Path<InstanceId>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock().await;
    match mgr.stop(id).await {
        Ok(()) => {
            let info = mgr.get(id);
            Json(serde_json::json!({ "ok": true, "instance": info })).into_response()
        }
        Err(e) => {
            let status = if e.contains("not found") { StatusCode::NOT_FOUND }
                         else if e.contains("not running") { StatusCode::CONFLICT }
                         else { StatusCode::BAD_REQUEST };
            (status, Json(serde_json::json!({ "ok": false, "error": e }))).into_response()
        }
    }
}

// ── WebSocket handlers (unchanged) ───────────────────────────────────────

async fn ws_sessions_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_sessions(socket, state.registry))
}

async fn handle_ws_sessions(mut socket: WebSocket, registry: SharedRegistry) {
    let (initial, mut rx) = registry.subscribe_sessions();

    // Send the current list immediately.
    let msg = serde_json::json!({ "type": "sessions", "sessions": initial });
    if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(sessions) => {
                        let msg = serde_json::json!({ "type": "sessions", "sessions": sessions });
                        if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let sessions = registry.list_sessions();
                        let msg = serde_json::json!({ "type": "sessions", "sessions": sessions });
                        if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                if msg.is_none() { break; }
            }
        }
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<SessionId>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, session_id, state.registry))
}

// ── Log entry WebSocket ───────────────────────────────────────────────────

/// Sends the full history snapshot first, then streams live entries.
///
/// Each entry is a JSON object with a `"type"` field (`"packet"` or `"raw"`)
/// thanks to `#[serde(tag = "type")]` on [`LogEntry`].
///
/// The history batch is wrapped: `{"type": "history", "entries": [...]}`.
/// Live entries are sent individually as-is (each already carries `"type"`).
async fn handle_ws(mut socket: WebSocket, session_id: SessionId, registry: SharedRegistry) {
    let Some((history, mut rx)) = registry.subscribe(session_id) else {
        let msg = serde_json::json!({ "type": "error", "message": "session not found" });
        let _ = socket.send(Message::Text(msg.to_string().into())).await;
        return;
    };

    // Send history batch.
    let history_msg = serde_json::json!({ "type": "history", "entries": history });
    if socket
        .send(Message::Text(history_msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Stream live entries until the client disconnects or the session ends.
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(entry) => {
                        if send_entry(&mut socket, &entry).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("ws/{session_id}: lagged by {n} entries");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                if msg.is_none() { break; }
            }
        }
    }
}

async fn send_entry(socket: &mut WebSocket, entry: &LogEntry) -> Result<(), ()> {
    let json = serde_json::to_string(entry).map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}
