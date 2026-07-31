use crate::codec::encryption;
use crate::protocol::{Protocol, Role};
use tokio::net::TcpStream;

use super::protocol_stream::ProtocolStream;
use super::tcp::TcpByteStream;
use super::codec_transport::CodecTransport;
use super::crypto_stream::CryptoStream;
use super::tcp::TcpTransport;

/// Create a server-side TCP transport (receives from client).
pub fn server_transport(stream: TcpStream, protocol: &Protocol) -> TcpTransport {
    let byte_stream = TcpByteStream::new(stream);
    let (encryptor, decryptor) = encryption::cipher_pair(protocol, Role::Server);
    let crypto = CryptoStream::new(byte_stream, encryptor, decryptor);
    CodecTransport::new(crypto, Role::Server, protocol.seed_size(), protocol.client_version())
}

/// Create a client-side TCP transport (receives from server).
pub fn client_transport(stream: TcpStream, protocol: &Protocol) -> TcpTransport {
    let byte_stream = TcpByteStream::new(stream);
    let (encryptor, decryptor) = encryption::cipher_pair(protocol, Role::Client);
    let crypto = CryptoStream::new(byte_stream, encryptor, decryptor);
    CodecTransport::new(crypto, Role::Client, protocol.seed_size(), protocol.client_version())
}

/// Create a server-side transport over a custom [`ProtocolStream`].
///
/// The stream is expected to handle encryption/decryption already
/// (e.g. via [`CryptoStream`]). If you need standard encryption,
/// wrap your stream in a `CryptoStream` first.
pub fn server_transport_with_stream<S: ProtocolStream + 'static>(
    stream: S,
    protocol: &Protocol,
) -> CodecTransport<S> {
    CodecTransport::new(stream, Role::Server, protocol.seed_size(), protocol.client_version())
}

/// Create a client-side transport over a custom [`ProtocolStream`].
///
/// The stream is expected to handle encryption/decryption already
/// (e.g. via [`CryptoStream`]). If you need standard encryption,
/// wrap your stream in a `CryptoStream` first.
pub fn client_transport_with_stream<S: ProtocolStream + 'static>(
    stream: S,
    protocol: &Protocol,
) -> CodecTransport<S> {
    CodecTransport::new(stream, Role::Client, protocol.seed_size(), protocol.client_version())
}
