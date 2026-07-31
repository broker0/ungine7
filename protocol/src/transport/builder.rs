use u_core::PacketDirection;
use crate::protocol::Protocol;
use super::protocol_stream::ProtocolStream;
use super::{factory, PacketTransport, TransportError};

use tokio::net::TcpStream;


/// Errors that can occur during transport building.
#[derive(Debug)]
pub enum TransportBuildError {
    /// No transport was configured
    NoTransport,
    /// I/O error during transport creation
    Io(std::io::Error),
    /// Transport-level error
    Transport(TransportError),
}

impl std::fmt::Display for TransportBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTransport => write!(f, "no transport configured"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
        }
    }
}

impl std::error::Error for TransportBuildError {}

impl From<std::io::Error> for TransportBuildError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<TransportError> for TransportBuildError {
    fn from(e: TransportError) -> Self {
        Self::Transport(e)
    }
}


/// Builder for constructing `PacketTransport` instances with a clean API.
///
/// Produces a `(Box<dyn PacketTransport>, Direction)` pair that can be
/// used directly or wrapped in a higher-level `Session` (from `network`).
///
/// # Standard TCP example
///
/// ```rust,no_run
/// use protocol::protocol::Protocol;
/// use protocol::transport::builder::TransportBuilder;
///
/// # async fn example() {
/// let stream = tokio::net::TcpStream::connect("127.0.0.1:2593").await.unwrap();
/// let protocol = Protocol::login(0xDEADBEEF, u_core::ProtocolVersion::AOS_CLIENT, true);
///
/// let (transport, direction) = TransportBuilder::client(stream, &protocol)
///     .build()
///     .unwrap();
/// # }
/// ```
///
/// # Custom ProtocolStream example
///
/// Use [`client_with_stream()`](Self::client_with_stream) or
/// [`server_with_stream()`](Self::server_with_stream) to provide a custom
/// [`ProtocolStream`] (e.g. with logging, custom encryption, etc.):
///
/// ```rust,no_run
/// use protocol::transport::builder::TransportBuilder;
/// use protocol::transport::tcp::TcpByteStream;
/// use protocol::transport::crypto_stream::CryptoStream;
/// use protocol::codec::encryption;
/// use protocol::Role;
///
/// # async fn example() {
/// let stream = tokio::net::TcpStream::connect("127.0.0.1:2593").await.unwrap();
/// let protocol = protocol::Protocol::login(
///     0xDEADBEEF,
///     u_core::ProtocolVersion::AOS_CLIENT,
///     true,
/// );
///
/// // Build custom stream stack: TCP → Crypto → (pass to builder for framing)
/// let tcp = TcpByteStream::new(stream);
/// let (enc, dec) = encryption::cipher_pair(&protocol, Role::Client);
/// let crypto = CryptoStream::new(tcp, enc, dec);
/// let (transport, direction) = TransportBuilder::client_with_stream(crypto, &protocol)
///     .build()
///     .unwrap();
/// # }
/// ```
pub struct TransportBuilder {
    transport: Option<Box<dyn PacketTransport>>,
    direction: PacketDirection,
}

impl TransportBuilder {
    /// Create a new empty builder (use `.client()` or `.server()` instead in most cases).
    pub fn new() -> Self {
        Self {
            transport: None,
            direction: PacketDirection::ServerToClient,
        }
    }

    /// Create a client-side transport over TCP (receives from server).
    ///
    /// Automatically wires up encryption via [`CryptoStream`](super::crypto_stream::CryptoStream).
    pub fn client(stream: TcpStream, protocol: &Protocol) -> Self {
        let transport = factory::client_transport(stream, protocol);
        Self {
            transport: Some(Box::new(transport)),
            direction: PacketDirection::ServerToClient,
        }
    }

    /// Create a server-side transport over TCP (receives from client).
    ///
    /// Automatically wires up encryption via [`CryptoStream`](super::crypto_stream::CryptoStream).
    pub fn server(stream: TcpStream, protocol: &Protocol) -> Self {
        let transport = factory::server_transport(stream, protocol);
        Self {
            transport: Some(Box::new(transport)),
            direction: PacketDirection::ClientToServer,
        }
    }

    /// Create a client-side transport over a custom [`ProtocolStream`] (receives from server).
    ///
    /// The stream is expected to handle encryption/decryption already
    /// (e.g. via [`CryptoStream`](super::crypto_stream::CryptoStream)).
    /// This method only adds packet framing on top.
    pub fn client_with_stream(stream: impl ProtocolStream + 'static, protocol: &Protocol) -> Self {
        let transport = factory::client_transport_with_stream(stream, protocol);
        Self {
            transport: Some(Box::new(transport)),
            direction: PacketDirection::ServerToClient,
        }
    }

    /// Create a server-side transport over a custom [`ProtocolStream`] (receives from client).
    ///
    /// The stream is expected to handle encryption/decryption already
    /// (e.g. via [`CryptoStream`](super::crypto_stream::CryptoStream)).
    /// This method only adds packet framing on top.
    pub fn server_with_stream(stream: impl ProtocolStream + 'static, protocol: &Protocol) -> Self {
        let transport = factory::server_transport_with_stream(stream, protocol);
        Self {
            transport: Some(Box::new(transport)),
            direction: PacketDirection::ClientToServer,
        }
    }

    /// Configure with a custom `PacketTransport` implementation.
    pub fn custom(transport: Box<dyn PacketTransport>, direction: PacketDirection) -> Self {
        Self {
            transport: Some(transport),
            direction,
        }
    }

    /// Override the packet direction.
    pub fn direction(mut self, direction: PacketDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Build the transport, returning the transport and its direction.
    pub fn build(self) -> Result<(Box<dyn PacketTransport>, PacketDirection), TransportBuildError> {
        let transport = self.transport.ok_or(TransportBuildError::NoTransport)?;
        Ok((transport, self.direction))
    }
}

impl Default for TransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}
