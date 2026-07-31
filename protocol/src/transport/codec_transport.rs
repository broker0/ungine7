use std::fmt::Debug;
use bytes::{Bytes, BytesMut};
use log::{debug, error, trace, warn};
use u_core::ProtocolVersion;
use crate::codec::CodecError;
use crate::codec::packet_table::{PacketLengthError, PacketTable};
use crate::protocol::{Role, MAX_SEED_SIZE};
use crate::logs;

use super::protocol_stream::ProtocolStream;
use super::*;


#[derive(Debug, PartialEq)]
enum CodecState {
    WaitingSeed,
    Active,
}


/// Protocol-aware transport that layers packet framing on top of any
/// [`ProtocolStream`].
///
/// `CodecTransport` handles seed extraction and packet framing (length
/// detection, reassembly). Encryption is handled by the underlying stream
/// — typically a [`CryptoStream`](super::crypto_stream::CryptoStream)
/// that implements `ProtocolStream`.
///
/// ```text
/// ProtocolStream (plaintext I/O — encryption handled below)
///   → seed extraction via read_seed() (first N bytes, unencrypted)
///   → packet framing (PacketTable)
///   → TransportEvent::Packet
/// ```
///
/// The reverse path for sending:
///
/// ```text
/// TransportEvent::Packet
///   → packet validation (PacketTable)
///   → write_all() (encryption handled by ProtocolStream)
/// ```
///
/// # Type parameter
///
/// `S` is the underlying protocol stream — typically
/// `CryptoStream<TcpByteStream>` for standard encrypted connections,
/// but can be any [`ProtocolStream`] implementation. This allows inserting
/// middleware (logging, traffic analysis, throttling) at any level.
///
/// # Standard usage
///
/// For TCP connections, use the type alias [`TcpTransport`](super::tcp::TcpTransport)
/// or the [`TransportBuilder`](super::builder::TransportBuilder) which handles
/// all wiring automatically.
///
/// # Custom usage
///
/// ```rust,no_run
/// use protocol::transport::codec_transport::CodecTransport;
/// use protocol::transport::tcp::TcpByteStream;
/// use protocol::transport::crypto_stream::CryptoStream;
/// use protocol::codec::encryption;
/// use protocol::protocol::{Protocol, Role};
///
/// # async fn example() {
/// let stream = tokio::net::TcpStream::connect("127.0.0.1:2593").await.unwrap();
/// let protocol = Protocol::login(0xDEADBEEF, u_core::ProtocolVersion::AOS_CLIENT, true);
///
/// let byte_stream = TcpByteStream::new(stream);
/// let (encryptor, decryptor) = encryption::cipher_pair(&protocol, Role::Client);
/// let crypto = CryptoStream::new(byte_stream, encryptor, decryptor);
///
/// let transport = CodecTransport::new(
///     crypto,
///     Role::Client,
///     protocol.seed_size(),
///     protocol.client_version(),
/// );
/// # }
/// ```
pub struct CodecTransport<S: ProtocolStream> {
    stream: S,
    role: Role,
    seed_size: usize,
    packet_table: PacketTable,

    decode_buf: BytesMut,
    recv_state: CodecState,
    send_state: CodecState,
}

impl<S: ProtocolStream> Debug for CodecTransport<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodecTransport")
            .field("stream", &self.stream)
            .field("role", &self.role)
            .field("recv_state", &self.recv_state)
            .field("send_state", &self.send_state)
            .field("decode_buf_len", &self.decode_buf.len())
            .finish()
    }
}

impl<S: ProtocolStream> CodecTransport<S> {
    /// Create a new `CodecTransport` over the given protocol stream.
    ///
    /// Encryption/decryption is handled by the stream itself (e.g. via
    /// [`CryptoStream`](super::crypto_stream::CryptoStream)). This
    /// struct only handles seed extraction and packet framing.
    pub fn new(
        stream: S,
        role: Role,
        seed_size: usize,
        client_version: ProtocolVersion,
    ) -> Self {
        assert!(
            seed_size <= MAX_SEED_SIZE,
            "seed_size ({seed_size}) exceeds MAX_SEED_SIZE ({MAX_SEED_SIZE})"
        );

        let recv_state = match role {
            Role::Server => CodecState::WaitingSeed,
            Role::Client => CodecState::Active,
        };

        let send_state = match role {
            Role::Client => CodecState::WaitingSeed,
            Role::Server => CodecState::Active,
        };

        Self {
            stream,
            role,
            seed_size,
            packet_table: PacketTable::new(client_version),

            decode_buf: BytesMut::new(),
            recv_state,
            send_state,
        }
    }

    /// Warn threshold for unusually large outgoing packets (32 KiB).
    const LARGE_PACKET_THRESHOLD: usize = 32000;

