//! Object interaction packets.
//!
//! Covers picking up, wearing, equipping, deleting, clicking on
//! world objects and mobiles, combat requests, and container content display.

use bytes::Bytes;
use u_io::{BE, BinaryReader, BinaryWriter, Decode, Encode, ListU16, NullString, RawBytes, ReadPrimitives, packet_reader};
use macros::{Packet, Decode as DecodeMacro, Encode as EncodeMacro, WireEnum};

use crate::layer::Layer;
use crate::traits::{ManualPacket, PacketError, PacketSize, BasicPacket};

// ── 0x34 GetMobileStatus (10 bytes, fixed, C→S) ───────────────────────────

/// What the client is requesting in packet 0x34.
///
/// | Wire value | Meaning                                              |
/// |------------|------------------------------------------------------|
/// | 0x04       | Basic status bar — server replies with 0x11          |
/// | 0x05       | Skills list — server replies with 0x3A               |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum MobileStatusRequest {
    /// Request the mobile's basic status bar (0x11 `StatusBarInfo` response).
    #[wire_enum(0x04, "status")]
    Status,
    /// Request the mobile's skills list (0x3A `SendSkills` response).
    #[wire_enum(0x05, "skills")]
    Skills,
    /// Unknown / future request type.
    #[wire_enum(unknown)]
    Unknown(u8),
}

/// Packet 0x34 — Get Mobile Status (10 bytes, fixed, C→S)
///
/// Sent by the client to request a mobile's status or skills.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x34, size = fixed(10), endian = "be")]
pub struct GetMobileStatus {
    pub id: u8,
    pub pattern: u32,
    pub request_type: MobileStatusRequest,
    pub serial: u32,
}

impl GetMobileStatus {
    /// Request basic status (0x11 response).
    pub fn status(serial: u32) -> Self {
        Self { id: Self::ID, pattern: 0xEDEDEDED, request_type: MobileStatusRequest::Status, serial }
    }

    /// Request skills (0x3A response).
    pub fn skills(serial: u32) -> Self {
        Self { id: Self::ID, pattern: 0xEDEDEDED, request_type: MobileStatusRequest::Skills, serial }
    }
}

// ── 0x6C TargetCursor (19 bytes, fixed, bidirectional) ─────────────────────

/// Packet 0x6C — Target Cursor Commands (19 bytes, fixed, bidirectional)
///
/// Server sends this to request the client to target something.
/// Client responds with the same packet containing the target info.
///
/// # Cursor target
/// - `0`: Select Object
/// - `1`: Select X, Y, Z
///
/// # Cursor type
/// - `0`: Neutral
/// - `1`: Harmful
/// - `2`: Helpful
/// - `3`: Cancel current targeting (server-sent)
///
/// To cancel a pending target cursor, the server sends cursor_type = 3
/// with cursor_id = 0.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x6C, size = fixed(19), endian = "be")]
pub struct TargetCursor {
    pub id: u8,
    pub cursor_target: u8,
    pub cursor_id: u32,
    pub cursor_type: u8,
    /// Serial of the clicked object (client response only).
    pub target_serial: u32,
    pub x: u16,
    pub y: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub z: i8,
    /// Graphic of clicked static tile, 0 for map/landscape (client response only).
    pub graphic: u16,
}

// ── 0xAA AttackResponse (5 bytes, fixed, S→C) ─────────────────────────────

/// Packet 0xAA — Allow/Refuse Attack (5 bytes, fixed, S→C)
///
/// Sent by the server to confirm or refuse an attack target.
/// When `serial` is `0x00000000` the attack is refused.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xAA, size = fixed(5), endian = "be")]
pub struct AttackResponse {
    pub id: u8,
    pub serial: u32,
}

impl AttackResponse {
    /// Attack refused (no target).
    pub fn refused() -> Self {
        Self { id: Self::ID, serial: 0 }
    }

    /// Whether this response is a refusal.
    pub fn is_refused(&self) -> bool {
        self.serial == 0
    }
}

// ── 0x99 MultiPlacement (26 bytes, fixed, both) ──────────────────────────

/// Packet 0x99 — Multi (house/boat) placement target (26 bytes, fixed, both).
///
/// Sent **server→client** (`request == 0x01`) to begin a house-placement
/// preview: the client shows a moving multi outline at the cursor and a
/// targeting cursor (`cursor_target == 3`, i.e. multi target).  The
/// **client→server** form (`request == 0x00`) is the placement confirmation,
/// though most clients answer via a `0x6C` `TargetCursor` instead.
///
/// ## Wire layout
///
/// ```text
/// BYTE[1]  cmd            = 0x99
/// BYTE[1]  request        (0x01 from server, 0x00 from client)
/// BYTE[4]  deed_serial    (ID of the deed item being placed)
/// BYTE[12] unknown        (all 0)
/// BYTE[2]  multi_model    (item model − 0x4000)
/// BYTE[6]  unknown        (all 0)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x99, size = fixed(26), endian = "be")]
pub struct MultiPlacement {
    pub id: u8,
    /// `0x01` when sent by the server, `0x00` when sent by the client.
    pub request: u8,
    /// Serial of the deed item being placed.
    pub deed_serial: u32,
    #[binary(pad = 12)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    /// Multi model — the multi id (i.e. `item_graphic − 0x4000`).
    pub multi_model: u16,
    #[binary(pad = 6)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad1: (),
}

impl MultiPlacement {
    /// Build a server→client placement-preview request for `deed_serial`
    /// showing the multi `multi_model` (the multi id, *not* offset by 0x4000).
    pub fn server_request(deed_serial: u32, multi_model: u16) -> Self {
        Self {
            id: Self::ID,
            request: 0x01,
            deed_serial,
            _pad0: (),
            multi_model,
            _pad1: (),
        }
    }
}

// ── 0x2F FightOccurring (10 bytes, fixed, S→C) ───────────────────────────

/// Packet 0x2F — Fight Occurring (10 bytes, fixed, S→C)
///
/// Sent by the server to notify the client that a fight is occurring
/// between two entities.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x2F, size = fixed(10), endian = "be")]
pub struct FightOccurring {
    pub id: u8,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    /// Serial of the attacker.
    pub attacker: u32,
    /// Serial of the defender.
    pub defender: u32,
}

