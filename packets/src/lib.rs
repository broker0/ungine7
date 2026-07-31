//! High-level binary packet serialization for Ultima Online.
//!
//! This crate provides derive macros (`Decode`, `Encode`, `WireEnum`, `Packet`) for
//! declarative packet definitions, built on top of `io` primitives.
//!
//! # Example
//!
//! ```ignore
//! use packets::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
//! #[binary(endian = "be")]
//! pub struct Ping {
//!     pub id: u8,
//!     pub sequence: u8,
//! }
//! ```

// Allow `::packets::` paths to resolve inside this crate itself
// (needed by proc-macro generated code).
extern crate self as packets;

// Re-export derive macros.
pub use macros::{Decode, Encode, Packet, WireEnum};

// Re-export io so that generated code can reference `::u_io::*`.
pub use u_io;

pub mod traits;
pub mod registry;
pub mod compress;

pub mod action;
pub mod buff;
pub mod character;
pub mod chat;
pub mod gump;
pub mod house;
pub mod interaction;
pub mod layer;
pub mod login;
pub mod map;
pub mod mobile_flags;
pub mod movement;
pub mod profile;
pub mod redirect;
pub mod seed;
pub mod skills;
pub mod speech;
pub mod status;
pub mod system;
pub mod tooltip;
pub mod trade;
pub mod world;

/// Convenience prelude — import everything needed for packet definitions.
///
/// ```rust,ignore
/// use packets::prelude::*;
/// ```
pub mod prelude {
    // Derive macros.
    pub use macros::{Decode, Encode, Packet, WireEnum};

    // Core io primitives.
    pub use u_io::{
        BE, LE,
        BinaryReader, BinaryWriter, ByteOrder,
        Decode, DecodeError,
        Encode,
        FixedString, ListU16, ListU8,
        NullString, NullUnicodeString, Pad,
        ReadPrimitives,
    };
    pub use u_io::packet::PacketSize;

    // Packet traits and helpers.
    pub use crate::traits::{encode_packet, packet_reader, packet_writer, ManualPacket, PacketError, BasicPacket};

    // Core domain types commonly needed alongside packets.
    pub use u_core::{Facing, Heading, MobilePos, PacketDirection, Pos2D, Pos3D, ProtocolVersion, RawPacket};
}
