//! Map / cartography packets.
//!
//! | Packet | Name                              | Direction |
//! |--------|-----------------------------------|-----------|
//! | 0x56   | [`MapPacket`]                     | Both      |
//! | 0x90   | [`MapMessage`]                    | S→C       |
//! | 0xF5   | [`NewMapMessage`]                 | S→C       |

use std::fmt;

use u_io::{BE, BinaryWriter, Decode, Encode, packet_reader};
use macros::Packet;

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── 0x90 MapMessage (19 bytes, fixed, S→C) ────────────────────────────────

/// Packet 0x90 — Map Message (19 bytes, fixed, S→C)
///
/// Sent by the server to open a map item (treasure or cartography map).
/// Defines the map's gump art and the coordinate bounds of the region shown.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x90
/// BYTE[4]  map_serial        — serial of the map item
/// BYTE[2]  gump_art          — gump art ID (typically 0x139D)
/// BYTE[2]  upper_left_x      — upper-left corner X of mapped region
/// BYTE[2]  upper_left_y      — upper-left corner Y of mapped region
/// BYTE[2]  lower_right_x     — lower-right corner X of mapped region
/// BYTE[2]  lower_right_y     — lower-right corner Y of mapped region
/// BYTE[2]  gump_width        — width of the gump in pixels
/// BYTE[2]  gump_height       — height of the gump in pixels
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x90, size = fixed(19), endian = "be")]
pub struct MapMessage {
    pub id: u8,
    /// Serial of the map item in the world.
    pub map_serial: u32,
    /// Gump art ID used for the corner image (typically `0x139D`).
    pub gump_art: u16,
    /// Upper-left X coordinate of the region shown on the map.
    pub upper_left_x: u16,
    /// Upper-left Y coordinate of the region shown on the map.
    pub upper_left_y: u16,
    /// Lower-right X coordinate of the region shown on the map.
    pub lower_right_x: u16,
    /// Lower-right Y coordinate of the region shown on the map.
    pub lower_right_y: u16,
    /// Width of the displayed gump in pixels.
    pub gump_width: u16,
    /// Height of the displayed gump in pixels.
    pub gump_height: u16,
}

// ── 0xF5 NewMapMessage (21 bytes, fixed, S→C) ─────────────────────────────

/// Packet 0xF5 — New Map Message (21 bytes, fixed, S→C)
///
/// Extended version of [`MapMessage`] (0x90) that adds a [`NewMapMessage::facet_id`] field
/// identifying which facet (map shard) the region belongs to.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xF5
/// BYTE[4]  map_serial        — serial of the map item
/// BYTE[2]  gump_art          — gump art ID for the corner image (typically 0x139D)
/// BYTE[2]  upper_left_x      — upper-left corner X of mapped region
/// BYTE[2]  upper_left_y      — upper-left corner Y of mapped region
/// BYTE[2]  lower_right_x     — lower-right corner X of mapped region
/// BYTE[2]  lower_right_y     — lower-right corner Y of mapped region
/// BYTE[2]  gump_width        — width of the gump in pixels
/// BYTE[2]  gump_height       — height of the gump in pixels
/// BYTE[2]  facet_id          — facet ID matching facetXX.mul numbering
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xF5, size = fixed(21), endian = "be")]
pub struct NewMapMessage {
    pub id: u8,
    /// Serial of the map item in the world.
    pub map_serial: u32,
    /// Gump art ID used for the corner image (typically `0x139D`).
    pub gump_art: u16,
    /// Upper-left X coordinate of the region shown on the map.
    pub upper_left_x: u16,
    /// Upper-left Y coordinate of the region shown on the map.
    pub upper_left_y: u16,
    /// Lower-right X coordinate of the region shown on the map.
    pub lower_right_x: u16,
    /// Lower-right Y coordinate of the region shown on the map.
    pub lower_right_y: u16,
    /// Width of the displayed gump in pixels.
    pub gump_width: u16,
    /// Height of the displayed gump in pixels.
    pub gump_height: u16,
    /// Facet ID corresponding to the `facetXX.mul` file numbering.
    pub facet_id: u16,
}

// ── 0x56 MapPacket helpers ────────────────────────────────────────────────

/// Action flag for [`MapPacket`].
///
/// | Value | Direction | Meaning                                  |
/// |-------|-----------|------------------------------------------|
/// | 1     | C→S       | Add pin                                  |
/// | 2     | C→S       | Insert new pin                           |
/// | 3     | C→S       | Change pin                               |
/// | 4     | C→S       | Remove pin                               |
/// | 5     | C→S       | Clear all pins                           |
/// | 6     | C→S       | Toggle edit map (request from client)    |
/// | 7     | S→C       | Reply to action 6                        |
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MapAction {
    /// Add a new pin at the given position.
    AddPin { pin_id: u8, x: u16, y: u16 },
    /// Insert a new pin (shifts existing pins).
    InsertPin { pin_id: u8, x: u16, y: u16 },
    /// Move an existing pin to a new position.
    ChangePin { pin_id: u8, x: u16, y: u16 },
    /// Remove the pin with the given ID.
    RemovePin { pin_id: u8, x: u16, y: u16 },
    /// Remove all pins from the map.
    ClearPins,
    /// Client requests to toggle edit mode.
    ToggleEditMap,
    /// Server reply to [`MapAction::ToggleEditMap`].
    ///
    /// `plotting` is `true` when plotting (edit) mode is now active.
    EditMapReply { plotting: bool },
    /// Unknown action flag.
    Unknown { action: u8, pin_id: u8, x: u16, y: u16 },
}