impl FightOccurring {
    /// Create a new fight notification.
    pub fn new(attacker: u32, defender: u32) -> Self {
        Self {
            id: Self::ID,
            _pad0: (),
            attacker,
            defender,
        }
    }
}

// ── 0x08 DropItem (version-dependent 14/15 bytes, C→S) ───────────────────

/// Packet 0x08 — Drop Item, legacy format (14 bytes, fixed, C→S)
///
/// Used by 2D clients before 6.0.1.7. No grid index.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x08, size = fixed(14), endian = "be")]
pub struct DropItemLegacy {
    pub id: u8,
    pub serial: u32,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub container_serial: u32,
}

/// Packet 0x08 — Drop Item, modern format (15 bytes, fixed, C→S)
///
/// Used by UOKR+ and 2D clients 6.0.1.7+. Includes a grid index byte.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x08, size = fixed(15), endian = "be")]
pub struct DropItemModern {
    pub id: u8,
    pub serial: u32,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub grid_index: u8,
    pub container_serial: u32,
}

/// Packet 0x08 — Drop Item (version-dependent, C→S)
///
/// Two wire formats:
/// - **Legacy** (pre-6.0.1.7): 14 bytes, no grid index
/// - **Modern** (6.0.1.7+ / UOKR+): 15 bytes, with `grid_index`
///
/// Format is auto-detected by packet length.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DropItem {
    Legacy(DropItemLegacy),
    Modern(DropItemModern),
}

impl ManualPacket for DropItem {
    const ID: u8 = 0x08;
    const SIZE: PacketSize = PacketSize::Fixed(15);

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        if data.is_empty() {
            return Err(u_io::DecodeError::Truncated.into());
        }
        if data[0] != Self::ID {
            return Err(PacketError::BadId { expected: Self::ID, actual: data[0] });
        }
        match data.len() {
            14 => {
                let mut r = BinaryReader::<BE>::new(data);
                Ok(Self::Legacy(Decode::decode(&mut r)?))
            }
            15 => {
                let mut r = BinaryReader::<BE>::new(data);
                Ok(Self::Modern(Decode::decode(&mut r)?))
            }
            n => Err(u_io::DecodeError::Other(
                format!("unexpected 0x08 DropItem length: {n}, expected 14 or 15"),
            ).into()),
        }
    }
}

impl Encode<BE> for DropItem {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        match self {
            Self::Legacy(p) => p.encode(w),
            Self::Modern(p) => p.encode(w),
        }
    }
}

// ── 0x25 AddItemToContainer (version-dependent 20/21 bytes, S→C) ─────────

/// Packet 0x25 — Add Item To Container, legacy format (20 bytes, S→C)
///
/// Used by 2D clients before 6.0.1.7. No grid/slot index.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x25, size = fixed(20), endian = "be")]
pub struct AddItemToContainerLegacy {
    pub id: u8,
    pub serial: u32,
    pub graphic: u16,
    pub graphic_offset: u8,
    pub amount: u16,
    pub x: u16,
    pub y: u16,
    pub container_serial: u32,
    pub color: u16,
}

/// Packet 0x25 — Add Item To Container, modern format (21 bytes, S→C)
///
/// Used by UOKR+ and 2D clients 6.0.1.7+. Includes a slot/grid index.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x25, size = fixed(21), endian = "be")]
pub struct AddItemToContainerModern {
    pub id: u8,
    pub serial: u32,
    pub graphic: u16,
    pub graphic_offset: u8,
    pub amount: u16,
    pub x: u16,
    pub y: u16,
    pub slot_index: u8,
    pub container_serial: u32,
    pub color: u16,
}

/// Packet 0x25 — Add Item To Container (version-dependent, S→C)
///
/// Two wire formats:
/// - **Legacy** (pre-6.0.1.7): 20 bytes, no slot index
/// - **Modern** (6.0.1.7+ / UOKR+): 21 bytes, with `slot_index`
///
/// Format is auto-detected by packet length.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AddItemToContainer {
    Legacy(AddItemToContainerLegacy),
    Modern(AddItemToContainerModern),
}

impl ManualPacket for AddItemToContainer {
    const ID: u8 = 0x25;
    const SIZE: PacketSize = PacketSize::Fixed(21);

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        if data.is_empty() {
            return Err(u_io::DecodeError::Truncated.into());
        }
        if data[0] != Self::ID {
            return Err(PacketError::BadId { expected: Self::ID, actual: data[0] });
        }
        match data.len() {
            20 => {
                let mut r = BinaryReader::<BE>::new(data);
                Ok(Self::Legacy(Decode::decode(&mut r)?))
            }
            21 => {
                let mut r = BinaryReader::<BE>::new(data);
                Ok(Self::Modern(Decode::decode(&mut r)?))
            }
            n => Err(u_io::DecodeError::Other(
                format!("unexpected 0x25 AddItemToContainer length: {n}, expected 20 or 21"),
            ).into()),
        }
    }
}

impl Encode<BE> for AddItemToContainer {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        match self {
            Self::Legacy(p) => p.encode(w),
            Self::Modern(p) => p.encode(w),
        }
    }
}

impl AddItemToContainer {
    /// Serial of the item being added.
    pub fn serial(&self) -> u32 {
        match self {
            Self::Legacy(p) => p.serial,
            Self::Modern(p) => p.serial,
        }
    }

    /// Serial of the container the item is placed into.
    pub fn container_serial(&self) -> u32 {
        match self {
            Self::Legacy(p) => p.container_serial,
            Self::Modern(p) => p.container_serial,
        }
    }

    /// Item graphic.
    pub fn graphic(&self) -> u16 {
        match self {
            Self::Legacy(p) => p.graphic,
            Self::Modern(p) => p.graphic,
        }
    }

    /// Stack amount.
    pub fn amount(&self) -> u16 {
        match self {
            Self::Legacy(p) => p.amount,
            Self::Modern(p) => p.amount,
        }
    }

    /// Gump-relative X position.
    pub fn x(&self) -> u16 {
        match self {
            Self::Legacy(p) => p.x,
            Self::Modern(p) => p.x,
        }
    }

