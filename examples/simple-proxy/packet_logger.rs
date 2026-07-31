//! Packet-aware logger handler.
//!
//! Uses [`packets::registry::PacketRegistry`] to parse known UO packets
//! and log structured info via their `Debug` representation. Unknown packets
//! fall back to id + size. Decode errors are logged as warnings.

use log::{debug, trace, warn};

use u_core::PacketDirection;
use network::handler::packet_handler::{HandlerAction, PacketHandler};
use protocol::RawPacket;

use packets::registry::{DecodedResult, OutputFormat, PacketRegistry};

const TARGET: &str = "packet_logger";

/// A handler that parses known packets and logs structured details.
#[derive(Debug)]
pub struct PacketLogger {
    label: String,
    registry: PacketRegistry,
}

impl PacketLogger {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            registry: PacketRegistry::default(),
        }
    }
}

impl PacketHandler for PacketLogger {
    fn name(&self) -> &str { &self.label }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        let data = &packet.data;
        let dir = packet.direction;
        let id = packet.id();
        let len = packet.len();

        match self.registry.decode(id, data, dir, OutputFormat::Debug) {
            DecodedResult::Ok(decoded) => {
                debug!(target: TARGET, "[{}] {} 0x{:02X} ({} bytes) — {}",
                    self.label, dir, id, len, decoded);
            }
            DecodedResult::DecodeError(err) => {
                warn!(target: TARGET, "[{}] {} 0x{:02X} ({} bytes) — decode error: {}",
                    self.label, dir, id, len, err);
                trace!(target: TARGET, "[{}] raw: {:02X?}", self.label, data);
            }
            DecodedResult::Unknown => {
                debug!(target: TARGET, "[{}] {} 0x{:02X} ({} bytes)",
                    self.label, dir, id, len);
            }
        }

        HandlerAction::Forward(packet)
    }
}
