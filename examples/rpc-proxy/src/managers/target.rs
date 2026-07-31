//! Target cursor manager.
//!
//! Tracks the server's active target cursor request and arbitrates
//! responses from multiple connected clients.  When one client submits
//! a target response, all other clients receive a cancel.

use log::debug;

use framework::rythmos::ClientId;
use packets::interaction::TargetCursor;
use packets::traits::BasicPacket;
use protocol::RawPacket;

// ── Result types ──────────────────────────────────────────────────────────

/// Result of processing an S→C target cursor packet from the server.
pub enum TargetServerResult {
    /// New target request — broadcast to all clients.
    Request,
    /// Server cancelled the target — broadcast cancel to all clients.
    Cancel,
}

/// Result of processing a C→S target cursor response from a client.
pub enum TargetSubmitResult {
    /// Response accepted — forward to server, cancel for other clients.
    Accepted {
        to_server: RawPacket,
        cancel_others: Vec<(ClientId, RawPacket)>,
    },
    /// Stale or mismatched — drop silently.
    Stale,
}

// ── TargetManager ─────────────────────────────────────────────────────────

/// Manages target cursor state for a session.
///
/// Pure logic — no I/O, no async.  The caller (HeadlessClient) is
/// responsible for delivering packets to clients and the server.
pub struct TargetManager {
    /// Cursor ID of the currently active target request, if any.
    pending: Option<u32>,
    /// Connected client IDs.
    clients: Vec<ClientId>,
}

impl TargetManager {
    pub fn new() -> Self {
        Self {
            pending: None,
            clients: Vec::new(),
        }
    }

    /// Returns the pending cursor ID, if any.
    pub fn pending_cursor_id(&self) -> Option<u32> {
        self.pending
    }

    /// Server sent a target cursor packet (S→C 0x6C).
    pub fn on_server_packet(&mut self, data: &[u8]) -> Option<TargetServerResult> {
        let pkt = match TargetCursor::from_bytes(data) {
            Ok(p) => p,
            Err(_) => {
                log::warn!("[target] failed to parse S→C TargetCursor");
                return None;
            }
        };

        if pkt.cursor_type == 3 {
            // Server cancels the target cursor.
            debug!(
                "[target] S→C cancel: cursor_id=0x{:08X}",
                pkt.cursor_id
            );
            self.pending = None;
            Some(TargetServerResult::Cancel)
        } else {
            // Server requests a target cursor.
            debug!(
                "[target] S→C request: cursor_id=0x{:08X} cursor_target={} cursor_type={}",
                pkt.cursor_id, pkt.cursor_target, pkt.cursor_type
            );
            self.pending = Some(pkt.cursor_id);
            Some(TargetServerResult::Request)
        }
    }

    /// Client submitted a target response (C→S 0x6C).
    pub fn on_client_response(&mut self, client_id: ClientId, data: &[u8]) -> TargetSubmitResult {
        let pkt = match TargetCursor::from_bytes(data) {
            Ok(p) => p,
            Err(_) => {
                log::warn!("[target] failed to parse C→S TargetCursor");
                return TargetSubmitResult::Stale;
            }
        };

        let Some(pending_id) = self.pending else {
            debug!(
                "[target] C→S from client {} dropped: no pending target",
                client_id
            );
            return TargetSubmitResult::Stale;
        };

        if pkt.cursor_id != pending_id {
            debug!(
                "[target] C→S from client {} dropped: cursor_id mismatch (got 0x{:08X}, expected 0x{:08X})",
                client_id, pkt.cursor_id, pending_id
            );
            return TargetSubmitResult::Stale;
        }

        debug!(
            "[target] C→S from client {}: cursor_id=0x{:08X} target_serial=0x{:08X}",
            client_id, pkt.cursor_id, pkt.target_serial
        );

        // Clear pending.
        self.pending = None;

        // Forward the response to the server.
        let to_server = RawPacket::c2s(data.to_vec().into());

        // Send cancel to ALL OTHER clients.
        let cancel = Self::cancel_packet(pending_id);
        let cancel_others: Vec<_> = self
            .clients
            .iter()
            .filter(|&&cid| cid != client_id)
            .map(|&cid| (cid, cancel.clone()))
            .collect();

        TargetSubmitResult::Accepted {
            to_server,
            cancel_others,
        }
    }

    pub fn attach_client(&mut self, client_id: ClientId) {
        if !self.clients.contains(&client_id) {
            self.clients.push(client_id);
        }
    }

    pub fn detach_client(&mut self, client_id: ClientId) {
        self.clients.retain(|&c| c != client_id);
    }

    /// Build a cancel-target packet (cursor_type = 3) for a given cursor_id.
    fn cancel_packet(cursor_id: u32) -> RawPacket {
        let cancel = TargetCursor {
            id: TargetCursor::ID,
            cursor_target: 0,
            cursor_id,
            cursor_type: 3, // Cancel
            target_serial: 0,
            x: 0,
            y: 0,
            _pad0: (),
            z: 0,
            graphic: 0,
        };
        RawPacket::s2c(cancel.to_bytes())
    }
}