    /// Gump-relative Y position.
    pub fn y(&self) -> u16 {
        match self {
            Self::Legacy(p) => p.y,
            Self::Modern(p) => p.y,
        }
    }

    /// Item colour / hue.
    pub fn color(&self) -> u16 {
        match self {
            Self::Legacy(p) => p.color,
            Self::Modern(p) => p.color,
        }
    }

    /// Grid index (modern format only; `None` for legacy).
    pub fn grid_index(&self) -> Option<u8> {
        match self {
            Self::Legacy(_) => None,
            Self::Modern(p) => Some(p.slot_index),
        }
    }
}


/// Packet 0x06 — Double Click (5 bytes, fixed, C→S)
///
/// Sent by the client when the player double-clicks an object or mobile.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x06, size = fixed(5), endian = "be")]
pub struct DoubleClick {
    pub id: u8,
    pub serial: u32,
}

// ── 0x07 PickUpItem (7 bytes, fixed, C→S) ─────────────────────────────────

/// Packet 0x07 — Pick Up Item (7 bytes, fixed, C→S)
///
/// Sent by the client when the player picks up an item from the world
/// or a container.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x07, size = fixed(7), endian = "be")]
pub struct PickUpItem {
    pub id: u8,
    pub serial: u32,
    pub amount: u16,
}

// ── 0x27 RejectMoveItem (2 bytes, fixed, S→C) ─────────────────────────────

/// Reason the server rejected a pick-up / move-item request.
///
/// | Wire value | Meaning                         |
/// |------------|---------------------------------|
/// | 0x00       | Cannot lift the item            |
/// | 0x01       | Out of range                    |
/// | 0x02       | Out of sight                    |
/// | 0x03       | Belongs to another              |
/// | 0x04       | Already holding something       |
/// | 0x05       | Empty message on client         |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum RejectMoveItemReason {
    /// Cannot lift the item.
    #[wire_enum(0x00, "cannot_lift")]
    CannotLift,
    /// Item is out of range.
    #[wire_enum(0x01, "out_of_range")]
    OutOfRange,
    /// Item is out of sight.
    #[wire_enum(0x02, "out_of_sight")]
    OutOfSight,
    /// Item belongs to another player.
    #[wire_enum(0x03, "belongs_to_another")]
    BelongsToAnother,
    /// Player is already holding something.
    #[wire_enum(0x04, "already_holding")]
    AlreadyHolding,
    /// Empty message on client (no visible feedback).
    #[wire_enum(0x05, "empty_message")]
    EmptyMessage,
    /// Unknown reason code.
    #[wire_enum(unknown)]
    Unknown(u8),
}

/// Packet 0x27 — Reject Move Item Request (2 bytes, fixed, S→C)
///
/// Sent by the server when it refuses a pick-up or move-item request.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x27
/// BYTE[1]  reason   — see [`RejectMoveItemReason`]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x27, size = fixed(2), endian = "be")]
pub struct RejectMoveItem {
    pub id: u8,
    /// Reason the server rejected the move request.
    pub reason: RejectMoveItemReason,
}

impl RejectMoveItem {
    /// Convenience constructor.
    pub fn new(reason: RejectMoveItemReason) -> Self {
        Self { id: Self::ID, reason }
    }
}

// ── 0x09 SingleClick (5 bytes, fixed, C→S) ────────────────────────────────

/// Packet 0x09 — Single Click (5 bytes, fixed, C→S)
///
/// Sent by the client when the player single-clicks an object or mobile.
/// The server typically responds with a name label (0x1C).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x09, size = fixed(5), endian = "be")]
pub struct SingleClick {
    pub id: u8,
    pub serial: u32,
}

// ── 0x13 WearItem (10 bytes, fixed, C→S) ──────────────────────────────────

/// Packet 0x13 — Drop → Wear Item (10 bytes, fixed, C→S)
///
/// Sent by the client to equip an item onto a character. Note: the
/// `layer` value should not be trusted by the server.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x13, size = fixed(10), endian = "be")]
pub struct WearItem {
    pub id: u8,
    pub item_serial: u32,
    pub layer: Layer,
    pub player_serial: u32,
}

// ── 0x1D DeleteObject (5 bytes, fixed, S→C) ───────────────────────────────

/// Packet 0x1D — Delete Object (5 bytes, fixed, S→C)
///
/// Sent by the server to remove an item or mobile from the client's view.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x1D, size = fixed(5), endian = "be")]
pub struct DeleteObject {
    pub id: u8,
    pub serial: u32,
}

// ── 0x2E EquipItem (15 bytes, fixed, S→C) ─────────────────────────────────

/// Packet 0x2E — Equip Item (15 bytes, fixed, S→C)
///
/// Sent by the server to show an item equipped on a mobile.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x2E, size = fixed(15), endian = "be")]
pub struct EquipItem {
    pub id: u8,
    pub item_serial: u32,
    pub graphic: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub layer: Layer,
    pub player_serial: u32,
    pub color: u16,
}

// ── 0x24 DrawContainer (version-dependent, S→C) ───────────────────────────
//
// Format depends on client version:
// - Clients < 7.0.9.0 (CV_7090): 7 bytes (id + serial + gump_model)
// - Clients >= 7.0.9.0 (CV_7090): 9 bytes (id + serial + gump_model + u16)
//   The trailing u16 is a container-grid/draw flag; use 0x0000 for standard
//   containers (the client interprets non-zero as "open in grid layout").

/// Packet 0x24 — Draw Container, legacy format (7 bytes, fixed, S→C)
///
/// Used by clients before 7.0.9.0. Opens a container gump on the client.
/// Model `0x003C` is the standard backpack; shops use `0x0030`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x24, size = fixed(7), endian = "be")]
pub struct DrawContainerLegacy {
    pub id: u8,
    pub serial: u32,
    pub gump_model: u16,
}

/// Packet 0x24 — Draw Container, modern format (9 bytes, fixed, S→C)
///
/// Used by clients 7.0.9.0+ (High Seas). Adds a trailing `u16` for the
/// container-grid layout flag. Pass `0x0000` for normal containers;
/// `0x7D` enables the new backpack grid display.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x24, size = fixed(9), endian = "be")]
pub struct DrawContainerModern {
    pub id: u8,
    pub serial: u32,
    pub gump_model: u16,
    /// Container-grid layout flag. `0x0000` = standard gump.
    /// `0x007D` = new grid-style backpack (7.0.9.0+).
    pub draw_grid: u16,
}

