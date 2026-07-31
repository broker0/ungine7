//! Axum web server for the headless replay anima.
//!
//! Provides a web UI and WebSocket API for controlling UO replay playback.
//!
//! # Routes
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `GET`  | `/` | Serve the replay control web UI |
//! | `GET`  | `/api/status` | Current playback status (JSON) |
//! | `GET`  | `/api/logs` | List available `.uolog` files |
//! | `GET`  | `/ws` | WebSocket — bidirectional command/status channel |
//!
//! # WebSocket protocol (JSON)
//!
//! **Incoming (browser → server):**
//! ```json
//! {"cmd": "pause"}
//! {"cmd": "seek", "ms": 15000}
//! {"cmd": "seek_relative", "delta_ms": -10000}
//! {"cmd": "step", "count": 1}
//! {"cmd": "step_client", "count": -1}
//! {"cmd": "step_server", "count": 1}
//! {"cmd": "fast_forward", "delta_ms": 5000}
//! {"cmd": "restart"}
//! {"cmd": "stop"}
//! {"cmd": "set_speed", "speed": 2.0}
//! ```
//!
//! **Outgoing (server → browser):**
//! ```json
//! {"type": "status", ...PlaybackStatus fields...}
//! {"type": "session", "event": "connected"|"disconnected"|"log_loaded", ...}
//! ```

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use log::{debug, info, warn};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::replay_session::playback_headless::{PlaybackCommand, PlaybackStatus, PacketLogEntry};

// ── Embedded assets ───────────────────────────────────────────────────────

static REPLAY_HTML: &str = include_str!("../replay.html");

// ── Session events (broadcast to web clients) ─────────────────────────────

/// Events about the replay session lifecycle, broadcast to all WebSocket clients.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum SessionEvent {
    /// A UO client connected to the replay server.
    #[serde(rename = "client_connected")]
    ClientConnected,
    /// The UO client disconnected.
    #[serde(rename = "client_disconnected")]
    ClientDisconnected,
    /// A log file was loaded and preprocessing is complete.
    #[serde(rename = "log_loaded")]
    LogLoaded {
        log_name: String,
        char_name: Option<String>,
    },
    /// Playback finished (reached end of log).
    #[serde(rename = "playback_finished")]
    PlaybackFinished,
}

// ── Shared state ──────────────────────────────────────────────────────────

/// Maximum number of packet log entries kept in the ring buffer for history.
const PACKET_HISTORY_CAPACITY: usize = 4000;

/// Shared state between the web server and the replay continuum.
///
/// This is wrapped in `Arc` and passed to all Axum handlers.
pub struct ReplayAppState {
    /// Sender for playback commands — web UI pushes commands here.
    /// `None` if no replay session is active.
    pub command_tx: Mutex<Option<mpsc::Sender<PlaybackCommand>>>,
    /// Receiver for playback status updates — web UI subscribes here.
    pub status_tx: broadcast::Sender<PlaybackStatus>,
    /// Receiver for session lifecycle events.
    pub session_tx: broadcast::Sender<SessionEvent>,
    /// Broadcast channel for packet log entries — web UI subscribes here.
    pub packet_log_tx: broadcast::Sender<PacketLogEntry>,
    /// Last known playback status (for new WebSocket clients).
    pub last_status: Mutex<Option<PlaybackStatus>>,
    /// Ring buffer of recent packet log entries (for new WebSocket clients).
    pub packet_history: Mutex<VecDeque<PacketLogEntry>>,
    /// Directory containing `.uolog` files.
    pub logs_dir: PathBuf,
    /// Optional directory for serving HTML from disk (dev mode).
    pub dev_html_dir: Option<PathBuf>,
}

pub type SharedAppState = Arc<ReplayAppState>;

impl ReplayAppState {
    pub fn new(logs_dir: PathBuf, dev_html_dir: Option<PathBuf>) -> Self {
        let (status_tx, _) = broadcast::channel(256);
        let (session_tx, _) = broadcast::channel(64);
        let (packet_log_tx, _) = broadcast::channel(8192);
        Self {
            command_tx: Mutex::new(None),
            status_tx,
            session_tx,
            packet_log_tx,
            last_status: Mutex::new(None),
            packet_history: Mutex::new(VecDeque::with_capacity(PACKET_HISTORY_CAPACITY)),
            logs_dir,
            dev_html_dir,
        }
    }

    /// Update the last known status (called from the status broadcast listener).
    pub async fn update_status(&self, status: PlaybackStatus) {
        *self.last_status.lock().await = Some(status);
    }

    /// Push a packet log entry into the ring buffer.
    pub async fn push_packet_log(&self, entry: PacketLogEntry) {
        let mut history = self.packet_history.lock().await;
        if history.len() >= PACKET_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(entry);
    }

    /// Clear the packet history (e.g. on replay restart).
    pub async fn clear_packet_history(&self) {
        self.packet_history.lock().await.clear();
    }
}

// ── Router ────────────────────────────────────────────────────────────────

