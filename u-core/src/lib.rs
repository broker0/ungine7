//! Core domain types for Ultima Online.
//!
//! This crate provides the foundational vocabulary types shared across the
//! UO crate ecosystem: [`ProtocolVersion`], [`PacketDirection`], [`RawPacket`],
//! [`Role`], and position types ([`Pos2D`], [`Pos3D`], [`TilePos`],
//! [`MobilePos`], [`BlockKey`], [`Heading`], [`Facing`]).

pub mod packet;
pub mod position;
pub mod role;
pub mod version;

// ── Convenience re-exports ─────────────────────────────────────────────────

pub use packet::{PacketDirection, RawPacket};
pub use position::{BlockKey, Facing, Heading, MobilePos, Pos2D, Pos3D, TilePos};
pub use role::Role;
pub use version::ProtocolVersion;

// ── Prelude ────────────────────────────────────────────────────────────────

/// Prelude — import all core domain types.
///
/// ```rust,ignore
/// use u_core::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        BlockKey, Facing, Heading, MobilePos, PacketDirection, Pos2D, Pos3D, ProtocolVersion,
        RawPacket, Role, TilePos,
    };
}