/// Packet 0x24 — Draw Container (version-dependent, S→C)
///
/// Two wire formats:
/// - **Legacy** (pre-7.0.9.0): 7 bytes, no grid flag
/// - **Modern** (7.0.9.0+): 9 bytes, with `draw_grid` field
///
/// Use [`DrawContainer::new`] to pick the correct format by client version.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DrawContainer {
    Legacy(DrawContainerLegacy),
    Modern(DrawContainerModern),
}

impl ManualPacket for DrawContainer {
    const ID: u8 = 0x24;
    const SIZE: PacketSize = PacketSize::Fixed(9);

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        if data.is_empty() {
            return Err(u_io::DecodeError::Truncated.into());
        }
        if data[0] != Self::ID {
            return Err(PacketError::BadId { expected: Self::ID, actual: data[0] });
        }
        match data.len() {
            7 => {
                let mut r = BinaryReader::<BE>::new(data);
                Ok(Self::Legacy(Decode::decode(&mut r)?))
            }
            9 => {
                let mut r = BinaryReader::<BE>::new(data);
                Ok(Self::Modern(Decode::decode(&mut r)?))
            }
            n => Err(u_io::DecodeError::Other(
                format!("unexpected 0x24 DrawContainer length: {n}, expected 7 or 9"),
            ).into()),
        }
    }
}

impl Encode<BE> for DrawContainer {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        match self {
            Self::Legacy(p) => p.encode(w),
            Self::Modern(p) => p.encode(w),
        }
    }
}

impl DrawContainer {
    /// Create with the correct format for the given client version.
    ///
    /// Clients before 7.0.9.0 (`CV_7090`) use the legacy (7-byte) format;
    /// clients from 7.0.9.0 onwards use the modern (9-byte) format with
    /// `draw_grid = 0x0000` (standard gump layout).
    pub fn new(serial: u32, gump_model: u16, version: u_core::ProtocolVersion) -> Self {
        if version >= u_core::ProtocolVersion::CV_7090 {
            Self::Modern(DrawContainerModern {
                id: Self::ID,
                serial,
                gump_model,
                draw_grid: 0x0000,
            })
        } else {
            Self::Legacy(DrawContainerLegacy {
                id: Self::ID,
                serial,
                gump_model,
            })
        }
    }

    /// Serial of the container being opened.
    pub fn serial(&self) -> u32 {
        match self {
            Self::Legacy(p) => p.serial,
            Self::Modern(p) => p.serial,
        }
    }

    /// Gump model / object type of the container.
    pub fn gump_model(&self) -> u16 {
        match self {
            Self::Legacy(p) => p.gump_model,
            Self::Modern(p) => p.gump_model,
        }
    }
}

// ── 0x3C ContainerContent (dynamic, S→C) ──────────────────────────────────

/// A container item in the **legacy** format (pre-6.0.1.7, 19 bytes per item).
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct ContainerItemLegacy {
    pub serial: u32,
    pub graphic: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub amount: u16,
    pub x: u16,
    pub y: u16,
    pub container_serial: u32,
    pub color: u16,
}

/// A container item in the **modern** format (6.0.1.7+ / KR 2.45.5.6+, 20 bytes per item).
///
/// Includes a `grid_index` byte for the backpack grid layout.
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct ContainerItemModern {
    pub serial: u32,
    pub graphic: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub amount: u16,
    pub x: u16,
    pub y: u16,
    pub grid_index: u8,
    pub container_serial: u32,
    pub color: u16,
}

/// Packet 0x3C — Container Content / Add Items to Container (dynamic, S→C)
///
/// Populates a container with items. Two wire formats exist:
///
/// - **Legacy** (pre-6.0.1.7): 19 bytes per item (no grid index)
/// - **Modern** (6.0.1.7+): 20 bytes per item (with `grid_index`)
///
/// The format is detected automatically by dividing the item-data portion
/// of the packet by the item count.
///
/// For shops this packet provides the item list; prices/descriptions
/// come via packet 0x74.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ContainerContent {
    Legacy(Vec<ContainerItemLegacy>),
    Modern(Vec<ContainerItemModern>),
}

impl ManualPacket for ContainerContent {
    const ID: u8 = 0x3C;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 5, false)?;
        let pkt_len: u16 = Decode::decode(&mut r)?;
        let count: u16 = Decode::decode(&mut r)?;

        // Detect format: (total_len - 5 byte header) / item_count
        let items_data_len = (pkt_len as usize).saturating_sub(5);
        let modern = if count == 0 {
            true
        } else {
            items_data_len / count as usize >= Self::ITEM_SIZE_MODERN
        };

        if modern {
            let mut items = Vec::with_capacity(count as usize);
            for _ in 0..count {
                items.push(ContainerItemModern::decode(&mut r)?);
            }
            Ok(Self::Modern(items))
        } else {
            let mut items = Vec::with_capacity(count as usize);
            for _ in 0..count {
                items.push(ContainerItemLegacy::decode(&mut r)?);
            }
            Ok(Self::Legacy(items))
        }
    }

    /// Encode into [`Bytes`] with pre-computed capacity.
    fn to_bytes(&self) -> Bytes {
        let cap = 5 + self.count() * Self::ITEM_SIZE_MODERN;
        let mut writer = BinaryWriter::<BE>::with_capacity(cap);
        self.encode(&mut writer);
        writer.set_u16_at(1, writer.len() as u16);
        writer.finish()
    }
}

impl ContainerContent {
    /// Per-item wire size for each format.
    const ITEM_SIZE_MODERN: usize = 20;

    /// Number of items regardless of format.
    pub fn count(&self) -> usize {
        match self {
            Self::Legacy(v) => v.len(),
            Self::Modern(v) => v.len(),
        }
    }

    /// Container serial from the first item, if any.
    ///
    /// All items in a `ContainerContent` packet share the same
    /// `container_serial`, so inspecting the first one is sufficient.
    pub fn container_serial(&self) -> Option<u32> {
        match self {
            Self::Legacy(v) => v.first().map(|i| i.container_serial),
            Self::Modern(v) => v.first().map(|i| i.container_serial),
        }
    }

