//! Generic binary I/O primitives for Ultima Online data formats.
//!
//! Provides a byte-order–parameterized reader/writer, `Decode<E>`/`Encode<E>`
//! traits, common wire-format types, and the [`BasicPacket`] framed-packet abstraction.
//!
//! Used by `protocol` (big-endian network packets) and `files`
//! (little-endian MUL/IDX data files).

pub mod endian;
pub mod error;
pub mod packet;
pub mod reader;
pub mod stream;
pub mod strings;
pub mod traits;
pub mod types;
pub mod writer;

// ── Convenience re-exports ─────────────────────────────────────────────────

pub use endian::{ByteOrder, BE, LE};
pub use error::DecodeError;
pub use packet::{encode_packet, packet_reader, packet_writer, ManualPacket, PacketError, BasicPacket};
pub use packet::PacketSize;
pub use reader::{BinaryReader, ReadPrimitives};
pub use stream::StreamReader;
pub use strings::{FixedString, NullString, NullUnicodeString, RawBytes, encode_le_utf16_str, decode_le_utf16_str, encode_le_utf16_bytes, decode_le_utf16_bytes};
pub use traits::{Decode, Encode};
pub use types::{ListU16, ListU8, Pad};
pub use writer::BinaryWriter;
