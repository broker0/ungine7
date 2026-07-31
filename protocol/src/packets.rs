//! Packet definitions — re-exported from the [`packets`] crate.
//!
//! This module provides backward-compatible access to all packet types.
//! New code should depend on `packets` directly.

// Re-export all packet modules from packets.
pub use packets::{character, login, redirect, seed, system};

// Re-export PacketError from io.
pub use u_io::PacketError;

/// Packet traits and protocol-specific helpers.
///
/// Re-exports core traits from [`packets::traits`] and adds the
/// [`traits::from_raw_packet`] helper that depends on [`RawPacket`](crate::codec::packet::RawPacket).
pub mod traits {
    pub use packets::traits::*;

    use crate::codec::packet::RawPacket;

    /// Decode a typed packet from a [`RawPacket`], checking the command byte.
    pub fn from_raw_packet<T: BasicPacket>(packet: &RawPacket) -> Result<T, super::PacketError> {
        T::from_bytes(&packet.data)
    }
}
