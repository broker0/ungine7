//! Shared packet handlers used by multiple example proxies.

use log::debug;

use u_core::PacketDirection;
use network::handler::packet_handler::{HandlerAction, PacketHandler};
use protocol::RawPacket;

// ── SubcommandFilter ──────────────────────────────────────────────────────

/// Drops General Information packets (0xBF) with subcommand 0xFACE.
///
/// Some servers send `0xBF` with non-standard subcommands that confuse
/// certain clients.  This handler silently drops the offending packets
/// before they reach the client.
#[derive(Debug)]
pub struct SubcommandFilter;

impl PacketHandler for SubcommandFilter {
    fn name(&self) -> &str { "subcmd-filter" }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if packet.id() == 0xBF && packet.len() >= 5 {
            let subcmd = u16::from_be_bytes([packet.data[3], packet.data[4]]);
            if subcmd == 0xFACE {
                debug!("[subcmd-filter] dropped 0xBF sub=0xFACE ({} bytes)", packet.len());
                return HandlerAction::Drop;
            }
        }
        HandlerAction::Forward(packet)
    }
}