    /// Returns `true` if this uses the modern (20-byte/item) format.
    pub fn is_modern(&self) -> bool {
        matches!(self, Self::Modern(_))
    }

    /// Upsert an item from a `0x25 AddItemToContainer` packet.
    ///
    /// If an item with the same serial already exists it is replaced;
    /// otherwise the new item is appended.  The variant
    /// (Legacy/Modern) of `self` determines which conversion is used.
    pub fn upsert_from_add_item(&mut self, add: &AddItemToContainer) {
        match self {
            Self::Legacy(items) => {
                let item = match add {
                    AddItemToContainer::Legacy(a) => ContainerItemLegacy {
                        serial: a.serial,
                        graphic: a.graphic,
                        _pad0: (),
                        amount: a.amount,
                        x: a.x,
                        y: a.y,
                        container_serial: a.container_serial,
                        color: a.color,
                    },
                    AddItemToContainer::Modern(a) => ContainerItemLegacy {
                        serial: a.serial,
                        graphic: a.graphic,
                        _pad0: (),
                        amount: a.amount,
                        x: a.x,
                        y: a.y,
                        container_serial: a.container_serial,
                        color: a.color,
                    },
                };
                if let Some(existing) = items.iter_mut().find(|i| i.serial == item.serial) {
                    *existing = item;
                } else {
                    items.push(item);
                }
            }
            Self::Modern(items) => {
                let item = match add {
                    AddItemToContainer::Legacy(a) => ContainerItemModern {
                        serial: a.serial,
                        graphic: a.graphic,
                        _pad0: (),
                        amount: a.amount,
                        x: a.x,
                        y: a.y,
                        grid_index: 0,
                        container_serial: a.container_serial,
                        color: a.color,
                    },
                    AddItemToContainer::Modern(a) => ContainerItemModern {
                        serial: a.serial,
                        graphic: a.graphic,
                        _pad0: (),
                        amount: a.amount,
                        x: a.x,
                        y: a.y,
                        grid_index: a.slot_index,
                        container_serial: a.container_serial,
                        color: a.color,
                    },
                };
                if let Some(existing) = items.iter_mut().find(|i| i.serial == item.serial) {
                    *existing = item;
                } else {
                    items.push(item);
                }
            }
        }
    }
    /// Collect the serial of every item in this container content.
    pub fn item_serials(&self) -> Vec<u32> {
        match self {
            Self::Legacy(items) => items.iter().map(|i| i.serial).collect(),
            Self::Modern(items) => items.iter().map(|i| i.serial).collect(),
        }
    }

    /// Remove an item by serial.
    ///
    /// Returns `true` if the item was found and removed.
    pub fn remove_item(&mut self, serial: u32) -> bool {
        match self {
            Self::Legacy(items) => {
                let before = items.len();
                items.retain(|i| i.serial != serial);
                items.len() != before
            }
            Self::Modern(items) => {
                let before = items.len();
                items.retain(|i| i.serial != serial);
                items.len() != before
            }
        }
    }
}

impl Encode<BE> for ContainerContent {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u16(self.count() as u16);

        match self {
            Self::Legacy(items) => {
                for item in items {
                    item.encode(w);
                }
            }
            Self::Modern(items) => {
                for item in items {
                    item.encode(w);
                }
            }
        }
    }
}

// ── 0x89 CorpseClothing (dynamic, S→C) ───────────────────────────────────

/// A single clothing/equipment entry on a corpse.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CorpseClothingEntry {
    /// Equipment layer.
    pub layer: Layer,
    /// Serial of the item in that layer.
    pub item_id: u32,
}

/// Packet 0x89 — Corpse Clothing (dynamic, S→C)
///
/// Sent by the server to describe which items are visually equipped on a
/// corpse container. Each entry maps an equipment layer to an item serial.
/// The list is terminated by a zero layer byte (`0x00`).
///
/// This packet is typically followed immediately by a [`ContainerContent`]
/// (0x3C) packet with the actual item details.
///
/// # Wire format
///
/// ```text
/// BYTE    cmd           (0x89)
/// BYTE[2] blockSize     (total packet length)
/// BYTE[4] corpseID      (serial of the corpse)
///   repeating:
///     BYTE    itemLayer   (equipment layer, 0x00 = end)
///     BYTE[4] itemID      (serial of the equipped item)
/// BYTE    terminator    (0x00)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CorpseClothing {
    /// Serial of the corpse container.
    pub corpse_id: u32,
    /// Clothing/equipment entries on the corpse.
    pub items: Vec<CorpseClothingEntry>,
}

impl ManualPacket for CorpseClothing {
    const ID: u8 = 0x89;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 8, true)?;
        let corpse_id: u32 = Decode::decode(&mut r)?;

        // Equipment list — terminated by layer == 0x00 (Layer::Invalid)
        let mut items = Vec::new();
        loop {
            let layer: Layer = Decode::decode(&mut r)?;
            if layer == Layer::Invalid {
                break;
            }
            let item_id: u32 = Decode::decode(&mut r)?;
            items.push(CorpseClothingEntry { layer, item_id });
        }

        Ok(Self { corpse_id, items })
    }
}

impl Encode<BE> for CorpseClothing {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.corpse_id);

        for entry in &self.items {
            w.put_u8(entry.layer.to_wire());
            w.put_u32(entry.item_id);
        }

        w.put_u8(0x00); // terminator
    }
}

// ── 0x74 OpenBuyWindow (dynamic, S→C) ─────────────────────────────────────

/// A single item entry in an [`OpenBuyWindow`] packet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BuyItem {
    /// Price in gold pieces.
    pub price: u32,
    /// Human-readable item description (e.g. "a longsword").
    pub description: String,
}

