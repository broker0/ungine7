//! Typed commands sent to [`HeadlessClient`](super::headless) by
//! [`VirtualClient`](super::virtual_client)s, WebSocket handlers, and
//! bot logic.
//!
//! All interaction with the headless session goes through a single
//! `mpsc::Sender<ClientCommand>` channel — there is no shared mutable
//! state between the headless loop and its consumers.

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use framework::rythmos::ClientId;
use protocol::RawPacket;

use crate::rpc::protocol::{EquippedItemEntry, ServerMessage, WorldItemData, WorldMobileData};
use crate::types::{FullSessionState, WsClientId};

/// A command destined for the [`HeadlessClient`](super::headless) main loop.
#[derive(Debug)]
pub enum ClientCommand {
    // ── UO client management ─────────────────────────────────────────

    /// Raw UO packet from a connected client (C→S).
    /// HeadlessClient parses and dispatches to the right manager.
    RawPacket {
        client_id: ClientId,
        data: RawPacket,
    },

    /// A new UO client has finished login/bootstrap and is ready
    /// to participate in the session.  `sink` receives per-client
    /// packets (arbiter responses, etc.).
    AttachClient {
        client_id: ClientId,
        sink: mpsc::Sender<RawPacket>,
    },

    /// A UO client has disconnected.
    DetachClient {
        client_id: ClientId,
    },

    // ── Typed commands (WS / bot) ────────────────────────────────────

    /// Query full session state (position, character, world).
    GetState {
        reply: oneshot::Sender<FullSessionState>,
    },

    /// Request world-bootstrap packets for a newly-connected Mirror.
    ///
    /// The headless loop generates them from its local [`framework::diorama::ObserverPipeline`]
    /// and replies with a ready-to-send packet vector.
    GetBootstrap {
        reply: oneshot::Sender<Vec<RawPacket>>,
    },

    /// Request the cached `0xB9 EnableFeatures` packet (if any).
    ///
    /// Used by [`super::virtual_client::LoginMode::JoinExisting`] clients during the login handshake —
    /// the UO client expects `0xB9` before `0xA9 CharacterList`.
    GetEnableFeatures {
        reply: oneshot::Sender<Option<Bytes>>,
    },

    /// Request a snapshot of all items (non-mobiles) in the visible set.
    GetItems {
        reply: oneshot::Sender<Vec<WorldItemData>>,
    },

    /// Request a snapshot of all mobiles in the visible set.
    GetMobiles {
        reply: oneshot::Sender<Vec<WorldMobileData>>,
    },

    /// Request data for a single mobile by serial.
    GetMobile {
        serial: u32,
        reply:  oneshot::Sender<Option<WorldMobileData>>,
    },

    /// Request the equipment list of a mobile by serial.
    GetEquipment {
        serial: u32,
        reply:  oneshot::Sender<Vec<EquippedItemEntry>>,
    },

    /// Send a `0x06 DoubleClick` packet for the given serial.
    UseObject {
        serial: u32,
        reply:  oneshot::Sender<()>,
    },

    /// Execute one bot step in the given direction.
    ///
    /// `heading`: 0=N 1=NE 2=E 3=SE 4=S 5=SW 6=W 7=NW (matches `Heading` repr).
    /// `raw`: if true, skip passability validation.
    /// Reply value: `true` = step queued, `false` = blocked.
    Step {
        heading: u8,
        raw:     bool,
        reply:   oneshot::Sender<bool>,
    },

    // ── WS management ────────────────────────────────────────────────

    /// Attach a WebSocket observer.
    ///
    /// `filter`: packet IDs to forward to this observer.
    /// `None` means all packets; `Some([])` means no packets (effectively
    /// paused); `Some([0x78, 0x20])` means only those IDs.
    AttachWs {
        ws_id:  WsClientId,
        sink:   mpsc::Sender<ServerMessage>,
        filter: Option<Vec<u8>>,
    },

    /// Detach a WebSocket observer.
    DetachWs {
        ws_id: WsClientId,
    },
}