    /// Validate outgoing packet against the packet table.
    fn validate_outgoing(&self, data: &[u8]) -> Result<(), TransportError> {
        let expected = self.packet_table.get_packet_length(data)
            .map_err(|e| TransportError::Codec(e.into()))?;

        if data.len() != expected {
            return Err(TransportError::Codec(CodecError::SizeMismatch {
                packet_id: data[0],
                expected,
                actual: data.len(),
            }));
        }

        if data.len() > Self::LARGE_PACKET_THRESHOLD {
            warn!(target: logs::TRANSPORT_PACKET,
                "outgoing packet 0x{:02X} is unusually large ({} bytes)", data[0], data.len());
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl<S: ProtocolStream> PacketTransport for CodecTransport<S> {
    async fn recv(&mut self) -> Result<TransportEvent, TransportError> {
        // Phase 1: Seed (read via read_seed — bypasses encryption)
        if self.recv_state == CodecState::WaitingSeed {
            let mut seed_buf = [0u8; MAX_SEED_SIZE];
            self.stream.read_seed(&mut seed_buf[..self.seed_size]).await
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::UnexpectedEof => TransportError::Closed,
                    _ => TransportError::Io(e),
                })?;

            trace!(target: logs::TRANSPORT_RAW, "read seed ({} bytes): {:02X?}", self.seed_size, &seed_buf[..self.seed_size]);

            self.recv_state = CodecState::Active;
            return Ok(TransportEvent::Seed(
                Bytes::copy_from_slice(&seed_buf[..self.seed_size])
            ));
        }

        // Phase 2: Packets (read via read_into — data is already decrypted by the stream)
        loop {
            // Try to extract a complete packet from decode_buf
            match self.packet_table.get_packet_length(&self.decode_buf) {
                Ok(packet_len) if self.decode_buf.len() >= packet_len => {
                    let packet = self.decode_buf.split_to(packet_len).freeze();
                    debug!(target: logs::TRANSPORT_PACKET, "recv packet 0x{:02X} ({} bytes)", packet[0], packet.len());
                    trace!(target: logs::TRANSPORT_PACKET, "packet data: {:02X?}", &packet[..]);
                    return Ok(TransportEvent::Packet(packet));
                }

                Ok(_) => {
                    // Have the header but not enough payload yet — fall through to read
                }

                Err(PacketLengthError::InsufficientData) => {
                    // Not even enough for a header — fall through to read
                }

                Err(PacketLengthError::UnknownPacket) => {
                    error!(target: logs::TRANSPORT_PACKET, "unknown packet 0x{:02X}", self.decode_buf[0]);
                    return Err(TransportError::Codec(
                        PacketLengthError::UnknownPacket.into()
                    ));
                }

                Err(PacketLengthError::InvalidDynamicLength(len)) => {
                    error!(target: logs::TRANSPORT_PACKET, "invalid dynamic length {} for packet 0x{:02X}", len, self.decode_buf[0]);
                    return Err(TransportError::Codec(
                        PacketLengthError::InvalidDynamicLength(len).into()
                    ));
                }
            }

            // Read plaintext data (already decrypted by CryptoStream or similar)
            let n = self.stream.read_into(&mut self.decode_buf).await
                .map_err(TransportError::Io)?;

            if n == 0 {
                return Err(TransportError::Closed);
            }

            trace!(target: logs::TRANSPORT_RAW, "read {} plaintext bytes, decode_buf now {} bytes", n, self.decode_buf.len());
        }
    }

    async fn send(&mut self, event: TransportEvent) -> Result<(), TransportError> {
        match event {
            TransportEvent::Seed(data) => {
                if self.send_state != CodecState::WaitingSeed {
                    return Err(TransportError::Codec(CodecError::ProtocolViolation(
                        "seed already sent or not expected for this role"
                    )));
                }

                trace!(target: logs::TRANSPORT_RAW, "sending seed ({} bytes): {:02X?}", data.len(), &data[..]);
                self.stream.write_seed(&data).await
                    .map_err(TransportError::Io)?;

                self.send_state = CodecState::Active;
            }

            TransportEvent::Packet(data) => {
                if self.send_state == CodecState::WaitingSeed {
                    return Err(TransportError::Codec(CodecError::ProtocolViolation(
                        "must send seed before packets"
                    )));
                }

                debug!(target: logs::TRANSPORT_PACKET, "send packet 0x{:02X} ({} bytes)", data[0], data.len());
                trace!(target: logs::TRANSPORT_PACKET, "packet data: {:02X?}", &data[..]);

                // Validate against packet table
                self.validate_outgoing(&data)?;

                // Write plaintext — encryption is handled by the stream
                self.stream.write_all(&data).await
                    .map_err(TransportError::Io)?;
            }
        }

        Ok(())
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        self.stream.flush().await.map_err(TransportError::Io)
    }

    async fn close(&mut self) {
        if !self.decode_buf.is_empty() {
            let len = self.decode_buf.len();
            let preview = &self.decode_buf[..len.min(128)];
            warn!(target: logs::TRANSPORT_PACKET,
                "closing with {len} unconsumed bytes in decode buffer: {preview:02X?}");
        }
        self.stream.shutdown().await;
    }
}
