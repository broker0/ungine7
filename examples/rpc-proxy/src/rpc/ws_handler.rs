use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use log::{debug, warn};
use tokio::sync::{mpsc, oneshot};

use crate::registry::{SessionEntry, SharedSessionRegistry};
use crate::rpc::protocol::{ServerMessage, SessionInfo, WsRequest};
use crate::rpc::server::AppState;
use crate::session::commands::ClientCommand;
use crate::types::{SessionId, WsClientId};

// ── WsClientId counter ────────────────────────────────────────────────────

static NEXT_WS_ID: AtomicU64 = AtomicU64::new(1);

fn next_ws_id() -> WsClientId {
    WsClientId(NEXT_WS_ID.fetch_add(1, Ordering::Relaxed))
}

// ── Upgrade ───────────────────────────────────────────────────────────────

pub async fn ws_handler_fn(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state.registry))
}

// ── Main socket loop ──────────────────────────────────────────────────────

async fn handle_socket(mut socket: WebSocket, registry: SharedSessionRegistry) {
    let ws_id = next_ws_id();

    // Channel through which the headless loop pushes ServerMessage events.
    let (ws_tx, mut ws_rx) = mpsc::channel::<ServerMessage>(64);

    // Current attached session entry (set by Attach).
    let mut attached: Option<Arc<SessionEntry>> = None;
    // Whether we have sent AttachWs to the headless loop.
    let mut subscribed = false;

    debug!("[ws {}] connected", ws_id.0);

    loop {
        tokio::select! {
            // ── Incoming from WS client ───────────────────────────────
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else {
                    // Socket closed or error.
                    break;
                };

                let text = match msg.to_text() {
                    Ok(t) => t.to_owned(),
                    Err(_) => continue,  // binary frames ignored
                };

                let req = match serde_json::from_str::<WsRequest>(&text) {
                    Ok(r) => r,
                    Err(e) => {
                        send_error(&mut socket, format!("Invalid request: {e}")).await;
                        continue;
                    }
                };

                handle_request(
                    req,
                    ws_id,
                    &ws_tx,
                    &registry,
                    &mut attached,
                    &mut subscribed,
                    &mut socket,
                )
                .await;
            }

            // ── Outgoing from headless loop ───────────────────────────
            Some(server_msg) = ws_rx.recv() => {
                let text = match serde_json::to_string(&server_msg) {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("[ws {}] serialize error: {}", ws_id.0, e);
                        continue;
                    }
                };
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
        }
    }

    // ── Cleanup ───────────────────────────────────────────────────────
    debug!("[ws {}] disconnected", ws_id.0);
    if subscribed {
        if let Some(entry) = &attached {
            let _ = entry
                .command_tx
                .send(ClientCommand::DetachWs { ws_id })
                .await;
        }
    }
}

// ── Per-request dispatch ──────────────────────────────────────────────────

