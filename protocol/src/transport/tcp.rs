use std::fmt::Debug;
use std::future::Future;
use log::warn;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use super::protocol_stream::ProtocolStream;
use super::codec_transport::CodecTransport;
use super::crypto_stream::CryptoStream;


/// [`ProtocolStream`] implementation backed by a TCP socket.
///
/// Wraps a [`TcpStream`] split into read/write halves with buffered writes.
/// Sets `TCP_NODELAY` on construction for low-latency proxying.
pub struct TcpByteStream {
    reader: OwnedReadHalf,
    writer: BufWriter<OwnedWriteHalf>,
}

impl Debug for TcpByteStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpByteStream").finish()
    }
}

impl TcpByteStream {
    /// Create a new `TcpByteStream` from a connected [`TcpStream`].
    ///
    /// Disables Nagle's algorithm (`TCP_NODELAY`) — critical for low-latency
    /// proxying where small packets (movement acks, pings) must not be delayed.
    pub fn new(stream: TcpStream) -> Self {
        if let Err(e) = stream.set_nodelay(true) {
            warn!(target: crate::logs::TRANSPORT_RAW, "failed to set TCP_NODELAY: {e}");
        }

        let (reader, write_half) = stream.into_split();
        let writer = BufWriter::new(write_half);

        Self { reader, writer }
    }
}

impl ProtocolStream for TcpByteStream {
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf).await
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<()>> + Send {
        async {
            self.reader.read_exact(buf).await.map(|_| ())
        }
    }

    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush().await
    }

    async fn shutdown(&mut self) {
        self.writer.shutdown().await.ok();
    }
}


/// Standard TCP-based packet transport.
///
/// This is the default transport used for UO client/server connections.
/// It combines three layers:
///
/// - [`TcpByteStream`] — raw TCP I/O
/// - [`CryptoStream`] — encryption/decryption
/// - [`CodecTransport`] — packet framing
///
/// For most use cases, create via [`TransportBuilder`](super::builder::TransportBuilder)
/// or the factory functions in [`factory`](super::factory).
///
/// To insert custom middleware (logging, traffic analysis) between layers,
/// build the stream stack manually:
///
/// ```rust,no_run
/// use protocol::transport::tcp::TcpByteStream;
/// use protocol::transport::crypto_stream::CryptoStream;
/// use protocol::transport::codec_transport::CodecTransport;
/// use protocol::codec::encryption;
/// use protocol::protocol::Role;
///
/// // TcpByteStream → LoggingStream → CryptoStream → LoggingStream → CodecTransport
/// ```
pub type TcpTransport = CodecTransport<CryptoStream<TcpByteStream>>;
