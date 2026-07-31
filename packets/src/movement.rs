//! Movement packets.
//!
//! Covers client move requests, server acknowledgements and rejections.

use macros::{Packet, WireEnum};

// ── 0x02 MoveRequest (7 bytes, fixed, C→S) ────────────────────────────────

/// Packet 0x02 — Move Request (7 bytes, fixed, C→S)
///
/// Sent by the client each step. The `direction` field encodes both the
/// compass heading (bits 0–2) and the running flag (bit 7).
///
/// Direction values (lower 3 bits):
/// - 0x00 North, 0x01 NE, 0x02 East, 0x03 SE,
///   0x04 South, 0x05 SW, 0x06 West, 0x07 NW.
///
/// OR with 0x80 when running (e.g. 0x80 = running north).
///
/// The `fastwalk_key` pops the top element from the fastwalk prevention
/// stack (initialized by 0xBF sub 1, pushed by 0xBF sub 2).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x02, size = fixed(7), endian = "be")]
pub struct MoveRequest {
    pub id: u8,
    pub direction: u8,
    pub sequence: u8,
    pub fastwalk_key: u32,
}

impl MoveRequest {
    pub fn is_running(&self) -> bool {
        self.direction & 0x80 != 0
    }

    pub fn heading(&self) -> u8 {
        self.direction & 0x07
    }
}

// ── 0x21 MoveReject (8 bytes, fixed, S→C) ─────────────────────────────────

/// Packet 0x21 — Character Move Rejection (8 bytes, fixed, S→C)
///
/// Server rejects a client move and snaps the character back to the
/// given position.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x21, size = fixed(8), endian = "be")]
pub struct MoveReject {
    pub id: u8,
    pub sequence: u8,
    pub x: u16,
    pub y: u16,
    pub direction: u8,
    pub z: i8,
}

// ── 0x22 ResyncRequest (3 bytes, fixed, C→S) ──────────────────────────────

/// Packet 0x22 — Resync Request (3 bytes, fixed, C→S)
///
/// Sent by the client to request a full position/state resync from the
/// server. Shares the same ID as [`MoveAck`].
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x22, size = fixed(3), endian = "be")]
pub struct ResyncRequest {
    pub id: u8,
    pub sequence: u8,
    #[binary(pad = 2)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
}

// ── 0x22 MoveAck (3 bytes, fixed, S→C) ────────────────────────────────────

/// Notoriety value sent inside a [`MoveAck`] packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum Notoriety {
    /// 0 — invalid / across server line
    #[wire_enum(0x00, "invalid")]
    Invalid,
    /// 1 — innocent (blue)
    #[wire_enum(0x01, "innocent")]
    Innocent,
    /// 2 — guilded / ally (green)
    #[wire_enum(0x02, "ally")]
    Ally,
    /// 3 — attackable but not criminal (gray)
    #[wire_enum(0x03, "attackable")]
    Attackable,
    /// 4 — criminal (gray)
    #[wire_enum(0x04, "criminal")]
    Criminal,
    /// 5 — enemy (orange)
    #[wire_enum(0x05, "enemy")]
    Enemy,
    /// 6 — murderer (red)
    #[wire_enum(0x06, "murderer")]
    Murderer,
    /// 7 — translucent
    #[wire_enum(0x07, "translucent")]
    Translucent,
    #[wire_enum(unknown)]
    Unknown(u8),
}

/// Packet 0x22 — Character Move ACK (3 bytes, fixed, S→C)
///
/// Server acknowledges a client move and reports the character's current
/// notoriety.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x22, size = fixed(3), endian = "be")]
pub struct MoveAck {
    pub id: u8,
    pub sequence: u8,
    pub notoriety: Notoriety,
}
