use u_core::PacketDirection;
use protocol::RawPacket;
use std::fmt::Debug;

#[derive(Debug)]
pub enum HandlerAction {
    Forward(RawPacket),
    Drop,
    Replace(Vec<RawPacket>),
    Stop(RawPacket),
    StopDrop,
    /// Forward the packet onward and also send reply packet(s) back
    /// to the source (opposite direction).
    ForwardAndReply { forward: RawPacket, reply: Vec<RawPacket> },
    /// Drop the packet but send reply packet(s) back to the source
    /// (opposite direction).  Useful for auto-responding (e.g. ping → pong)
    /// without forwarding the original packet.
    Reply(Vec<RawPacket>),
}

pub trait PacketHandler: Send + Debug {
    fn name(&self) -> &str;
    fn handle(&mut self, dir: PacketDirection, packet: RawPacket) -> HandlerAction;
    fn on_start(&mut self) {}
    fn on_close(&mut self) {}
}
