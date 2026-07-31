use async_trait::async_trait;
use protocol::RawPacket;
use protocol::transport::{PacketTransport, TransportError, TransportEvent};
use protocol::prelude::{BasicPacket, encode_packet};
use tokio::sync::mpsc;
use crate::error;
use crate::logs;
use log::{debug, trace};
use u_core::PacketDirection;
use crate::handler::{HandlerChain, HandlerResult};
use crate::handler::packet_handler::PacketHandler;

/// Events produced by [`Session::recv`].
#[derive(Debug)]
pub enum SessionEvent {
    Seed(bytes::Bytes),
    Packet(RawPacket),
    Disconnected,
    /// A handler requested the session to stop.
    Stopped,
    Error(TransportError),
}

/// Result from [`Session::recv`] that may include reply packets.
///
/// When a handler returns [`Reply`](crate::handler::packet_handler::HandlerAction::Reply)
/// or [`ForwardAndReply`](crate::handler::packet_handler::HandlerAction::ForwardAndReply),
/// the reply packets are collected here.  The caller (typically the relay loop)
/// is responsible for sending them back to the source transport.
#[derive(Debug)]
pub struct RecvResult {
    pub event: SessionEvent,
    /// Packets to send back to the source (opposite direction).
    /// Empty when no handler produced replies.
    pub replies: Vec<RawPacket>,
}

/// Result from [`Session::send`] that may include reply packets.
///
/// When an outbound handler produces replies, they should be sent
/// back through the inbound path of the same session (i.e. back to
/// whichever side originally received this packet).
#[derive(Debug)]
pub struct SendResult {
    /// Packets to send back to the source (opposite direction).
    pub replies: Vec<RawPacket>,
}

/// High-level session: transport + direction + handler chains.
///
/// Wraps a [`PacketTransport`] and applies inbound/outbound handler
/// pipelines to every packet flowing through.
pub struct Session {
    transport: Box<dyn PacketTransport>,
    direction: PacketDirection,
    inbound_handlers: HandlerChain,
    outbound_handlers: HandlerChain,
    /// Set to true when a handler returns Stop/StopDrop.
    stopped: bool,
    /// Packets buffered from Replace/Stop that produced multiple packets.
    /// We yield them one at a time from `recv()`.
    pending_recv: Vec<RawPacket>,
    /// Reply packets buffered from a previous recv that produced multiple forward packets.
    /// Replies are only returned with the first forward packet; subsequent pending packets
    /// carry no replies.
    pending_replies: Vec<RawPacket>,
    started: bool,
}

impl Session {
    /// Create a session without any handlers.
    pub fn new(transport: Box<dyn PacketTransport>, direction: PacketDirection) -> Self {
        Self {
            transport,
            inbound_handlers: HandlerChain::new(),
            outbound_handlers: HandlerChain::new(),
            direction,
            stopped: false,
            pending_recv: Vec::new(),
            pending_replies: Vec::new(),
            started: false,
        }
    }

    /// Create a session with pre-built handler chains.
    pub fn with_handlers(
        transport: Box<dyn PacketTransport>,
        direction: PacketDirection,
        inbound_handlers: HandlerChain,
        outbound_handlers: HandlerChain,
    ) -> Self {
        Self {
            transport,
            inbound_handlers,
            outbound_handlers,
            direction,
            stopped: false,
            pending_recv: Vec::new(),
            pending_replies: Vec::new(),
            started: false,
        }
    }

    pub fn direction(&self) -> PacketDirection {
        self.direction
    }

    fn ensure_started(&mut self) {
        if !self.started {
            self.started = true;
            self.inbound_handlers.notify_start();
            self.outbound_handlers.notify_start();
        }
    }