/// Packet 0x74 — Open Buy Window (variable, S→C)
///
/// Sent by the server to populate a vendor buy window with item prices
/// and descriptions. Each entry corresponds to an item previously sent
/// via [`ContainerContent`] (0x3C).
///
/// # Protocol
///
/// To open a buy window the server must:
///
/// 1. Wear the "for-sale" container on the vendor (layer 0x1A)
/// 2. Wear the "bought" container on the vendor (layer 0x1B)
/// 3. For each container:
///    a. Send [`ContainerContent`] (0x3C) with items
///    b. Send this packet (0x74) with prices/descriptions
/// 4. Send [`DrawContainer`] (0x24) with model `0x0030`
///
/// Items in this packet must be presented in the **reversed** order of
/// the 0x3C packet for most container objtypes. For objtype `0x2AF8`,
/// items must be sorted by increasing X coordinate instead.
///
/// # Wire format
///
/// ```text
/// BYTE    cmd             (0x74)
/// BYTE[2] blockSize       (total packet length)
/// BYTE[4] containerSerial (vendor serial, often vendorID | 0x40000000)
/// BYTE    itemCount       (number of items)
///   repeating itemCount times:
///     BYTE[4] price
///     BYTE    descLen
///     BYTE[descLen] description
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OpenBuyWindow {
    /// Serial of the buy container (often `vendorID | 0x40000000`).
    pub container_serial: u32,
    /// Items for sale with prices and descriptions.
    pub items: Vec<BuyItem>,
}

impl ManualPacket for OpenBuyWindow {
    const ID: u8 = 0x74;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 8, true)?;

        let container_serial: u32 = Decode::decode(&mut r)?;
        let item_count: u8 = Decode::decode(&mut r)?;

        let mut items = Vec::with_capacity(item_count as usize);
        for _ in 0..item_count {
            let price: u32 = Decode::decode(&mut r)?;
            let desc_len: u8 = Decode::decode(&mut r)?;
            let mut buf = vec![0u8; desc_len as usize];
            r.read_bytes(&mut buf)?;
            // Strip trailing null terminator if present.
            if buf.last() == Some(&0) {
                buf.pop();
            }
            let description = String::from_utf8_lossy(&buf).into_owned();
            items.push(BuyItem { price, description });
        }

        Ok(Self { container_serial, items })
    }
}

impl Encode<BE> for OpenBuyWindow {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.container_serial);
        w.put_u8(self.items.len() as u8);

        for item in &self.items {
            w.put_u32(item.price);
            let desc_bytes = item.description.as_bytes();
            // Length includes the null terminator.
            w.put_u8((desc_bytes.len() + 1) as u8);
            w.put_slice(desc_bytes);
            w.put_u8(0x00);
        }
    }
}

// ── 0x9E SellList (dynamic, S→C) ──────────────────────────────────────────

/// A single item entry in a [`SellList`] packet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SellItem {
    /// Serial of the item.
    pub item_id: u32,
    /// Graphic / art model of the item.
    pub item_model: u16,
    /// Hue / color of the item.
    pub hue: u16,
    /// Stack amount.
    pub amount: u16,
    /// Price the vendor will pay.
    pub value: u16,
    /// Human-readable item name.
    pub name: String,
}

/// Packet 0x9E — Sell List (variable, S→C)
///
/// Sent by the server to open a sell window on the client, listing items
/// the vendor is willing to buy from the player along with their prices.
///
/// # Wire format
///
/// ```text
/// BYTE    cmd             (0x9E)
/// BYTE[2] blockSize       (total packet length)
/// BYTE[4] shopkeeperID    (serial of the vendor)
/// BYTE[2] numItems
///   repeating numItems times:
///     BYTE[4] itemID
///     BYTE[2] itemModel
///     BYTE[2] itemHue
///     BYTE[2] itemAmount
///     BYTE[2] value
///     BYTE[2] nameLength
///     BYTE[nameLength] name
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SellList {
    /// Serial of the shopkeeper NPC.
    pub shopkeeper_id: u32,
    /// Items the vendor is willing to buy.
    pub items: Vec<SellItem>,
}

impl ManualPacket for SellList {
    const ID: u8 = 0x9E;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 9, true)?;

        let shopkeeper_id: u32 = Decode::decode(&mut r)?;
        let num_items: u16 = Decode::decode(&mut r)?;

        let mut items = Vec::with_capacity(num_items as usize);
        for _ in 0..num_items {
            let item_id: u32 = Decode::decode(&mut r)?;
            let item_model: u16 = Decode::decode(&mut r)?;
            let hue: u16 = Decode::decode(&mut r)?;
            let amount: u16 = Decode::decode(&mut r)?;
            let value: u16 = Decode::decode(&mut r)?;
            let name_len: u16 = Decode::decode(&mut r)?;
            let mut buf = vec![0u8; name_len as usize];
            r.read_bytes(&mut buf)?;
            let name = String::from_utf8_lossy(&buf).into_owned();
            items.push(SellItem { item_id, item_model, hue, amount, value, name });
        }

        Ok(Self { shopkeeper_id, items })
    }
}

impl Encode<BE> for SellList {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.shopkeeper_id);
        w.put_u16(self.items.len() as u16);

        for item in &self.items {
            w.put_u32(item.item_id);
            w.put_u16(item.item_model);
            w.put_u16(item.hue);
            w.put_u16(item.amount);
            w.put_u16(item.value);
            let name_bytes = item.name.as_bytes();
            w.put_u16(name_bytes.len() as u16);
            w.put_slice(name_bytes);
        }
    }
}

// ── 0x05 RequestAttack (5 bytes, fixed, C→S) ─────────────────────────────

/// Packet 0x05 — Request Attack (5 bytes, fixed, C→S)
///
/// Sent by the client when the player initiates an attack on a target.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x05
/// BYTE[4]  target_id — serial of the mobile to attack
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x05, size = fixed(5), endian = "be")]
pub struct RequestAttack {
    pub id: u8,
    /// Serial of the mobile the player is attacking.
    pub target_id: u32,
}

impl RequestAttack {
    pub fn new(target_id: u32) -> Self {
        Self { id: Self::ID, target_id }
    }
}

// ── 0x95 DyeWindow (9 bytes, fixed, both) ────────────────────────────────