pub fn build_router(state: SharedAppState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/api/status", get(api_status))
        .route("/api/logs", get(api_logs))
        .route("/api/packets", get(api_packets))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

pub async fn run_server(
    state: SharedAppState,
    addr: &str,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind web server");
    info!("Replay Web UI listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("web server error");
    info!("Replay Web UI server stopped");
}

// ── Page handlers ─────────────────────────────────────────────────────────

async fn serve_index(State(state): State<SharedAppState>) -> Response<String> {
    if let Some(ref dir) = state.dev_html_dir {
        let path = dir.join("replay.html");
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(content)
                    .unwrap();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Fall through to embedded copy.
            }
            Err(e) => {
                log::error!("Failed to read replay.html from {}: {e}", path.display());
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(format!("Failed to read replay.html: {e}"))
                    .unwrap();
            }
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(REPLAY_HTML.to_string())
        .unwrap()
}

// ── REST API ──────────────────────────────────────────────────────────────

/// `GET /api/status` — current playback status.
async fn api_status(State(state): State<SharedAppState>) -> impl IntoResponse {
    let status = state.last_status.lock().await.clone();
    match status {
        Some(s) => Json(serde_json::json!({
            "ok": true,
            "status": s,
        })).into_response(),
        None => Json(serde_json::json!({
            "ok": true,
            "status": null,
            "message": "no active playback session",
        })).into_response(),
    }
}

/// `GET /api/logs` — list `.uolog` files in the logs directory.
async fn api_logs(State(state): State<SharedAppState>) -> impl IntoResponse {
    match crate::packet_log::scan_log_files(&state.logs_dir) {
        Ok(files) => {
            let names: Vec<String> = files
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .collect();
            Json(serde_json::json!({ "ok": true, "logs": names }))
        }
        Err(e) => {
            Json(serde_json::json!({ "ok": false, "error": format!("{e}") }))
        }
    }
}

/// `GET /api/packets` — return the packet log ring buffer (history).
async fn api_packets(State(state): State<SharedAppState>) -> impl IntoResponse {
    let history = state.packet_history.lock().await;
    let entries: Vec<&PacketLogEntry> = history.iter().collect();
    Json(serde_json::json!({ "ok": true, "packets": entries }))
}

// ── WebSocket ─────────────────────────────────────────────────────────────

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedAppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// WebSocket command from the browser.
#[derive(Debug, Deserialize)]
struct WsCommand {
    cmd: String,
    #[serde(default)]
    ms: Option<u64>,
    #[serde(default)]
    delta_ms: Option<i64>,
    #[serde(default)]
    count: Option<i32>,
    #[serde(default)]
    speed: Option<f64>,
}

impl WsCommand {
    /// Try to convert a JSON command into a `PlaybackCommand`.
    fn to_playback_command(&self) -> Option<PlaybackCommand> {
        match self.cmd.as_str() {
            "pause" | "toggle_pause" => Some(PlaybackCommand::TogglePause),
            "seek" => self.ms.map(PlaybackCommand::SeekAbsolute),
            "seek_relative" => self.delta_ms.map(PlaybackCommand::SeekRelative),
            "step" => self.count.map(PlaybackCommand::StepPacket),
            "step_client" => self.count.map(PlaybackCommand::StepClientPacket),
            "step_server" => self.count.map(PlaybackCommand::StepServerPacket),
            "fast_forward" => self.delta_ms.map(PlaybackCommand::FastForward),
            "restart" => Some(PlaybackCommand::Restart),
            "stop" => Some(PlaybackCommand::Stop),
            "set_speed" => self.speed.map(PlaybackCommand::SetSpeed),
            _ => {
                warn!("[ws] unknown command: {}", self.cmd);
                None
            }
        }
    }
}

async fn handle_ws(mut socket: WebSocket, state: SharedAppState) {
    info!("[ws] new WebSocket client connected");

    // Send current status immediately if available.
    {
        let status = state.last_status.lock().await.clone();
        if let Some(s) = status {
            let msg = serde_json::json!({ "type": "status", "data": s });
            if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                return;
            }
        }
    }

    // Send packet history snapshot.
    {
        let history = state.packet_history.lock().await;
        if !history.is_empty() {
            let entries: Vec<&PacketLogEntry> = history.iter().collect();
            let msg = serde_json::json!({ "type": "packet_history", "entries": entries });
            if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                return;
            }
        }
    }

    // Subscribe to status, session, and packet log broadcasts.
    let mut status_rx = state.status_tx.subscribe();
    let mut session_rx = state.session_tx.subscribe();
    let mut packet_rx = state.packet_log_tx.subscribe();

    loop {
        tokio::select! {
            // Status updates from playback continuum → forward to browser.
            result = status_rx.recv() => {
                match result {
                    Ok(status) => {
                        let msg = serde_json::json!({ "type": "status", "data": status });
                        if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!("[ws] lagged by {n} status messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            // Session events → forward to browser.
            result = session_rx.recv() => {
                match result {
                    Ok(event) => {
                        let msg = serde_json::to_string(&event).unwrap_or_default();
                        if socket.send(Message::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            // Packet log entries → forward to browser.
            result = packet_rx.recv() => {
                match result {
                    Ok(entry) => {
                        let msg = serde_json::json!({ "type": "packet", "data": entry });
                        if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!("[ws] lagged by {n} packet log entries");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            // Commands from browser → forward to playback continuum.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsCommand>(&text) {
                            Ok(ws_cmd) => {
                                if let Some(pb_cmd) = ws_cmd.to_playback_command() {
                                    let cmd_tx = state.command_tx.lock().await;
                                    if let Some(tx) = cmd_tx.as_ref() {
                                        if let Err(e) = tx.send(pb_cmd).await {
                                            debug!("[ws] failed to send command: {e}");
                                        }
                                    } else {
                                        debug!("[ws] no active session — command dropped");
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("[ws] failed to parse command: {e}");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    info!("[ws] WebSocket client disconnected");
}