impl MapAction {
    /// Wire value of the `action` byte.
    pub fn action_flag(&self) -> u8 {
        match self {
            Self::AddPin { .. }          => 1,
            Self::InsertPin { .. }       => 2,
            Self::ChangePin { .. }       => 3,
            Self::RemovePin { .. }       => 4,
            Self::ClearPins              => 5,
            Self::ToggleEditMap          => 6,
            Self::EditMapReply { .. }    => 7,
            Self::Unknown { action, .. } => *action,
        }
    }
}

impl fmt::Display for MapAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddPin { pin_id, x, y }    => write!(f, "AddPin(pin={pin_id}, x={x}, y={y})"),
            Self::InsertPin { pin_id, x, y } => write!(f, "InsertPin(pin={pin_id}, x={x}, y={y})"),
            Self::ChangePin { pin_id, x, y } => write!(f, "ChangePin(pin={pin_id}, x={x}, y={y})"),
            Self::RemovePin { pin_id, x, y } => write!(f, "RemovePin(pin={pin_id}, x={x}, y={y})"),
            Self::ClearPins                  => write!(f, "ClearPins"),
            Self::ToggleEditMap              => write!(f, "ToggleEditMap"),
            Self::EditMapReply { plotting }  => write!(f, "EditMapReply(plotting={plotting})"),
            Self::Unknown { action, pin_id, x, y } =>
                write!(f, "Unknown(action=0x{action:02X}, pin={pin_id}, x={x}, y={y})"),
        }
    }
}

// ── 0x56 MapPacket (11 bytes, fixed, bidirectional) ───────────────────────

/// Packet 0x56 — Map Packet / cartography & treasure map (11 bytes, both)
///
/// Used by the client to add, move, or remove pins on a map item,
/// and by the server to confirm or reply to those actions.
/// X/Y coordinates are relative to the upper-left corner of the map, in pixels.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x56
/// BYTE[4]  map_serial   — serial of the map object
/// BYTE[1]  action       — action flag (1–7, see [`MapAction`])
/// BYTE[1]  pin_id       — pin ID, or plotting flag (action 7 only)
/// BYTE[2]  x            — X offset from map upper-left, in pixels
/// BYTE[2]  y            — Y offset from map upper-left, in pixels
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MapPacket {
    /// Serial of the map item in the world.
    pub map_serial: u32,
    /// Decoded action and its associated data.
    pub action: MapAction,
}

impl MapPacket {
    /// Construct an `AddPin` packet.
    pub fn add_pin(map_serial: u32, pin_id: u8, x: u16, y: u16) -> Self {
        Self { map_serial, action: MapAction::AddPin { pin_id, x, y } }
    }

    /// Construct a `RemovePin` packet.
    pub fn remove_pin(map_serial: u32, pin_id: u8, x: u16, y: u16) -> Self {
        Self { map_serial, action: MapAction::RemovePin { pin_id, x, y } }
    }

    /// Construct a `ClearPins` packet.
    pub fn clear_pins(map_serial: u32) -> Self {
        Self { map_serial, action: MapAction::ClearPins }
    }

    /// Construct a `ToggleEditMap` packet (C→S).
    pub fn toggle_edit(map_serial: u32) -> Self {
        Self { map_serial, action: MapAction::ToggleEditMap }
    }

    /// Construct an `EditMapReply` packet (S→C).
    pub fn edit_reply(map_serial: u32, plotting: bool) -> Self {
        Self { map_serial, action: MapAction::EditMapReply { plotting } }
    }
}

impl fmt::Display for MapPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MapPacket(serial=0x{:08X}, {})", self.map_serial, self.action)
    }
}

impl ManualPacket for MapPacket {
    const ID: u8 = 0x56;
    const SIZE: PacketSize = PacketSize::Fixed(11);

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 11, false)?;

        let map_serial: u32 = Decode::decode(&mut r)?;
        let action_flag: u8 = Decode::decode(&mut r)?;
        let pin_id: u8      = Decode::decode(&mut r)?;
        let x: u16          = Decode::decode(&mut r)?;
        let y: u16          = Decode::decode(&mut r)?;

        let action = match action_flag {
            1 => MapAction::AddPin    { pin_id, x, y },
            2 => MapAction::InsertPin { pin_id, x, y },
            3 => MapAction::ChangePin { pin_id, x, y },
            4 => MapAction::RemovePin { pin_id, x, y },
            5 => MapAction::ClearPins,
            6 => MapAction::ToggleEditMap,
            7 => MapAction::EditMapReply { plotting: pin_id != 0 },
            _ => MapAction::Unknown { action: action_flag, pin_id, x, y },
        };

        Ok(Self { map_serial, action })
    }
}

impl Encode<BE> for MapPacket {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u32(self.map_serial);
        w.put_u8(self.action.action_flag());

        match &self.action {
            MapAction::AddPin    { pin_id, x, y }
            | MapAction::InsertPin { pin_id, x, y }
            | MapAction::ChangePin { pin_id, x, y }
            | MapAction::RemovePin { pin_id, x, y } => {
                w.put_u8(*pin_id);
                w.put_u16(*x);
                w.put_u16(*y);
            }
            MapAction::ClearPins | MapAction::ToggleEditMap => {
                w.put_u8(0);
                w.put_u16(0);
                w.put_u16(0);
            }
            MapAction::EditMapReply { plotting } => {
                w.put_u8(*plotting as u8);
                w.put_u16(0);
                w.put_u16(0);
            }
            MapAction::Unknown { pin_id, x, y, .. } => {
                w.put_u8(*pin_id);
                w.put_u16(*x);
                w.put_u16(*y);
            }
        }
    }
}
