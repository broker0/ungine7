use std::fmt::Debug;
use std::future::Future;
use bytes::BytesMut;

/// Bidirectional byte stream with protocol-aware seed handling.
///
/// `ProtocolStream` is the fundamental I/O abstraction in the transport
/// pipeline. It provides raw byte read/write operations, plus two
/// protocol-specific methods — [`read_seed()`](Self::read_seed) and
/// [`write_seed()`](Self::write_seed) — that allow encryption middleware
/// (like [`CryptoStream`](super::crypto_stream::CryptoStream)) to bypass
/// cipher processing for seed data.
///
/// # Layering
///
/// Implementations can be composed in a middleware chain:
///
/// ```text
/// TcpByteStream (raw TCP I/O)
///   └─ CryptoStream (encrypts write_all / decrypts read, passes seed through)
///        └─ LoggingStream (observes plaintext after decryption)
///             └─ CodecTransport (packet framing)
/// ```
///
/// # Seed methods
///
/// UO connections start with an unencrypted seed (4 or [`MAX_SEED_SIZE`](crate::protocol::MAX_SEED_SIZE) bytes) before
/// any encrypted traffic. The default implementations of `read_seed` and
/// `write_seed` simply delegate to `read_exact` and `write_all` — which
/// is correct for non-encrypting streams. [`CryptoStream`](super::crypto_stream::CryptoStream)
/// overrides them to bypass its cipher, forwarding the call directly to
/// the inner stream.
///
/// # Zero-copy path
///
/// [`read_into()`](Self::read_into) appends data directly into a `BytesMut`
/// buffer. [`CryptoStream`](super::crypto_stream::CryptoStream) overrides it
/// to decrypt directly into the caller's buffer, avoiding an intermediate copy.
/// [`CodecTransport`](super::codec_transport::CodecTransport) uses `read_into`
/// to fill its decode buffer efficiently.
///
/// # Middleware example
///
/// ```rust,no_run
/// use bytes::BytesMut;
/// use protocol::transport::protocol_stream::ProtocolStream;
/// use protocol::transport::tcp::TcpByteStream;
///
/// #[derive(Debug)]
/// struct LoggingStream<S: ProtocolStream> {
///     inner: S,
///     label: String,
/// }
///
/// impl<S: ProtocolStream> ProtocolStream for LoggingStream<S> {
///     async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
///         let n = self.inner.read(buf).await?;
///         eprintln!("[{}] read {} bytes: {:02X?}", self.label, n, &buf[..n]);
///         Ok(n)
///     }
///
///     async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
///         eprintln!("[{}] write {} bytes: {:02X?}", self.label, data.len(), data);
///         self.inner.write_all(data).await
///     }
///
///     async fn flush(&mut self) -> std::io::Result<()> {
///         self.inner.flush().await
///     }
///
///     async fn shutdown(&mut self) {
///         self.inner.shutdown().await;
///     }
/// }
/// ```
pub trait ProtocolStream: Send + Debug {
    /// Read raw bytes into `buf`. Returns the number of bytes read.
    ///
    /// Returns `0` when the stream has reached EOF (remote closed).
    fn read(&mut self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<usize>> + Send;

    /// Read exactly `buf.len()` bytes from the stream.
    ///
    /// Returns [`io::ErrorKind::UnexpectedEof`](std::io::ErrorKind::UnexpectedEof)
    /// if the stream closes before the buffer is filled.
    ///
    /// The default implementation loops over [`read()`](Self::read).
    /// Concrete implementations (e.g. [`TcpByteStream`](super::tcp::TcpByteStream)) may override
    /// this with a more efficient native call.
    fn read_exact(&mut self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<()>> + Send {
        async {
            let mut filled = 0;
            while filled < buf.len() {
                let n = self.read(&mut buf[filled..]).await?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "stream closed before read_exact completed",
                    ));
                }
                filled += n;
            }
            Ok(())
        }
    }

    /// Read bytes, appending directly into a [`BytesMut`] buffer.
    ///
    /// Returns the number of new bytes appended. The default implementation
    /// reads into a temporary stack buffer and copies into `dst`.
    ///
    /// [`CryptoStream`](super::crypto_stream::CryptoStream) overrides this
    /// to decrypt directly into `dst`, providing a zero-copy path when used
    /// with [`CodecTransport`](super::codec_transport::CodecTransport).
    fn read_into(&mut self, dst: &mut BytesMut) -> impl Future<Output = std::io::Result<usize>> + Send {
        async {
            let mut tmp = [0u8; 4096];
            let n = self.read(&mut tmp).await?;
            if n > 0 {
                dst.extend_from_slice(&tmp[..n]);
            }
            Ok(n)
        }
    }

    /// Write all bytes from `data` to the stream.
    ///
    /// This may buffer internally — call [`flush()`](Self::flush) to ensure
    /// data is actually sent to the underlying transport.
    fn write_all(&mut self, data: &[u8]) -> impl Future<Output = std::io::Result<()>> + Send;

    /// Flush any buffered writes to the underlying transport.
    ///
    /// The default implementation is a no-op for streams that don't buffer.
    fn flush(&mut self) -> impl Future<Output = std::io::Result<()>> + Send {
        async { Ok(()) }
    }

    /// Shut down the stream, signaling that no more data will be written.
    fn shutdown(&mut self) -> impl Future<Output = ()> + Send;

    /// Read seed bytes, bypassing any encryption layer.
    ///
    /// The seed (4 or [`MAX_SEED_SIZE`](crate::protocol::MAX_SEED_SIZE) bytes) is the first data on every UO connection
    /// and is always transmitted in plaintext. Non-encrypting streams simply
    /// delegate to [`read_exact()`](Self::read_exact); encryption middleware
    /// like [`CryptoStream`](super::crypto_stream::CryptoStream) forwards
    /// the call to its inner stream, skipping the cipher.
    fn read_seed(&mut self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<()>> + Send {
        async {
            self.read_exact(buf).await
        }
    }

    /// Write seed bytes, bypassing any encryption layer.
    ///
    /// See [`read_seed()`](Self::read_seed) for details on why seeds are
    /// handled separately.
    fn write_seed(&mut self, data: &[u8]) -> impl Future<Output = std::io::Result<()>> + Send {
        async {
            self.write_all(data).await
        }
    }
}