    pub async fn recv(&mut self) -> RecvResult {
        self.ensure_started();

        if self.stopped {
            return RecvResult { event: SessionEvent::Stopped, replies: Vec::new() };
        }

        // Drain pending replies from a previous multi-packet result.
        let pending_replies = std::mem::take(&mut self.pending_replies);

        if let Some(p) = self.pending_recv.pop() {
            return RecvResult { event: SessionEvent::Packet(p), replies: pending_replies };
        }

        loop {
            match self.transport.recv().await {
                Ok(TransportEvent::Seed(data)) => {
                    return RecvResult { event: SessionEvent::Seed(data), replies: Vec::new() };
                }

                Ok(TransportEvent::Packet(data)) => {
                    let packet = RawPacket::new(data, self.direction);
                    let packet_id = packet.id();
                    debug!(target: logs::SESSION, "session recv {} -> 0x{:02X} ({} bytes)",
                        self.direction, packet_id, packet.data.len());

                    let result = self.inbound_handlers.process(packet, self.direction);

                    let HandlerResult { forward: packets, replies, stop } = result;

                    if stop {
                        self.stopped = true;
                    }

                    if packets.is_empty() {
                        if self.stopped {
                            return RecvResult {
                                event: SessionEvent::Stopped,
                                replies,
                            };
                        }
                        if replies.is_empty() {
                            trace!(target: logs::HANDLER,
                                "packet 0x{:02X} was dropped by handler chain", packet_id);
                            continue;
                        }
                        // All forward packets were dropped but we have replies —
                        // continue reading but return the replies with the next event.
                        self.pending_replies.extend(replies);
                        continue;
                    }

                    let mut iter = packets.into_iter();
                    let first = iter.next().unwrap();
                    let mut rest: Vec<RawPacket> = iter.collect();
                    rest.reverse();
                    self.pending_recv.clear();
                    self.pending_recv.extend(rest);

                    return RecvResult {
                        event: SessionEvent::Packet(first),
                        replies,
                    };
                }

                Err(TransportError::Closed) => {
                    return RecvResult { event: SessionEvent::Disconnected, replies: Vec::new() };
                }
                Err(e) => {
                    return RecvResult { event: SessionEvent::Error(e), replies: Vec::new() };
                }
            }
        }
    }

    pub async fn send(&mut self, packet: RawPacket) -> error::Result<SendResult> {
        self.ensure_started();

        debug!(target: logs::SESSION, "session send {} -> 0x{:02X} ({} bytes)",
            self.direction, packet.id(), packet.data.len());

        let result = self.outbound_handlers.process(packet, self.direction);

        let HandlerResult { forward: packets, replies, stop } = result;

        if packets.is_empty() && replies.is_empty() {
            return Ok(SendResult { replies: Vec::new() });
        }

        for p in packets {
            self.transport.send(TransportEvent::Packet(p.data)).await?;
        }

        // Flush after writing all packets in the batch so they go out
        // in a single TCP segment instead of one syscall per packet.
        self.transport.flush().await?;

        if stop {
            self.stopped = true;
        }

        Ok(SendResult { replies })
    }

    /// Send a typed packet, encoding it and tagging with this session's direction.
    ///
    /// Convenience wrapper around [`send`](Self::send) that handles
    /// `encode_packet` + `RawPacket` construction automatically.
    pub async fn send_packet<T: BasicPacket>(&mut self, packet: &T) -> error::Result<SendResult> {
        let raw = RawPacket::new(encode_packet(packet), self.direction);
        self.send(raw).await
    }

    /// Enqueue a packet without flushing the transport.
    ///
    /// Use this when sending multiple packets in a batch: call
    /// `send_buffered` for each packet, then [`flush`](Self::flush)
    /// once at the end.  This avoids one TCP syscall per packet.
    pub async fn send_buffered(&mut self, packet: RawPacket) -> error::Result<SendResult> {
        self.ensure_started();

        debug!(target: logs::SESSION, "session send_buffered {} -> 0x{:02X} ({} bytes)",
            self.direction, packet.id(), packet.data.len());

        let result = self.outbound_handlers.process(packet, self.direction);

        let HandlerResult { forward: packets, replies, stop } = result;

        if packets.is_empty() && replies.is_empty() {
            return Ok(SendResult { replies: Vec::new() });
        }

        for p in packets {
            self.transport.send(TransportEvent::Packet(p.data)).await?;
        }

        if stop {
            self.stopped = true;
        }

        Ok(SendResult { replies })
    }

    /// Flush any buffered writes to the underlying transport.
    pub async fn flush(&mut self) -> error::Result<()> {
        self.transport.flush().await?;
        Ok(())
    }

