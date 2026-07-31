//! Character appearance, locale, animation and death packets.
//!
//! Covers the initial character body/position info sent after login,
//! the periodic draw updates, character animation commands, and death events.

use u_io::FixedString;
use u_io::BasicPacket;
use macros::Packet;

use crate::mobile_flags::MobileFlags;
use crate::movement::Notoriety;

// ── 0x1B CharacterLocaleAndBody (37 bytes, fixed, S→C) ────────────────────

/// Packet 0x1B — Char Locale and Body (37 bytes, fixed, S→C)
///
/// Sent by the server after character login to tell the client about
/// the player's body type, position and the server map boundaries.
///
/// # Notes on "unknown" fields
///
/// Several fields are documented as "always 0" but OSI and some emulators
/// write non-zero values in them (e.g. `unknown2` carries the server's IP
/// address in some RunUO-derived emulators). They are stored as plain `u32` /
/// `u8` so that roundtrip serialisation preserves the original bytes exactly.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x1B, size = fixed(37), endian = "be")]
pub struct CharacterLocaleAndBody {
    pub id: u8,
    pub serial: u32,
    /// Unknown — always 0 on OSI.
    pub unknown0: u32,
    pub body_type: u16,
    pub x: u16,
    pub y: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad1: (),
    pub z: i8,
    pub facing: u8,
    /// Unknown — some emulators write the server IP here (BE u32).
    pub unknown2: u32,
    /// Unknown — some emulators write `0x7F000000` (127.0.0.1 LE) here.
    pub unknown3: u32,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad4: (),
    pub map_width_minus8: u16,
    pub map_height: u16,
    #[binary(pad = 2)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad5: (),
    /// Unknown — always 0 on OSI.
    pub unknown6: u32,
}

// ── 0x20 DrawGamePlayer (19 bytes, fixed, S→C) ────────────────────────────

/// Packet 0x20 — Draw Game Player (19 bytes, fixed, S→C)
///
/// Sent by the server to update the client's own character appearance
/// and position on screen.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x20, size = fixed(19), endian = "be")]
pub struct DrawGamePlayer {
    pub id: u8,
    pub serial: u32,
    pub body_type: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub hue: u16,
    pub flags: MobileFlags,
    pub x: u16,
    pub y: u16,
    #[binary(pad = 2)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad1: (),
    pub direction: u8,
    pub z: i8,
}

// ── 0x77 UpdatePlayer (17 bytes, fixed, S→C) ──────────────────────────────

/// Packet 0x77 — Update Player (17 bytes, fixed, S→C)
///
/// Sent by the server to update a mobile's position, appearance and
/// status on screen.  Similar to [`DrawGamePlayer`] (0x20) but used for
/// any mobile, not just the player character.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x77, size = fixed(17), endian = "be")]
pub struct UpdateMobile {
    pub id: u8,
    pub serial: u32,
    pub model: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub direction: u8,
    pub hue: u16,
    pub status_flags: MobileFlags,
    pub notoriety: Notoriety,
}

// ── 0x88 OpenPaperdoll (66 bytes, fixed, S→C) ─────────────────────────────

/// Packet 0x88 — Open Paperdoll (66 bytes, fixed, S→C)
///
/// Opens the paperdoll window for a character.
///
/// # Flag byte (pre-AOS)
///
/// | Bit   | Meaning              |
/// |-------|----------------------|
/// | 0x02  | Can alter paperdoll  |
/// | 0x04  | Poisoned             |
/// | 0x08  | Golden health        |
/// | 0x40  | War mode             |
/// | 0x80  | Hidden               |
///
/// As of AOS clients (> 3.0.8z) war mode moved to bit 0x01.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x88, size = fixed(66), endian = "be")]
pub struct OpenPaperdoll {
    pub id: u8,
    pub serial: u32,
    pub text: FixedString<60>,
    pub flags: MobileFlags,
}

// ── 0x6E CharacterAnimation (14 bytes, fixed, S→C) ────────────────────────