async fn handle_request(
    req: WsRequest,
    ws_id: WsClientId,
    ws_tx: &mpsc::Sender<ServerMessage>,
    registry: &SharedSessionRegistry,
    attached: &mut Option<Arc<SessionEntry>>,
    subscribed: &mut bool,
    socket: &mut WebSocket,
) {
    match req {
        // ── Session listing ───────────────────────────────────────────

        WsRequest::ListSessions => {
            // Collect entries while holding the read lock, then release it
            // before awaiting the per-entry character_name locks.
            let entries: Vec<Arc<SessionEntry>> = {
                let reg = registry.read().await;
                reg.active_sessions().cloned().collect()
            };
            let mut sessions: Vec<SessionInfo> = Vec::with_capacity(entries.len());
            for e in &entries {
                let character = e.character_name.read().await.clone();
                sessions.push(SessionInfo {
                    id: e.id.0,
                    character,
                    players: 0,
                });
            }
            send_msg(socket, &ServerMessage::Sessions { sessions }).await;
        }

        // ── Attach to a session ───────────────────────────────────────

        WsRequest::Attach { session_id } => {
            let entry: Option<Arc<SessionEntry>> = {
                let reg = registry.read().await;
                reg.get(SessionId(session_id))
            };

            match entry {
                None => {
                    send_error(socket, format!("Session {session_id} not found")).await;
                }
                Some(entry) => {
                    // If we were subscribed to the old session, detach first.
                    if *subscribed {
                        if let Some(old) = attached.as_ref() {
                            let _ = old
                                .command_tx
                                .send(ClientCommand::DetachWs { ws_id })
                                .await;
                        }
                        *subscribed = false;
                    }

                    let char_name = entry
                        .character_name
                        .read()
                        .await
                        .clone()
                        .unwrap_or_else(|| "Unknown".to_string());

                    *attached = Some(entry);

                    send_msg(
                        socket,
                        &ServerMessage::Attached {
                            session_id,
                            character: char_name,
                        },
                    )
                    .await;
                }
            }
        }

        // ── Get state ─────────────────────────────────────────────────

        WsRequest::GetState => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::GetState { reply: reply_tx })
                .await
                .is_ok()
            {
                if let Ok(state) = reply_rx.await {
                    send_msg(socket, &ServerMessage::State { state }).await;
                }
            }
        }

        // ── Subscribe to packet events ────────────────────────────────

        WsRequest::Subscribe { filter } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            // If already subscribed, update the filter (re-attach).
            if *subscribed {
                let _ = entry
                    .command_tx
                    .send(ClientCommand::DetachWs { ws_id })
                    .await;
            }

            let _ = entry
                .command_tx
                .send(ClientCommand::AttachWs {
                    ws_id,
                    sink: ws_tx.clone(),
                    filter,
                })
                .await;

            *subscribed = true;
            send_msg(socket, &ServerMessage::Pong).await;
        }

        // ── Unsubscribe from packet events ────────────────────────────

        WsRequest::Unsubscribe => {
            if *subscribed {
                if let Some(entry) = attached.as_ref() {
                    let _ = entry
                        .command_tx
                        .send(ClientCommand::DetachWs { ws_id })
                        .await;
                }
                *subscribed = false;
            }
            send_msg(socket, &ServerMessage::Pong).await;
        }

        // ── Get items ─────────────────────────────────────────────────

        WsRequest::GetItems => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::GetItems { reply: reply_tx })
                .await
                .is_ok()
            {
                if let Ok(items) = reply_rx.await {
                    send_msg(socket, &ServerMessage::Items { items }).await;
                }
            }
        }

        // ── Get all mobiles ───────────────────────────────────────────

        WsRequest::GetMobiles => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::GetMobiles { reply: reply_tx })
                .await
                .is_ok()
            {
                if let Ok(mobiles) = reply_rx.await {
                    send_msg(socket, &ServerMessage::Mobiles { mobiles }).await;
                }
            }
        }

        // ── Get single mobile ─────────────────────────────────────────

        WsRequest::GetMobile { serial } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::GetMobile { serial, reply: reply_tx })
                .await
                .is_ok()
            {
                if let Ok(mobile) = reply_rx.await {
                    send_msg(socket, &ServerMessage::Mobile { mobile }).await;
                }
            }
        }

        // ── Get equipment of a mobile ─────────────────────────────────

        WsRequest::GetEquipment { serial } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::GetEquipment { serial, reply: reply_tx })
                .await
                .is_ok()
            {
                if let Ok(equipment) = reply_rx.await {
                    send_msg(socket, &ServerMessage::Equipment { serial, equipment }).await;
                }
            }
        }

        // ── Use object (double-click) ──────────────────────────────────

        WsRequest::UseObject { serial } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::UseObject { serial, reply: reply_tx })
                .await
                .is_ok()
            {
                if reply_rx.await.is_ok() {
                    send_msg(socket, &ServerMessage::Used { serial }).await;
                }
            }
        }

        // ── Step in a direction ───────────────────────────────────────

        WsRequest::Step { heading, raw } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            if heading >= 8 {
                send_error(socket, format!("Invalid heading {heading}: must be 0–7")).await;
                return;
            }

            let (reply_tx, reply_rx) = oneshot::channel();
            if entry
                .command_tx
                .send(ClientCommand::Step {
                    heading,
                    raw: raw.unwrap_or(false),
                    reply: reply_tx,
                })
                .await
                .is_ok()
            {
                if let Ok(queued) = reply_rx.await {
                    send_msg(socket, &ServerMessage::Stepped { heading, blocked: !queued }).await;
                }
            }
        }

        // ── Inject raw packet ─────────────────────────────────────────

        WsRequest::InjectPacket { hex } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            match decode_hex(&hex) {
                Ok(data) => {
                    use u_core::PacketDirection;
                    use protocol::RawPacket;

                    let pkt = RawPacket::new(data.into(), PacketDirection::ClientToServer);
                    let _ = entry
                        .command_tx
                        .send(ClientCommand::RawPacket {
                            client_id: 0,
                            data: pkt,
                        })
                        .await;
                    send_msg(socket, &ServerMessage::Pong).await;
                }
                Err(e) => {
                    send_error(socket, format!("Invalid hex: {e}")).await;
                }
            }
        }

        // ── Ping ──────────────────────────────────────────────────────

        WsRequest::Ping => {
            send_msg(socket, &ServerMessage::Pong).await;
        }

        // ── Lua scripting ─────────────────────────────────────────────

        #[cfg(feature = "lua")]
        WsRequest::RunScript { code } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            if entry
                .lua_cmd_tx
                .send(crate::lua_script::LuaCommand::RunCode { code, reply: reply_tx })
                .await
                .is_ok()
            {
                match reply_rx.await {
                    Ok(Ok(())) => send_msg(socket, &ServerMessage::ScriptStarted).await,
                    Ok(Err(e)) => send_msg(socket, &ServerMessage::ScriptError { message: e }).await,
                    Err(_) => send_error(socket, "Lua manager channel closed".into()).await,
                }
            } else {
                send_error(socket, "Lua manager channel closed".into()).await;
            }
        }

        #[cfg(feature = "lua")]
        WsRequest::RunScriptFile { path } => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            if entry
                .lua_cmd_tx
                .send(crate::lua_script::LuaCommand::RunFile(std::path::PathBuf::from(path)))
                .await
                .is_ok()
            {
                send_msg(socket, &ServerMessage::ScriptStarted).await;
            } else {
                send_error(socket, "Lua manager channel closed".into()).await;
            }
        }

        #[cfg(feature = "lua")]
        WsRequest::StopScript => {
            let Some(entry) = attached.as_ref() else {
                send_error(socket, "Not attached to a session".into()).await;
                return;
            };

            if entry
                .lua_cmd_tx
                .send(crate::lua_script::LuaCommand::Stop)
                .await
                .is_ok()
            {
                send_msg(socket, &ServerMessage::ScriptStopped).await;
            } else {
                send_error(socket, "Lua manager channel closed".into()).await;
            }
        }
    }
}

// ── Send helpers ──────────────────────────────────────────────────────────

async fn send_msg(socket: &mut WebSocket, msg: &ServerMessage) {
    if let Ok(text) = serde_json::to_string(msg) {
        let _ = socket.send(Message::Text(text)).await;
    }
}

async fn send_error(socket: &mut WebSocket, message: String) {
    send_msg(socket, &ServerMessage::Error { message }).await;
}

// ── Hex decode ────────────────────────────────────────────────────────────

/// Simple hex string decoder (avoids external `hex` crate dependency).
fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd length".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