    /// Send multiple packets at once.
    ///
    /// Each packet is run through the outbound handler chain individually.
    /// A single flush is performed at the end for batching efficiency.
    /// All reply packets are collected and returned together.
    pub async fn send_all(&mut self, packets: Vec<RawPacket>) -> error::Result<SendResult> {
        let mut all_replies = Vec::new();
        for p in packets {
            let result = self.send_buffered(p).await?;
            all_replies.extend(result.replies);
        }
        self.transport.flush().await?;
        Ok(SendResult { replies: all_replies })
    }

    pub async fn send_seed(&mut self, data: bytes::Bytes) -> error::Result<()> {
        self.ensure_started();
        self.transport.send(TransportEvent::Seed(data)).await?;
        self.transport.flush().await?;
        Ok(())
    }

    pub async fn close(&mut self) {
        self.inbound_handlers.notify_close();
        self.outbound_handlers.notify_close();
        self.transport.close().await;
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("direction", &self.direction)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}


/// Builder for constructing [`Session`] instances with handler chains.
///
/// Accepts a `(Box<dyn PacketTransport>, Direction)` pair (typically from
/// [`TransportBuilder`](protocol::transport::builder::TransportBuilder))
/// and allows attaching inbound/outbound handlers before building.
///
/// # Example
///
/// ```rust,no_run
/// use protocol::transport::builder::TransportBuilder;
/// use network::session::SessionBuilder;
/// use network::handler::filters::LogHandler;
///
/// # async fn example() {
/// let stream = tokio::net::TcpStream::connect("127.0.0.1:2593").await.unwrap();
/// let protocol = protocol::Protocol::login(
///     0xDEADBEEF, u_core::ProtocolVersion::AOS_CLIENT, true,
/// );
///
/// let (transport, direction) = TransportBuilder::client(stream, &protocol)
///     .build()
///     .unwrap();
///
/// let session = SessionBuilder::new(transport, direction)
///     .handler_inbound(Box::new(LogHandler::new("C→S")))
///     .build();
/// # }
/// ```
pub struct SessionBuilder {
    transport: Box<dyn PacketTransport>,
    direction: PacketDirection,
    inbound_handlers: Vec<Box<dyn PacketHandler>>,
    outbound_handlers: Vec<Box<dyn PacketHandler>>,
}

impl SessionBuilder {
    /// Create a new builder from a transport and direction.
    pub fn new(transport: Box<dyn PacketTransport>, direction: PacketDirection) -> Self {
        Self {
            transport,
            direction,
            inbound_handlers: Vec::new(),
            outbound_handlers: Vec::new(),
        }
    }

    /// Add a handler for packets coming **into** this session.
    pub fn handler_inbound(mut self, handler: Box<dyn PacketHandler>) -> Self {
        self.inbound_handlers.push(handler);
        self
    }

    /// Add a handler for packets going **out** of this session.
    pub fn handler_outbound(mut self, handler: Box<dyn PacketHandler>) -> Self {
        self.outbound_handlers.push(handler);
        self
    }

    /// Add the same handler to both directions.
    pub fn handler_both<H: PacketHandler + Clone + 'static>(self, handler: H) -> Self {
        self.handler_inbound(Box::new(handler.clone()))
            .handler_outbound(Box::new(handler))
    }

    /// Build the [`Session`].
    pub fn build(self) -> Session {
        let mut inbound = HandlerChain::new();
        for h in self.inbound_handlers {
            inbound.add(h);
        }

        let mut outbound = HandlerChain::new();
        for h in self.outbound_handlers {
            outbound.add(h);
        }

        Session::with_handlers(self.transport, self.direction, inbound, outbound)
    }
}


// ── PacketSink ────────────────────────────────────────────────────────────

/// Abstraction over different ways to send S→C packets to a client.
///
/// Implemented for [`Session`] (propagates transport errors) and for
/// [`mpsc::Sender<RawPacket>`] (fire-and-forget, never returns an error).
#[async_trait]
pub trait PacketSink: Send {
    /// Send a server-to-client packet.
    async fn send_packet(&mut self, packet: RawPacket) -> error::Result<()>;
}

#[async_trait]
impl PacketSink for Session {
    async fn send_packet(&mut self, packet: RawPacket) -> error::Result<()> {
        self.send(packet).await?;
        Ok(())
    }
}

#[async_trait]
impl PacketSink for mpsc::Sender<RawPacket> {
    async fn send_packet(&mut self, packet: RawPacket) -> error::Result<()> {
        let _ = self.send(packet).await;
        Ok(())
    }
}