/// Packet 0x6E — Character Animation (14 bytes, fixed, S→C)
///
/// Instructs the client to play an animation on a mobile.
///
/// # Action values
///
/// | Value | Animation                          |
/// |-------|------------------------------------|
/// | 0x00  | Walk unarmed                       |
/// | 0x01  | Walk armed                         |
/// | 0x02  | Run unarmed                        |
/// | 0x03  | Run armed                          |
/// | 0x04  | Stand                              |
/// | 0x05  | Shift shoulders                    |
/// | 0x06  | Hands on hips                      |
/// | 0x07  | Attack stance (short)              |
/// | 0x08  | Attack stance (longer)             |
/// | 0x09  | Swing (knife)                      |
/// | 0x0A  | Stab (underhanded)                 |
/// | 0x0B  | Swing overhand (sword)             |
/// | 0x0C  | Swing over and side (sword)        |
/// | 0x0D  | Swing side (sword)                 |
/// | 0x0E  | Stab with point of sword           |
/// | 0x0F  | Ready stance                       |
/// | 0x10  | Magic (cast)                       |
/// | 0x11  | Hands over head                    |
/// | 0x12  | Bow shot                           |
/// | 0x13  | Crossbow                           |
/// | 0x14  | Get hit                            |
/// | 0x15  | Fall down and die (backwards)      |
/// | 0x16  | Fall down and die (forwards)       |
/// | 0x17  | Ride horse (long)                  |
/// | 0x18  | Ride horse (medium)                |
/// | 0x19  | Ride horse (short)                 |
/// | 0x1A  | Swing sword from horse             |
/// | 0x1B  | Bow shot on horse                  |
/// | 0x1C  | Crossbow shot on horse             |
/// | 0x1D  | Block on horse with shield         |
/// | 0x1E  | Block on ground with shield        |
/// | 0x1F  | Swing, get hit in middle           |
/// | 0x20  | Bow (deep)                         |
/// | 0x21  | Salute                             |
/// | 0x22  | Scratch head                       |
/// | 0x23  | One foot forward (2 sec)           |
/// | 0x24  | Same                               |
///
/// # Repeat
///
/// - `1` — play once
/// - `2` — play twice
/// - `0` — repeat forever
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x6E, size = fixed(14), endian = "be")]
pub struct CharacterAnimation {
    pub id: u8,
    pub serial: u32,
    pub action: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub frame_count: u8,
    /// Number of times to play: 1 = once, 2 = twice, 0 = forever.
    pub repeat_count: u16,
    /// Direction: `0x00` = forward, `0x01` = backwards.
    pub direction: u8,
    /// Whether to repeat: `0` = don't repeat, `1` = repeat.
    pub repeat_flag: u8,
    /// Frame delay: `0x00` = fastest, `0xFF` = slowest.
    pub frame_delay: u8,
}

impl CharacterAnimation {
    /// Create an animation that plays once in the forward direction.
    pub fn once(serial: u32, action: u16, frame_count: u8) -> Self {
        Self {
            id: Self::ID,
            serial,
            action,
            _pad0: (),
            frame_count,
            repeat_count: 1,
            direction: 0x00,
            repeat_flag: 0,
            frame_delay: 0,
        }
    }

    /// Create an animation that loops forever in the forward direction.
    pub fn looping(serial: u32, action: u16, frame_count: u8) -> Self {
        Self {
            id: Self::ID,
            serial,
            action,
            _pad0: (),
            frame_count,
            repeat_count: 0,
            direction: 0x00,
            repeat_flag: 1,
            frame_delay: 0,
        }
    }
}

// ── 0xE2 NewCharacterAnimation (10 bytes, fixed, S→C) ─────────────────────

/// Packet 0xE2 — New Character Animation (10 bytes, fixed, S→C)
///
/// Used by the Kingdom Reborn (KR) client to trigger an animation on a
/// mobile.  Supersedes [`CharacterAnimation`] (0x6E) for KR clients.
///
/// # Action types
///
/// Refer to the KR animation tables for valid `action_type` / `sub_action`
/// combinations; they differ from the classic client values used by
/// [`CharacterAnimation`].
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xE2, size = fixed(10), endian = "be")]
pub struct NewCharacterAnimation {
    pub id: u8,
    pub serial: u32,
    /// High-level animation category.
    pub action_type: u16,
    /// Sub-animation within the category.
    pub sub_action: u16,
    /// Further sub-classification; purpose not fully documented.
    pub sub_sub_action: u8,
}

impl NewCharacterAnimation {
    /// Create a new animation command.
    pub fn new(serial: u32, action_type: u16, sub_action: u16, sub_sub_action: u8) -> Self {
        Self {
            id: Self::ID,
            serial,
            action_type,
            sub_action,
            sub_sub_action,
        }
    }
}

// ── 0xAF DisplayDeathAction (13 bytes, fixed, S→C) ────────────────────────

/// Packet 0xAF — Display Death Action (13 bytes, fixed, S→C)
///
/// Sent by the server when a character dies, associating the player's
/// serial with the corpse item that will appear in the world.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xAF
/// BYTE[4]  player_id   — serial of the dying player
/// BYTE[4]  corpse_id   — serial of the corpse item
/// BYTE[4]  unknown     — always 0x00000000
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xAF, size = fixed(13), endian = "be")]
pub struct DisplayDeathAction {
    pub id: u8,
    /// Serial of the player who died.
    pub player_id: u32,
    /// Serial of the corpse item placed in the world.
    pub corpse_id: u32,
    /// Reserved, always zero.
    #[binary(pad = 4)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _unknown: (),
}

impl DisplayDeathAction {
    /// Create a new death action packet.
    pub fn new(player_id: u32, corpse_id: u32) -> Self {
        Self { id: Self::ID, player_id, corpse_id, _unknown: () }
    }
}
