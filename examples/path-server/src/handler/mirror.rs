//! WebSocket mirror endpoint — receives raw S2C UO packets and ingests
//! them into the path-server's shadow world.
//!
//! Protocol:
//!   - Each binary WebSocket frame = one raw S2C UO packet (no framing,
//!     no JSON wrapping).
//!   - `0xBF` sub-command `0x0008` (`SetMap`) switches the current world;
//!     all subsequent entity packets are routed to that world.
//!   - All other recognised packets are forwarded to the worker via
//!     `EngineCommand::IngestPacket { emit_events: true }`.
//!
//! The mirror starts in world 0 (Felucca) by default.
//!
//! The ingestion logic lives in [`common::uo_engine::mirror::MirrorIngestor`];
//! this module is the thin axum WebSocket adapter.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use log::warn;

use common::uo_engine::mirror::{
    default_ingestable, log_action, log_connected, log_disconnected, log_send_failed,
    next_mirror_id, MirrorIngestor,
};

use crate::state::AppState;
use crate::worker::PathServerCommand;

// ── Upgrade ───────────────────────────────────────────────────────────────

pub async fn ws_mirror_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(|socket| handle_mirror_socket(socket, state))
}

// ── Main socket loop ──────────────────────────────────────────────────────

async fn handle_mirror_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mirror_id = next_mirror_id();
    let mut ingestor = MirrorIngestor::<PathServerCommand>::new(
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
