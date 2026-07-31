//! WebSocket mirror endpoint for demo-server.
//!
//! Exposes `/ws/mirror` over an axum HTTP server started on `--mirror-port`.
//! Each binary WebSocket frame is treated as a single raw S2C UO packet;
//! the ingestor tracks the current world via `0xBF SetMap` and forwards
//! accepted packets to the demo-server worker as `IngestPacket`.
//!
//! Only compiled when the `mirror` feature is enabled.
//!
//! ## Packet filter
//!
//! Uses [`default_ingestable`] for entity-level and text/tooltip packets
//! (items, mobiles, deletes, HP/status, `0xD6`/`0xC1`/`0xAE`/`0x88`).
//!
//! Container packets (`0x24`/`0x25`/`0x3C`) are **not** passed through
//! `is_ingestable` because they take a separate engine command
//! (`IngestContainerPacket`).  They are handled directly here in the socket
//! loop alongside the main `process_packet` call.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use bytes::Bytes;
use log::warn;

use common::uo_engine::mirror::{
    default_ingestable, log_action, log_connected, log_disconnected, log_send_failed,
    next_mirror_id, MirrorIngestor,
};
use common::uo_engine::handler::EngineCommand;
use framework::continuum::WorkerCommand;

use crate::commands::{DemoCommand, DemoWorkerTx};

// ── Router ────────────────────────────────────────────────────────────────

/// Build the axum router for the mirror endpoint.
///
/// The returned `Router` mounts a single WS route at `/ws/mirror`.
/// Wrap in a `tokio::net::TcpListener` + `axum::serve` call in `main`.
pub fn build_mirror_router(worker_tx: DemoWorkerTx) -> Router {
    Router::new()
        .route("/ws/mirror", get(ws_mirror_handler))
        .with_state(Arc::new(MirrorState { worker_tx }))
}

// ── State ─────────────────────────────────────────────────────────────────

struct MirrorState {
    worker_tx: DemoWorkerTx,
}

// ── Upgrade ───────────────────────────────────────────────────────────────

async fn ws_mirror_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<MirrorState>>,
) -> Response {
    ws.on_upgrade(|socket| handle_mirror_socket(socket, state))
}

// ── Main socket loop ──────────────────────────────────────────────────────

async fn handle_mirror_socket(mut socket: WebSocket, state: Arc<MirrorState>) {
    let mirror_id = next_mirror_id();
    let mut ingestor = MirrorIngestor::<DemoCommand>::new(
        state.worker_tx.clone(),
        0,
        default_ingestable,
        true,
    );

    log_connected(mirror_id, ingestor.current_world());

    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("[mirror {}] recv error: {}", mirror_id, e);
                break;
            }
        };

        let data = match msg {
            Message::Binary(b) => b.to_vec(),
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Text(_) => continue,
        };

        // ── Container packets (0x24/0x25/0x3C) ───────────────────────────
        // These use a separate engine command (IngestContainerPacket) and are
        // routed directly here, independently of the main ingestable filter.
        if !data.is_empty() && matches!(data[0], 0x24 | 0x25 | 0x3C) {
            let world = ingestor.current_world();
            let cmd = DemoCommand::Engine(EngineCommand::IngestContainerPacket {
                data: Bytes::copy_from_slice(&data),
            });
            let worker_cmd = WorkerCommand::MapCommand(world, cmd);
            if state.worker_tx.send(worker_cmd).await.is_err() {
                warn!("[mirror {}] worker channel closed (container pkt)", mirror_id);
                break;
            }
        }

        // ── Entity / text packets ─────────────────────────────────────────
        match ingestor.process_packet(data).await {
            Ok(action) => {
                log_action(mirror_id, &action, ingestor.packet_count, ingestor.ingested_count);
            }
            Err(_) => {
                log_send_failed(mirror_id);
                break;
            }
        }
    }

    log_disconnected(mirror_id, ingestor.packet_count, ingestor.ingested_count);
}
