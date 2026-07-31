//! Re-exports [`RawPacket`] and [`PacketDirection`] from [`u_core`].
//!
//! `RawPacket` was moved to the `u-core` crate so that lightweight crates
//! (e.g. `framework`) can use it without depending on the full protocol
//! stack.  This module preserves the original import path for backward
//! compatibility.

pub use u_core::packet::{PacketDirection, RawPacket};
