pub mod protocol_stream;
pub mod crypto_stream;
pub mod codec_transport;
pub mod tcp;
pub mod factory;
pub mod memory;
pub mod builder;

use std::fmt::Debug;
use async_trait::async_trait;
use bytes::Bytes;
use crate::codec::CodecError;

#[derive(Debug, Clone)]
pub enum TransportEvent {
    Seed(Bytes),
    Packet(Bytes),
}

#[derive(Debug)]
pub enum TransportError {
    Closed,
    Io(std::io::Error),
    Codec(CodecError),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "transport closed"),
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Codec(e) => write!(f, "codec: {e}"),
        }
    }
}

impl std::error::Error for TransportError {}

#[async_trait]
pub trait PacketTransport: Send + Debug {
    async fn recv(&mut self) -> Result<TransportEvent, TransportError>;
    async fn send(&mut self, event: TransportEvent) -> Result<(), TransportError>;

    /// Flush any buffered writes to the underlying transport.
    ///
    /// For transports with write buffering (e.g. [`BufWriter`](std::io::BufWriter)-wrapped TCP),
    /// this ensures all queued data is actually sent. The default
    /// implementation is a no-op for transports that don't buffer.
    async fn flush(&mut self) -> Result<(), TransportError> { Ok(()) }

    async fn close(&mut self);
}
