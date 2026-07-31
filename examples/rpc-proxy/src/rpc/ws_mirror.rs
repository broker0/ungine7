//! Outgoing WebSocket mirror client — streams raw S2C UO packets to an
//! external mirror endpoint (e.g. path-server's `/ws/mirror`).
//!
//! Spawned as a background task by the headless session when
//! `--mirror-url` is configured.  Subscribes to the session's
//! `packet_tx` broadcast and forwards every S2C packet as a binary
//! WebSocket frame.
//!
//! The task runs until the broadcast channel is closed (headless session
//! ends) or the remote endpoint disconnects.

use std::sync::Arc;

use log::{info, warn, error};
use tokio::sync::broadcast;
use u_core::PacketDirection;

use crate::registry::SessionEntry;
use crate::types::SessionId;

/// Spawn a mirror streaming task.
///
/// Connects to `mirror_url` via WebSocket and forwards all S2C packets
/// from `entry.packet_tx` as binary frames.  Returns a `JoinHandle`
/// so the caller can track the task's lifetime (though it is fire-and-forget
/// in practice).
pub fn spawn_mirror_task(
    entry: Arc<SessionEntry>,
    session_id: SessionId,
    mirror_url: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_mirror_client(entry, session_id, mirror_url))
}

async fn run_mirror_client(
    entry: Arc<SessionEntry>,
    session_id: SessionId,
    mirror_url: String,
) {
    use tokio_tungstenite::tungstenite::Message;

    info!(
        "[session {}] mirror: connecting to {}",
        session_id.0, mirror_url,
    );

    let ws_stream = match tokio_tungstenite::connect_async(&mirror_url).await {
        Ok((stream, response)) => {
            info!(
                "[session {}] mirror: connected (status {})",
                session_id.0,
                response.status(),
            );
            stream
        }
        Err(e) => {
            error!(
                "[session {}] mirror: failed to connect to {}: {}",
                session_id.0, mirror_url, e,
            );
            return;
        }
    };

    let (mut ws_sink, _ws_read) = {
        use futures_util::StreamExt;
        ws_stream.split()
    };

    let mut packet_rx: broadcast::Receiver<crate::types::PacketFrame> =
        entry.packet_tx.subscribe();

    let mut forwarded: u64 = 0;

    loop {
        match packet_rx.recv().await {
            Ok(frame) => {
                if frame.direction != PacketDirection::ServerToClient {
                    continue;
                }

                use futures_util::SinkExt;
                let msg = Message::Binary(frame.data.to_vec().into());
                if let Err(e) = ws_sink.send(msg).await {
                    warn!(
                        "[session {}] mirror: send failed after {} packets: {}",
                        session_id.0, forwarded, e,
                    );
                    break;
                }

                forwarded += 1;
                if forwarded % 5000 == 0 {
                    info!(
                        "[session {}] mirror: {} packets forwarded to {}",
                        session_id.0, forwarded, mirror_url,
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    "[session {}] mirror: lagged, skipped {} packets",
                    session_id.0, n,
                );
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!(
                    "[session {}] mirror: session broadcast closed",
                    session_id.0,
                );
                break;
            }
        }
    }

    // Close the WebSocket gracefully.
    {
        use futures_util::SinkExt;
        let _ = ws_sink.close().await;
    }

    info!(
        "[session {}] mirror: disconnected ({} packets forwarded)",
        session_id.0, forwarded,
    );
}