/// Packet 0x95 — Dye Window (9 bytes, fixed, both directions)
///
/// Sent by the server to open the dye tub UI for an item, and returned
/// by the client with the player's chosen colour.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x95
/// BYTE[4]  item_id    — serial of the item being dyed
/// BYTE[2]  model      — S→C: ignored (send 0); C→S: gump/model ID
/// BYTE[2]  color      — S→C: default highlight colour (0x0FAB); C→S: chosen colour
/// ```
///
/// ## Direction semantics
///
/// | Field   | Server → Client              | Client → Server          |
/// |---------|------------------------------|--------------------------|
/// | `model` | ignored (always 0 on send)   | gump model returned      |
/// | `color` | default colour (`0x0FAB`)    | player's chosen colour   |
///
/// Use [`DyeWindow::open`] to construct the S→C packet and
/// [`DyeWindow::response`] to construct the C→S reply.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x95, size = fixed(9), endian = "be")]
pub struct DyeWindow {
    pub id: u8,
    /// Serial of the item to be dyed.
    pub item_id: u32,
    /// S→C: ignored (0). C→S: gump/model ID echoed back by the client.
    pub model: u16,
    /// S→C: default colour shown in the picker (typically `0x0FAB`).
    /// C→S: colour chosen by the player.
    pub color: u16,
}

impl DyeWindow {
    /// Default colour sent by the server when opening the dye window.
    pub const DEFAULT_COLOR: u16 = 0x0FAB;

    /// Construct the S→C packet that opens the dye window for `item_id`.
    ///
    /// `color` is the colour pre-selected in the picker;
    /// pass `None` to use the UO default (`0x0FAB`).
    pub fn open(item_id: u32, color: Option<u16>) -> Self {
        Self {
            id: Self::ID,
            item_id,
            model: 0,
            color: color.unwrap_or(Self::DEFAULT_COLOR),
        }
    }

    /// Construct the C→S packet sent when the player confirms a colour choice.
    pub fn response(item_id: u32, model: u16, color: u16) -> Self {
        Self { id: Self::ID, item_id, model, color }
    }
}

// ── 0x9A ConsoleEntryPrompt (variable, both) ──────────────────────────────

/// Whether this is a server request or a client reply.
///
/// | Wire value | Meaning                                      |
/// |------------|----------------------------------------------|
/// | 0          | S→C request (client sends 0 + no text on ESC)|
/// | 1          | C→S reply with text                          |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u32)]
pub enum PromptType {
    /// Server sent — opens a text-entry prompt on the client.
    #[wire_enum(0, "request")]
    Request,
    /// Client sent — player's reply (or ESC with no text).
    #[wire_enum(1, "reply")]
    Reply,
    /// Unknown type value.
    #[wire_enum(unknown)]
    Unknown(u32),
}

/// Packet 0x9A — Console Entry Prompt (variable, both directions)
///
/// The server sends this to display a text-entry prompt to the player
/// (type = 0). The client replies with the same packet (type = 1) containing
/// the entered text, or with type = 0 and no text when ESC is pressed.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]   0x9A
/// BYTE[2]   length     — total packet length
/// BYTE[4]   serial     — serial of the entity issuing the prompt
/// BYTE[4]   prompt_id  — identifies this prompt session
/// BYTE[4]   kind       — 0 = request, 1 = reply
/// BYTE[?]   text       — optional null-terminated ASCII string
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ConsoleEntryPrompt {
    /// Serial of the entity that issued the prompt.
    pub serial: u32,
    /// Prompt session identifier.
    pub prompt_id: u32,
    /// Request from server or reply from client.
    pub kind: PromptType,
    /// Optional text.
    ///
    /// - S→C: absent (no text in the request).
    /// - C→S: the player's input, or `None` / empty when ESC was pressed.
    pub text: Option<String>,
}

impl ConsoleEntryPrompt {
    /// Minimum wire size: cmd(1) + len(2) + serial(4) + prompt_id(4) + kind(4) = 15
    const MIN_SIZE: usize = 15;

    /// Construct the S→C packet that opens a prompt on the client.
    pub fn request(serial: u32, prompt_id: u32) -> Self {
        Self { serial, prompt_id, kind: PromptType::Request, text: None }
    }

    /// Construct the C→S reply carrying the player's input.
    ///
    /// Pass an empty string or `""` to represent an ESC / cancelled prompt.
    pub fn reply(serial: u32, prompt_id: u32, text: impl Into<String>) -> Self {
        let s = text.into();
        Self {
            serial,
            prompt_id,
            kind: PromptType::Reply,
            text: if s.is_empty() { None } else { Some(s) },
        }
    }
}

impl ManualPacket for ConsoleEntryPrompt {
    const ID: u8 = 0x9A;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, Self::MIN_SIZE, true)?;

        let serial:    u32 = Decode::decode(&mut r)?;
        let prompt_id: u32 = Decode::decode(&mut r)?;
        let kind_raw:  u32 = Decode::decode(&mut r)?;
        let kind = PromptType::from_wire(kind_raw);

        // Text is present only when remaining bytes exist.
        let text = if r.remaining_len() > 0 {
            let ns: NullString = Decode::decode(&mut r)?;
            if ns.0.is_empty() { None } else { Some(ns.0) }
        } else {
            None
        };

        Ok(Self { serial, prompt_id, kind, text })
    }
}

impl Encode<BE> for ConsoleEntryPrompt {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        w.put_u32(self.serial);
        w.put_u32(self.prompt_id);
        w.put_u32(self.kind.to_wire());

        if let Some(text) = &self.text {
            w.put_slice(text.as_bytes());
        }
        w.put_u8(0x00); // null terminator always present
    }
}

// ── 0x3B BuyItems (dynamic, C→S) ──────────────────────────────────────────

/// A single item the client wishes to purchase in a [`BuyItems`] packet.
///
/// On the wire each entry is prefixed by a constant `0x1A` byte (the layer
/// marker for the vendor buy container).
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct BuyItemEntry {
    /// Constant `0x1A` — layer marker for the vendor buy container.
    #[binary(const_value = 0x1Au8)]
    pub _layer: u8,
    /// Serial of the item (from the 0x3C ContainerContent packet).
    pub item_id: u32,
    /// Number of units to purchase.
    pub quantity: u16,
}

impl BuyItemEntry {
    pub fn new(item_id: u32, quantity: u16) -> Self {
        Self { _layer: 0x1A, item_id, quantity }
    }
}

