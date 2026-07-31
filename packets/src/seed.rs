//! Login/game seed packets.

use u_core::ProtocolVersion;
use u_io::DecodeError;
use u_io::packet::PacketSize;
use macros::{Decode, Encode, Packet};

use crate::traits::{PacketError, BasicPacket};

// ── Seed (4 bytes, raw — no packet ID) ─────────────────────────────────────

/// 4-byte seed sent raw at the start of login and game connections.
///
/// This is **not** a `BasicPacket` — it has no ID byte and is sent raw
/// before any framed packet.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct Seed {
    pub value: u32,
}

impl Seed {
    pub const SIZE: PacketSize = PacketSize::Fixed(4);

    pub fn new(value: u32) -> Self {
        Self { value }
    }

    /// Decode from a raw byte slice (convenience for the detection phase,
    /// where no `BinaryReader` is available yet).
    pub fn decode_raw(data: &[u8]) -> Result<Self, PacketError> {
        if data.len() < 4 {
            return Err(DecodeError::Truncated.into());
        }
        Ok(Self {
            value: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        })
    }
}

// ── 0xEF ExtendedSeed (21 bytes, C→S) ──────────────────────────────────────

/// Packet 0xEF — Extended Login Seed (21 bytes, fixed, C→S)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xEF, size = fixed(21), endian = "be")]
pub struct ExtendedSeed {
    pub id: u8,
    pub seed: u32,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: u32,
}

impl ExtendedSeed {
    pub fn new(seed: u32, major: u32, minor: u32, patch: u32, build: u32) -> Self {
        Self {
            id: Self::ID,
            seed,
            major,
            minor,
            patch,
            build,
        }
    }

    /// Create from a seed value and a [`ProtocolVersion`].
    pub fn from_version(seed: u32, version: ProtocolVersion) -> Self {
        Self::new(
            seed,
            version.major,
            version.minor,
            version.patch,
            version.build,
        )
    }

    /// Reconstruct the [`ProtocolVersion`] from the version fields.
    pub fn version(&self) -> ProtocolVersion {
        ProtocolVersion::new(
            self.major,
            self.minor,
            self.patch,
            self.build,
        )
    }
}
