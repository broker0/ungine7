//! Packet observer handler.
//!
//! Uses [`packets::registry::PacketRegistry`] to parse known UO packets
//! and pushes structured [`PacketEntry`] values into the [`SessionRegistry`]
//! (ring buffer + broadcast channel) instead of writing to the log.
//!
//! Unknown packets fall back to a hex dump of the raw bytes.
//! The registry takes care of fan-out to all connected WebSocket subscribers.
//!
//! As a side effect, when packet 0x80 (`AccountLogin`) or 0x91 (`GameLogin`)
//! is seen, the account name is extracted and stored in the [`SessionRegistry`]
//! so it can be displayed in the web UI.

use std::sync::Arc;

use u_core::PacketDirection::{self, ClientToServer as C2S, ServerToClient as S2C};
use network::handler::packet_handler::{HandlerAction, PacketHandler};
use protocol::RawPacket;

use packets::login::{AccountLogin, GameLogin, LoginCharacter};
use packets::registry::{DecodedResult, OutputFormat, PacketRegistry};
use packets::traits::BasicPacket;

use crate::session_registry::{now_ms, PacketEntry, SessionId, SessionRegistry};

// ── PacketObserver ────────────────────────────────────────────────────────

/// A `PacketHandler` that records every packet to the shared `SessionRegistry`
/// and forwards it unchanged.
#[derive(Debug)]
pub struct PacketObserver {
    session_id: SessionId,
    registry: Arc<SessionRegistry>,
    packet_registry: PacketRegistry,
}

impl PacketObserver {
    pub fn new(session_id: SessionId, registry: Arc<SessionRegistry>) -> Self {
        Self {
            session_id,
            registry,
            packet_registry: PacketRegistry::default(),
        }
    }
}

impl PacketHandler for PacketObserver {
    fn name(&self) -> &str {
        "packet-observer"
    }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        let id = packet.id();
        let len = packet.len();
        let dir = packet.direction;

        let direction_str = match dir {
            C2S => "C\u{2192}S",
            S2C => "S\u{2192}C",
        };

        let hex = packet
            .data
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        let desc = match self.packet_registry.decode(id, &packet.data, dir, OutputFormat::Debug) {
            DecodedResult::Ok(decoded) => decoded.into_string(),
            DecodedResult::DecodeError(e) => format!("[decode error: {e}] {hex}"),
            DecodedResult::Unknown => hex.clone(),
        };

        let entry = PacketEntry {
            timestamp: now_ms(),
            direction: direction_str.to_string(),
            id: format!("0x{id:02X}"),
            len,
            desc,
            hex,
        };

        self.registry.push_packet(self.session_id, entry);

        // Sniff the account name from the first login packet seen.
        if dir == C2S {
            match id {
                AccountLogin::ID => {
                    if let Ok(pkt) = AccountLogin::from_bytes(&packet.data) {
                        let name = (*pkt.account).trim_end_matches('\0').to_string();
                        if !name.is_empty() {
                            self.registry.set_account(self.session_id, name);
                        }
                    }
                }
                GameLogin::ID => {
                    if let Ok(pkt) = GameLogin::from_bytes(&packet.data) {
                        let name = (*pkt.account).trim_end_matches('\0').to_string();
                        if !name.is_empty() {
                            self.registry.set_account(self.session_id, name);
                        }
                    }
                }
                LoginCharacter::ID => {
                    if let Ok(pkt) = LoginCharacter::from_bytes(&packet.data) {
                        let name = (*pkt.name).trim_end_matches('\0').to_string();
                        if !name.is_empty() {
                            self.registry.set_character(self.session_id, name);
                        }
                    }
                }
                _ => {}
            }
        }

        HandlerAction::Forward(packet)
    }
}