/// Packet 0x3B — Buy Item(s) (variable, C→S)
///
/// Sent by the client to purchase one or more items from a vendor.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x3B
/// BYTE[2]  blockSize
/// BYTE[4]  vendorID
/// BYTE[1]  flag          — 0x00 = no items (cancel), 0x02 = items follow
/// For each item (only present when flag == 0x02):
///   BYTE[1]  0x1A        (constant layer marker — part of BuyItemEntry)
///   BYTE[4]  itemID
///   BYTE[2]  quantity
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BuyItems {
    /// Serial of the vendor NPC.
    pub vendor_id: u32,
    /// Items to purchase. Empty means the client cancelled (flag = 0x00).
    pub items: Vec<BuyItemEntry>,
}

impl BuyItems {
    pub fn new(vendor_id: u32, items: Vec<BuyItemEntry>) -> Self {
        Self { vendor_id, items }
    }
}

impl ManualPacket for BuyItems {
    const ID: u8 = 0x3B;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + vendor_id(4) + flag(1) = 8 bytes
        let mut r = packet_reader(data, Self::ID, 8, true)?;

        let vendor_id: u32 = Decode::decode(&mut r)?;
        let flag: u8 = Decode::decode(&mut r)?;

        let mut items = Vec::new();
        if flag == 0x02 {
            while r.remaining_len() >= 7 {
                items.push(BuyItemEntry::decode(&mut r)?);
            }
        }

        Ok(Self { vendor_id, items })
    }
}

impl Encode<BE> for BuyItems {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.vendor_id);
        if self.items.is_empty() {
            w.put_u8(0x00); // flag: cancel
        } else {
            w.put_u8(0x02); // flag: items follow
            for item in &self.items {
                item.encode(w);
            }
        }
    }
}

// ── 0x9F SellListReply (dynamic, C→S) ─────────────────────────────────────

/// A single item the client wishes to sell in a [`SellListReply`] packet.
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct SellItemEntry {
    /// Serial of the item being sold.
    pub item_id: u32,
    /// Number of units to sell.
    pub quantity: u16,
}

impl SellItemEntry {
    pub fn new(item_id: u32, quantity: u16) -> Self {
        Self { item_id, quantity }
    }
}

/// Packet 0x9F — Sell List Reply (variable, C→S)
///
/// Sent by the client to sell one or more items to a vendor.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x9F
/// BYTE[2]  blockSize
/// BYTE[4]  shopkeeperID
/// BYTE[2]  itemCount
/// For each item:
///   BYTE[4]  itemID
///   BYTE[2]  quantity
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x9F, size = dynamic, endian = "be")]
pub struct SellListReply {
    pub id: u8,
    pub len: u16,
    /// Serial of the shopkeeper NPC.
    pub shopkeeper_id: u32,
    /// Items the client wishes to sell.
    pub items: ListU16<SellItemEntry>,
}

impl SellListReply {
    pub fn new(shopkeeper_id: u32, items: Vec<SellItemEntry>) -> Self {
        Self {
            id: 0x9F,
            len: 0,
            shopkeeper_id,
            items: ListU16::new(items),
        }
    }
}

// ── 0x23 DraggingOfItem (26 bytes, fixed, S→C) ────────────────────────────

/// Packet 0x23 — Dragging Of Item (26 bytes, fixed, S→C)
///
/// Sent by the server to animate an item being dragged from one location
/// (or entity) to another. Used for visual feedback when an item is moved
/// in the ecumene.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x23
/// BYTE[2]  model         — graphic / art model of the item
/// BYTE[3]  unknown1      — reserved / unknown
/// BYTE[2]  stack_count   — number of items in the stack
/// BYTE[4]  source_id     — serial of the source container or entity (0 = ecumene)
/// BYTE[2]  source_x      — X coordinate of the source
/// BYTE[2]  source_y      — Y coordinate of the source
/// BYTE[1]  source_z      — Z coordinate of the source
/// BYTE[4]  target_id     — serial of the target container or entity (0 = ecumene)
/// BYTE[2]  target_x      — X coordinate of the target
/// BYTE[2]  target_y      — Y coordinate of the target
/// BYTE[1]  target_z      — Z coordinate of the target
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x23, size = fixed(26), endian = "be")]
pub struct DraggingOfItem {
    pub id: u8,
    /// Graphic / art model of the dragged item.
    pub model: u16,
    /// Reserved bytes (unknown purpose).
    pub unknown1: RawBytes<3>,
    /// Number of items in the stack being dragged.
    pub stack_count: u16,
    /// Serial of the source container or entity (`0` = ground / world).
    pub source_id: u32,
    /// X coordinate of the source location.
    pub source_x: u16,
    /// Y coordinate of the source location.
    pub source_y: u16,
    /// Z coordinate of the source location.
    pub source_z: u8,
    /// Serial of the target container or entity (`0` = ground / world).
    pub target_id: u32,
    /// X coordinate of the target location.
    pub target_x: u16,
    /// Y coordinate of the target location.
    pub target_y: u16,
    /// Z coordinate of the target location.
    pub target_z: u8,
}

impl DraggingOfItem {
    /// Construct a drag animation from one world location to another.
    pub fn world_to_world(
        model: u16,
        stack_count: u16,
        source_x: u16, source_y: u16, source_z: u8,
        target_x: u16, target_y: u16, target_z: u8,
    ) -> Self {
        Self {
            id: Self::ID,
            model,
            unknown1: RawBytes([0u8; 3]),
            stack_count,
            source_id: 0,
            source_x,
            source_y,
            source_z,
            target_id: 0,
            target_x,
            target_y,
            target_z,
        }
    }

    /// Construct a drag animation between two serials (e.g. container to container).
    pub fn serial_to_serial(
        model: u16,
        stack_count: u16,
        source_id: u32, source_x: u16, source_y: u16, source_z: u8,
        target_id: u32, target_x: u16, target_y: u16, target_z: u8,
    ) -> Self {
        Self {
            id: Self::ID,
            model,
            unknown1: RawBytes([0u8; 3]),
            stack_count,
            source_id,
            source_x,
            source_y,
            source_z,
            target_id,
            target_x,
            target_y,
            target_z,
        }
    }
}
