//! Packet traits and helpers.
//!
//! Defines [`BasicPacket`] — the framed-packet abstraction with command byte and size,
//! [`ManualPacket`] for packets with hand-written decode logic,
//! [`PacketError`], and the [`encode_packet`] convenience wrapper.

use bytes::Bytes;

use crate::{BinaryReader, BinaryWriter, Decode, DecodeError, Encode, BE};

// ── PacketError ────────────────────────────────────────────────────────────

/// Unified error for packet (de)serialization.
///
/// Wraps [`DecodeError`] and adds the packet-specific `BadId` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    BadId { expected: u8, actual: u8 },
    Decode(DecodeError),
}

impl From<DecodeError> for PacketError {
    fn from(e: DecodeError) -> Self {
        PacketError::Decode(e)
    }
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::BadId { expected, actual } => {
                write!(f, "bad packet id: expected 0x{expected:02X}, got 0x{actual:02X}")
            }
            PacketError::Decode(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PacketError {}

// ── Shared encoding helper ─────────────────────────────────────────────────

/// Encode any `Encode<BE>` value into wire [`Bytes`], back-patching the
/// dynamic length field (bytes 1–2) when `size` is [`PacketSize::Dynamic`].
fn encode_to_wire<T: Encode<BE> + ?Sized>(packet: &T, size: PacketSize) -> Bytes {
    let mut w = BinaryWriter::<BE>::with_capacity(64);
    packet.encode(&mut w);
    if matches!(size, PacketSize::Dynamic) {
        w.set_u16_at(1, w.len() as u16);
    }
    w.finish()
}

// ── BasicPacket ───────────────────────────────────────────────────────────────

/// A framed UO packet with a known command byte and size.
///
/// Provides `from_bytes()` for decoding from raw byte data and
/// `to_bytes()` for encoding into wire [`Bytes`].
pub trait BasicPacket: Encode<BE> + Decode<BE> {
    const ID: u8;
    const SIZE: PacketSize;

    /// Decode from raw bytes, checking the command byte.
    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        if data.is_empty() {
            return Err(DecodeError::Truncated.into());
        }
        if data[0] != Self::ID {
            return Err(PacketError::BadId { expected: Self::ID, actual: data[0] });
        }
        let mut reader = BinaryReader::<BE>::new(data);
        Ok(Self::decode(&mut reader)?)
    }

    /// Encode this packet into wire [`Bytes`].
    ///
    /// For dynamic packets, the length field (bytes 1-2) is automatically
    /// back-patched with the actual total length after encoding.
    fn to_bytes(&self) -> Bytes {
        encode_to_wire(self, Self::SIZE)
    }
}

/// Encode a typed packet into [`Bytes`].
///
/// Convenience wrapper around [`BasicPacket::to_bytes()`].
pub fn encode_packet<T: BasicPacket>(packet: &T) -> Bytes {
    packet.to_bytes()
}

/// Whether a packet has a fixed or dynamic (length-prefixed) wire size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketSize {
    /// Fixed number of bytes (including the command byte).
    Fixed(u16),
    /// Variable length — bytes 1–2 contain a big-endian u16 total length.
    Dynamic,
}

impl PacketSize {
    /// Returns the fixed length, or `None` if dynamic.
    pub const fn fixed_len(&self) -> Option<usize> {
        match self {
            PacketSize::Fixed(val) => Some(*val as usize),
            PacketSize::Dynamic => None,
        }
    }
}

// ── packet_reader ──────────────────────────────────────────────────────────

/// Create a [`BinaryReader<BE>`] positioned after the packet header.
///
/// Validates:
/// - `data.len() >= min_len` (otherwise [`DecodeError::Truncated`])
/// - `data[0] == id` (otherwise [`PacketError::BadId`])
///
/// Advances the reader past:
/// - The 1-byte command ID (always)
/// - The 2-byte length field (only when `dynamic == true`)
pub fn packet_reader<'a>(
    data: &'a [u8],
    id: u8,
    min_len: usize,
    dynamic: bool,
) -> Result<BinaryReader<'a, BE>, PacketError> {
    if data.len() < min_len {
        return Err(DecodeError::Truncated.into());
    }
    if data[0] != id {
        return Err(PacketError::BadId { expected: id, actual: data[0] });
    }
    let mut r = BinaryReader::<BE>::new(data);
    let _id: u8 = Decode::decode(&mut r)?;
    if dynamic {
        let _len: u16 = Decode::decode(&mut r)?;
    }
    Ok(r)
}

// ── packet_writer ──────────────────────────────────────────────────────────

/// Create a [`BinaryWriter<BE>`] pre-filled with the packet header.
///
/// Writes:
/// - The 1-byte command ID (always)
/// - A 2-byte `0x0000` length placeholder (only when `dynamic == true`)
///
/// This is the encoding counterpart to [`packet_reader`]. For dynamic
/// packets the length placeholder at offset 1 is automatically back-patched
/// by [`ManualPacket::to_bytes`] (or the caller can use
/// [`BinaryWriter::set_u16_at`]).
///
/// # Example
///
/// ```ignore
/// let mut w = packet_writer(Self::ID, true);
/// w.put_u32(self.serial);
/// // … write remaining fields …
/// ```
pub fn packet_writer(id: u8, dynamic: bool) -> BinaryWriter<BE> {
    let mut w = BinaryWriter::<BE>::with_capacity(64);
    w.put_u8(id);
    if dynamic {
        w.put_u16(0); // length placeholder — back-patched by to_bytes()
    }
    w
}

// ── ManualPacket ───────────────────────────────────────────────────────────

/// A manually-coded packet that provides [`Encode<BE>`] but not [`Decode<BE>`].
///
/// Provides a default [`to_bytes()`](Self::to_bytes) implementation that:
/// 1. Creates a writer, calls [`Encode::encode`]
/// 2. Back-patches the u16 length at offset 1 (for [`PacketSize::Dynamic`] only)
/// 3. Returns the final [`Bytes`]
///
/// # Example
///
/// ```ignore
/// impl ManualPacket for TextCommand {
///     const ID: u8 = 0x12;
///     const SIZE: PacketSize = PacketSize::Dynamic;
/// }
/// // TextCommand::to_bytes() is now available via the trait default.
/// ```
pub trait ManualPacket: Encode<BE> {
    /// The command byte identifying this packet on the wire.
    const ID: u8;

    /// Whether this packet has a fixed or dynamic wire size.
    const SIZE: PacketSize;

    /// Decode from raw packet bytes.
    fn from_bytes(data: &[u8]) -> Result<Self, PacketError>
    where
        Self: Sized;

    /// Encode this packet into wire [`Bytes`].
    ///
    /// For dynamic packets, the length field (bytes 1–2) is automatically
    /// back-patched with the actual total length after encoding.
    fn to_bytes(&self) -> Bytes {
        encode_to_wire(self, Self::SIZE)
    }
}