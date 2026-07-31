use protocol::RawPacket;
use crate::logs;
use log::trace;

pub mod packet_handler;
pub mod filters;
pub mod redirect;

use packet_handler::{HandlerAction, PacketHandler};
use u_core::PacketDirection;

/// Aggregate result after running a packet through the entire handler chain.
#[derive(Debug)]
pub struct HandlerResult {
    /// Packets to pass further in the original direction.
    pub forward: Vec<RawPacket>,
    /// Packets to send back to the source (opposite direction).
    pub replies: Vec<RawPacket>,
    /// Whether a handler requested the session to stop.
    pub stop: bool,
}

impl HandlerResult {
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty() && self.replies.is_empty()
    }
}

#[derive(Debug)]
pub struct HandlerChain {
    handlers: Vec<Box<dyn PacketHandler>>,
}

impl HandlerChain {
    pub fn new() -> Self {
        Self { handlers: Vec::new() }
    }

    pub fn add(&mut self, handler: Box<dyn PacketHandler>) {
        self.handlers.push(handler);
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Run a packet through all handlers in order.
    ///
    /// Each handler sees the output(s) of the previous one.
    /// Drop / StopDrop can suppress the packet entirely.
    /// Replace can turn one packet into many.
    /// Stop yields the packet but signals the session to shut down.
    /// Reply / ForwardAndReply collect packets to send back to the source.
    pub fn process(&mut self, packet: RawPacket, dir: PacketDirection) -> HandlerResult {
        // current set of packets flowing through the chain
        let mut packets = vec![packet];
        let mut should_stop = false;
        let mut replies = Vec::new();

        for handler in &mut self.handlers {
            let mut next = Vec::with_capacity(packets.len());

            for p in packets {
                let action = handler.handle(dir, p);
                match action {
                    HandlerAction::Forward(p) => next.push(p),
                    HandlerAction::Drop => {
                        trace!(target: logs::HANDLER, "handler '{}' dropped packet", handler.name());
                    }
                    HandlerAction::Replace(mut replacements) => {
                        trace!(target: logs::HANDLER,
                            "handler '{}' replaced packet with {} packet(s)",
                            handler.name(), replacements.len());
                        next.append(&mut replacements);
                    }
                    HandlerAction::Stop(p) => {
                        trace!(target: logs::HANDLER,
                            "handler '{}' stopped session (forwarding packet)", handler.name());
                        next.push(p);
                        should_stop = true;
                    }
                    HandlerAction::StopDrop => {
                        trace!(target: logs::HANDLER,
                            "handler '{}' stopped session (dropping packet)", handler.name());
                        should_stop = true;
                    }
                    HandlerAction::ForwardAndReply { forward, reply } => {
                        trace!(target: logs::HANDLER,
                            "handler '{}' forwarded packet and queued {} reply(s)",
                            handler.name(), reply.len());
                        next.push(forward);
                        replies.extend(reply);
                    }
                    HandlerAction::Reply(reply) => {
                        trace!(target: logs::HANDLER,
                            "handler '{}' dropped packet and queued {} reply(s)",
                            handler.name(), reply.len());
                        replies.extend(reply);
                    }
                }
            }

            packets = next;

            if should_stop {
                break;
            }
        }

        HandlerResult {
            forward: packets,
            replies,
            stop: should_stop,
        }
    }

    /// Notify all handlers that the session has started.
    pub fn notify_start(&mut self) {
        for handler in &mut self.handlers {
            handler.on_start();
        }
    }

    /// Notify all handlers that the session is closing.
    pub fn notify_close(&mut self) {
        for handler in &mut self.handlers {
            handler.on_close();
        }
    }
}

impl Default for HandlerChain {
    fn default() -> Self {
        Self::new()
    }
}
