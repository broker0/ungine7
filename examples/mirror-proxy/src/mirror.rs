//! Outgoing WebSocket mirror — streams raw S2C UO packets to an external
//! mirror endpoint (e.g. path-server's `/ws/mirror`).
//!
//! Two pieces work together:
//!
//!   - [`spawn_mirror_streamer`] connects to the mirror URL via WebSocket and
//!     runs a background task that drains an mpsc channel, forwarding every
//!     buffer as a binary WebSocket frame.
//!   - [`MirrorTap`] is a [`PacketHandler`] inserted into the proxy's
//!     server-inbound chain.  It copies each S2C packet into the channel and
//!     forwards the packet onward to the client unchanged.
//!
//! There is no reconnect logic: if the mirror endpoint disconnects, packets
//! are silently dropped until the proxy session ends.

use bytes::Bytes;
use log::{error, info, warn};
use tokio::sync::mpsc;

use u_core::PacketDirection;

use network::handler::packet_handler::{HandlerAction, PacketHandler};
use protocol::RawPacket;

/// Bound on the in-flight packet queue between the relay and the WS streamer.
///
/// If the remote endpoint is slow and the queue fills up, further packets are
/// dropped (with a warning) rather than stalling the relay loop.
const CHANNEL_CAPACITY: usize = 1024;

/// Spawn the background WebSocket streaming task.
///
/// Connects to `mirror_url` and returns an mpsc sender.  Anything pushed into
/// the sender is forwarded to the mirror endpoint as a binary frame.  The task
/// runs until the channel is closed (proxy session ended) or the remote
/// endpoint disconnects.
pub fn spawn_mirror_streamer(tag: String, mirror_url: String) -> mpsc::Sender<Bytes> {
    let (tx, rx) = mpsc::channel::<Bytes>(CHANNEL_CAPACITY);
    tokio::spawn(run_mirror_streamer(tag, mirror_url, rx));
    tx
}

async fn run_mirror_streamer(tag: String, mirror_url: String, mut rx: mpsc::Receiver<Bytes>) {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    info!("{tag} mirror: connecting to {mirror_url}");

    let ws_stream = match tokio_tungstenite::connect_async(&mirror_url).await {
        Ok((stream, response)) => {
            info!("{tag} mirror: connected (status {})", response.status());
            stream
        }
        Err(e) => {
            error!("{tag} mirror: failed to connect to {mirror_url}: {e}");
            return;
        }
    };

    let (mut ws_sink, _ws_read) = {
        use futures_util::StreamExt;
        ws_stream.split()
    };

    let mut forwarded: u64 = 0;

    while let Some(data) = rx.recv().await {
        let msg = Message::Binary(data.to_vec().into());
        if let Err(e) = ws_sink.send(msg).await {
            warn!("{tag} mirror: send failed after {forwarded} packets: {e}");
            break;
        }

        forwarded += 1;
        if forwarded % 5000 == 0 {
            info!("{tag} mirror: {forwarded} packets forwarded to {mirror_url}");
        }
    }

    // Close the WebSocket gracefully.
    let _ = ws_sink.close().await;

    info!("{tag} mirror: disconnected ({forwarded} packets forwarded)");
}

// ── MirrorTap ─────────────────────────────────────────────────────────────

/// [`PacketHandler`] that copies every S2C packet into the mirror channel and
/// forwards it onward unchanged.
///
/// Placed in the proxy's server-inbound handler chain.  Never blocks the relay
/// loop: it uses [`mpsc::Sender::try_send`], so a full or closed channel simply
/// drops the packet (logged at most once per state change).
#[derive(Debug)]
pub struct MirrorTap {
    tag: String,
    tx: mpsc::Sender<Bytes>,
    /// When `true`, 0x1D DeleteObject packets are not mirrored (objects
    /// accumulate in the shadow world instead of despawning when they leave
    /// the player's view).  See [`MirrorTap::new`].
    block_delete: bool,
    /// Whether we've already warned that the channel is closed/full, to avoid
    /// log spam.
    warned: bool,
}

impl MirrorTap {
    /// Create a new mirror tap.
    ///
    /// `block_delete` controls whether 0x1D DeleteObject packets are excluded
    /// from the mirror stream.  When `false` (default behaviour) every S2C
    /// packet is mirrored, matching `rpc-proxy`.  When `true`, 0x1D is dropped
    /// from the mirror stream so objects accumulate on the mirror endpoint;
    /// the packet is still forwarded to the real client unchanged.
    pub fn new(tag: impl Into<String>, tx: mpsc::Sender<Bytes>, block_delete: bool) -> Self {
        Self {
            tag: tag.into(),
            tx,
            block_delete,
            warned: false,
        }
    }
}

impl PacketHandler for MirrorTap {
    fn name(&self) -> &str {
        "mirror-tap"
    }

    fn handle(&mut self, dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        // Only mirror server-to-client packets.
        //
        // When `block_delete` is enabled, 0x1D DeleteObject is NOT mirrored:
        // the UO server sends it both for real removals (item picked up /
        // destroyed) and for objects merely leaving the client's view range.
        // Forwarding it would make the mirror endpoint (path-server) despawn
        // objects as the player walks away.  Dropping it from the mirror
        // stream lets once-seen objects accumulate in the shadow world.  The
        // packet is still forwarded to the real client unchanged (see the
        // return below), so the client's own world view is unaffected.
        let pkt_id = packet.data.first().copied().unwrap_or(0);
        let skip = self.block_delete && pkt_id == 0x1D;
        if dir == PacketDirection::ServerToClient && !skip {
            match self.tx.try_send(packet.data.clone()) {
                Ok(()) => {
                    self.warned = false;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    if !self.warned {
                        warn!("{} mirror: channel full, dropping packets", self.tag);
                        self.warned = true;
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    if !self.warned {
                        warn!("{} mirror: channel closed, mirror inactive", self.tag);
                        self.warned = true;
                    }
                }
            }
        }

        HandlerAction::Forward(packet)
    }
}
