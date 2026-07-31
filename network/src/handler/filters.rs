use protocol::RawPacket;
use crate::logs;
use log::{debug, trace};
use u_core::PacketDirection;
use crate::handler::packet_handler::{HandlerAction, PacketHandler};

/// Logger — logs every packet passing through, forwards all.
#[derive(Debug, Clone)]
pub struct LogHandler {
    label: String,
}

impl LogHandler {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into() }
    }
}

impl PacketHandler for LogHandler {
    fn name(&self) -> &str { &self.label }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        debug!(target: logs::FILTER, "[{}] {} 0x{:02X} ({} bytes)",
            self.label, packet.direction, packet.id(), packet.len());
        trace!(target: logs::FILTER, "[{}] full packet: {:02X?}", self.label, packet.data);
        HandlerAction::Forward(packet)
    }
}

/// Drops packets that match a specific packet ID and subcommand value.
///
/// Some servers send `0xBF` (General Information) with non-standard
/// subcommands that confuse certain clients.  This handler silently drops
/// the offending packets before they reach the client.
///
/// The subcommand is read as a big-endian `u16` from bytes 3..5 of the
/// packet data.
///
/// # Example
///
/// ```rust
/// use network::handler::filters::SubcommandFilter;
///
/// // Drop 0xBF packets with subcommand 0xFACE (the default):
/// let filter = SubcommandFilter::default();
///
/// // Drop 0xBF packets with subcommands 0xFACE and 0xBEEF:
/// let filter = SubcommandFilter::new(0xBF, vec![0xFACE, 0xBEEF]);
/// ```
#[derive(Debug, Clone)]
pub struct SubcommandFilter {
    packet_id: u8,
    blocked: Vec<u16>,
}

impl SubcommandFilter {
    /// Create a filter for a specific packet ID and list of blocked subcommands.
    pub fn new(packet_id: u8, blocked: Vec<u16>) -> Self {
        Self { packet_id, blocked }
    }
}

impl Default for SubcommandFilter {
    /// Default filter: drops `0xBF` packets with subcommand `0xFACE`.
    fn default() -> Self {
        Self { packet_id: 0xBF, blocked: vec![0xFACE] }
    }
}

impl PacketHandler for SubcommandFilter {
    fn name(&self) -> &str { "subcmd-filter" }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if packet.id() == self.packet_id && packet.len() >= 5 {
            let subcmd = u16::from_be_bytes([packet.data[3], packet.data[4]]);
            if self.blocked.contains(&subcmd) {
                debug!(target: logs::FILTER,
                    "[subcmd-filter] dropped 0x{:02X} sub=0x{:04X} ({} bytes)",
                    self.packet_id, subcmd, packet.len());
                return HandlerAction::Drop;
            }
        }
        HandlerAction::Forward(packet)
    }
}
