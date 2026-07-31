//! World object and visual effect packets.
//!
//! Packets for drawing objects (items and mobiles) in the game world,
//! and for visual effects (projectiles, explosions, etc.).

use u_io::{BE, BinaryWriter, Decode, Encode, packet_reader, encode_le_utf16_str, decode_le_utf16_str};
use u_io::BasicPacket;
use macros::{Packet, WireEnum};

use crate::layer::Layer;
use crate::mobile_flags::MobileFlags;
use crate::movement::Notoriety;
use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── Object info flags ─────────────────────────────────────────────────────

/// Bitwise status flags for an object in [`ObjectInfo`].
///
/// - 0x00: None
/// - 0x02: Female
/// - 0x04: Poisoned
/// - 0x08: Yellow health bar
/// - 0x10: Faction ship
/// - 0x20: Movable (if normally not)
/// - 0x40: War mode
/// - 0x80: Hidden
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ObjectInfoFlags(pub u8);

impl ObjectInfoFlags {
    pub fn none(self) -> bool { self.0 == 0x00 }
    pub fn female(self) -> bool { self.0 & 0x02 != 0 }
    pub fn poisoned(self) -> bool { self.0 & 0x04 != 0 }
    pub fn yellow_hits(self) -> bool { self.0 & 0x08 != 0 }
    pub fn faction_ship(self) -> bool { self.0 & 0x10 != 0 }
    pub fn movable(self) -> bool { self.0 & 0x20 != 0 }
    pub fn war_mode(self) -> bool { self.0 & 0x40 != 0 }
    pub fn hidden(self) -> bool { self.0 & 0x80 != 0 }
}

// ── 0x1A ObjectInfo (dynamic, S→C) ────────────────────────────────────────

/// Packet 0x1A — Object Info (variable, S→C)
///
/// Draws a world item (not a mobile). The wire format uses bit flags in
/// the serial and coordinate fields to indicate the presence of optional
/// fields:
///
/// - `object_id & 0x80000000` → `amount` field present
/// - `graphic & 0x8000` → `graphic_increment` field present
/// - `x & 0x8000` → `facing` field present
/// - `y & 0x8000` → `dye` (color) field present
/// - `y & 0x4000` → `flags` field present
///
/// The stored fields always have the flag bits cleared.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObjectInfo {
    pub object_id: u32,
    pub graphic: u16,
    /// Item count, or graphic for corpses. Only present when `object_id & 0x80000000`.
    pub amount: Option<u16>,
    /// Increment graphic by this value. Only present when `graphic & 0x8000`.
    pub graphic_increment: Option<u8>,
    pub x: u16,
    pub y: u16,
    /// Facing direction. Only present when `x & 0x8000`.
    pub facing: Option<u8>,
    pub z: i8,
    /// Dye / color. Only present when `y & 0x8000`.
    pub dye: Option<u16>,
    /// Status flags. Only present when `y & 0x4000`.
    pub flags: Option<ObjectInfoFlags>,
}

impl ManualPacket for ObjectInfo {
    const ID: u8 = 0x1A;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 11, true)?;

        // Object ID — bit 31 indicates amount field present
        let raw_object_id: u32 = Decode::decode(&mut r)?;
        let has_amount = raw_object_id & 0x8000_0000 != 0;
        let object_id = raw_object_id & 0x7FFF_FFFF;

        // Graphic — bit 15 indicates graphic increment present
        let raw_graphic: u16 = Decode::decode(&mut r)?;
        let has_increment = raw_graphic & 0x8000 != 0;
        let graphic = raw_graphic & 0x7FFF;

        // Amount (conditional)
        let amount = if has_amount {
            Some(<u16 as Decode<BE>>::decode(&mut r)?)
        } else {
            None
        };

        // Graphic increment (conditional)
        let graphic_increment = if has_increment {
            Some(<u8 as Decode<BE>>::decode(&mut r)?)
        } else {
            None
        };

        // X — bit 15 indicates facing present
        let raw_x: u16 = Decode::decode(&mut r)?;
        let has_facing = raw_x & 0x8000 != 0;
        let x = raw_x & 0x7FFF;

        // Y — bit 15 indicates dye present, bit 14 indicates flags present
        let raw_y: u16 = Decode::decode(&mut r)?;
        let has_dye = raw_y & 0x8000 != 0;
        let has_flags = raw_y & 0x4000 != 0;
        let y = raw_y & 0x3FFF;

        // Facing (conditional)
        let facing = if has_facing {
            Some(<u8 as Decode<BE>>::decode(&mut r)?)
        } else {
            None
        };

        // Z (always present, signed)
        let z: i8 = Decode::decode(&mut r)?;

        // Dye (conditional)
        let dye = if has_dye {
            Some(<u16 as Decode<BE>>::decode(&mut r)?)
        } else {
            None
        };

        // Flags (conditional)
        let flags = if has_flags {
            Some(ObjectInfoFlags(<u8 as Decode<BE>>::decode(&mut r)?))
        } else {
            None
        };

        Ok(Self {
            object_id,
            graphic,
            amount,
            graphic_increment,
            x,
            y,
            facing,
            z,
            dye,
            flags,
        })
    }
}

/// Multi-object detection threshold for classic (pre-SA) protocol.
///
/// In packet `0x1A`, a `graphic >= 0x4000` indicates a multi-object (house,
/// boat, etc.).  The actual multi ID (index into `multi.idx`) is obtained by
/// subtracting this constant.
const MULTI_GRAPHIC_OFFSET: u16 = 0x4000;

impl ObjectInfo {
    /// Returns `true` if this item is a multi-object (house, boat, etc.).
    ///
    /// In the classic UO protocol, multi-objects are distinguished from
    /// regular items by having `graphic >= 0x4000`.
    #[inline]
    pub fn is_multi(&self) -> bool {
        self.graphic >= MULTI_GRAPHIC_OFFSET
    }

    /// If this item is a multi-object, returns its multi ID (index into
    /// `multi.idx` / `multi.mul`).
    ///
    /// The multi ID is `graphic - 0x4000`.  Returns `None` for regular items.
    #[inline]
    pub fn multi_id(&self) -> Option<u16> {
        if self.is_multi() {
            Some(self.graphic - MULTI_GRAPHIC_OFFSET)
        } else {
            None
        }
    }
}

impl Encode<BE> for ObjectInfo {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(0x1A);
        w.put_u16(0); // length placeholder — back-patched by encode_to_wire

        // Object ID — set bit 31 if amount is present
        let raw_object_id = if self.amount.is_some() {
            self.object_id | 0x8000_0000
        } else {
            self.object_id
        };
        w.put_u32(raw_object_id);

        // Graphic — set bit 15 if graphic_increment is present
        let raw_graphic = if self.graphic_increment.is_some() {
            self.graphic | 0x8000
        } else {
            self.graphic
        };
        w.put_u16(raw_graphic);

        // Amount (conditional — present when object_id bit 31 was set)
        if let Some(amount) = self.amount {
            w.put_u16(amount);
        }

        // Graphic increment (conditional)
        if let Some(inc) = self.graphic_increment {
            w.put_u8(inc);
        }

        // X — set bit 15 if facing present
        let raw_x = if self.facing.is_some() {
            self.x | 0x8000
        } else {
            self.x
        };
        w.put_u16(raw_x);

        // Y — set bit 15 if dye present, bit 14 if flags present
        let mut raw_y = self.y & 0x3FFF;
        if self.dye.is_some() {
            raw_y |= 0x8000;
        }
        if self.flags.is_some() {
            raw_y |= 0x4000;
        }
        w.put_u16(raw_y);

        // Facing (conditional)
        if let Some(facing) = self.facing {
            w.put_u8(facing);
        }

        // Z (always present, signed)
        w.put_i8(self.z);

        // Dye (conditional)
        if let Some(dye) = self.dye {
            w.put_u16(dye);
        }

        // Flags (conditional)
        if let Some(flags) = self.flags {
            w.put_u8(flags.0);
        }
    }
}

// ── Equipment item ─────────────────────────────────────────────────────────

/// A single equipped item in a [`DrawMobile`] / [`DrawMobileExtended`] packet.
///
/// On the wire bit 0x8000 of `graphic` signals that a 2-byte color word
/// follows (legacy clients). Modern clients (>= 7.03.31) always send the
/// color word. The stored `graphic` always has bit 15 cleared.
///
/// `color` is `None` when absent on the wire, and `Some(0)` is treated
/// the same as `None` during serialisation (no color word is written and
/// bit 15 is not set).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EquippedItem {
    pub serial: u32,
    pub graphic: u16,
    pub layer: Layer,
    pub color: Option<u16>,
}

// ── Shared equipment-list parser ───────────────────────────────────────────

/// Detect whether the equipment list was encoded in "modern client" mode
/// (clients >= 7.03.31, where the hue word is always present after the layer
/// byte) vs "legacy" mode (hue only present when `graphic & 0x8000`).
///
/// `item_bytes` is the number of bytes remaining in the packet after the
/// fixed mobile header and before (or including) the 4-byte `0x00000000`
/// terminator.
///
/// Modern mode: every item record is exactly 9 bytes
/// (`serial`(4) + `graphic`(2) + `layer`(1) + `hue`(2)).
/// Legacy mode: every item record is 7 bytes without hue or 9 bytes with it.
///
/// If `item_bytes - 4` (excluding the terminator) is divisible by 9,
/// the packet **must** be modern, because a legacy packet with all-hue items
/// would look identical on this metric — but we accept that trade-off: in
/// legacy mode mixed packets (some with hue, some without) will never be
/// divisible by 9 cleanly unless they happen to be all-hue, which would also
/// parse correctly in modern mode.
///
/// Returns `true` when modern mode is detected.
fn detect_modern_client(item_bytes: usize) -> bool {
    // item_bytes includes the 4-byte terminator.
    if item_bytes < 4 {
        return false;
    }
    let payload = item_bytes - 4; // bytes for actual item records
    payload > 0 && payload % 9 == 0
}

/// Parse the equipment list shared by [`DrawMobile`] (0x78) and
/// [`DrawMobileExtended`] (0xD3).
///
/// The list is terminated by a `0x00000000` serial.  If the packet is
/// truncated and no terminator is present we stop reading silently.
///
/// `modern_client` — `true` for clients >= 7.03.31 (CV_70331): the hue
/// word is always present after the layer byte.  `false` (legacy): the
/// hue word is only present when `graphic & 0x8000` is set.
fn parse_equipment_list(
    reader: &mut u_io::BinaryReader<BE>,
    modern_client: bool,
) -> Result<Vec<EquippedItem>, PacketError> {
    let mut items = Vec::new();
    loop {
        // Guard against truncated packets that lack a terminator.
        if reader.remaining_len() < 4 {
            break;
        }
        let item_serial: u32 = Decode::decode(reader)?;
        if item_serial == 0x0000_0000 {
            break;
        }

        let raw_graphic: u16 = Decode::decode(reader)?;
        let layer: Layer = Decode::decode(reader)?;

        // Clients >= 7.03.31 always send a hue word after the layer byte.
        // Older clients only include it when graphic bit 15 is set.
        let item_color = if modern_client || (raw_graphic & 0x8000 != 0) {
            Some(<u16 as Decode<BE>>::decode(reader)?)
        } else {
            None
        };

        items.push(EquippedItem {
            serial: item_serial,
            graphic: raw_graphic & 0x7FFF,
            layer,
            color: item_color,
        });
    }
    Ok(items)
}

/// Encode the equipment list into `writer`.
///
/// `color = Some(0)` is treated identically to `None` — no color word is
/// written and bit 0x8000 is not set on the graphic (mirrors Delphi behaviour).
///
/// When `modern_client` is `true` (clients >= 7.0.33.1 / CV_70331) a fixed
/// 9-byte record is emitted for every item regardless of the color value,
/// matching the always-present hue word expected by modern clients.  In legacy
/// mode the hue word is conditional (7 or 9 bytes per item).
fn encode_equipment_list(writer: &mut BinaryWriter<BE>, items: &[EquippedItem], modern_client: bool) {
    for item in items {
        writer.put_u32(item.serial);

        if modern_client {
            // Modern format (>= 7.0.33.1): fixed 9 bytes per item.
            // Hue is always present; bit 0x8000 is NOT set (the decoder
            // reads it unconditionally and masks the graphic with 0x7FFF).
            writer.put_u16(item.graphic & 0x7FFF);
            writer.put_u8(item.layer.to_wire());
            writer.put_u16(item.color.unwrap_or(0));
        } else {
            // Legacy format: hue word only when color is Some and > 0.
            let has_color = item.color.is_some_and(|c| c > 0);
            let raw_graphic = if has_color { item.graphic | 0x8000 } else { item.graphic };
            writer.put_u16(raw_graphic);
            writer.put_u8(item.layer.to_wire());

            if has_color {
                writer.put_u16(item.color.unwrap());
            }
        }
    }
    // Terminator
    writer.put_u32(0x0000_0000);
}

// ── Shared mobile header ───────────────────────────────────────────────────

/// Common header fields shared by [`DrawMobile`] (0x78) and
/// [`DrawMobileExtended`] (0xD3).
struct MobileHeader {
    serial: u32,
    graphic: u16,
    x: u16,
    y: u16,
    z: i8,
    direction: u8,
    color: u16,
    status: MobileFlags,
    notoriety: Notoriety,
}

/// Parse the mobile header fields common to 0x78 and 0xD3.
fn decode_mobile_header(
    reader: &mut u_io::BinaryReader<BE>,
) -> Result<MobileHeader, PacketError> {
    let serial: u32 = Decode::decode(reader)?;
    let graphic: u16 = Decode::decode(reader)?;
    let x: u16 = Decode::decode(reader)?;
    let y: u16 = Decode::decode(reader)?;
    let z: i8 = Decode::decode(reader)?;
    let direction: u8 = Decode::decode(reader)?;
    let color: u16 = Decode::decode(reader)?;
    let status_byte: u8 = Decode::decode(reader)?;
    let notoriety: Notoriety = Decode::decode(reader)?;
    Ok(MobileHeader {
        serial, graphic, x, y, z, direction, color,
        status: MobileFlags(status_byte), notoriety,
    })
}

/// Encode the mobile header fields common to 0x78 and 0xD3.
fn encode_mobile_header(writer: &mut BinaryWriter<BE>, serial: u32, graphic: u16,
    x: u16, y: u16, z: i8, direction: u8, color: u16, status: &MobileFlags,
    notoriety: &Notoriety,
) {
    writer.put_u32(serial);
    writer.put_u16(graphic);
    writer.put_u16(x);
    writer.put_u16(y);
    writer.put_i8(z);
    writer.put_u8(direction);
    writer.put_u16(color);
    writer.put_u8(status.0);
    notoriety.encode(writer);
}

// ── 0x78 DrawMobile (dynamic, S→C) ────────────────────────────────────────

/// Packet 0x78 — Draw Mobile (dynamic, S→C)
///
/// Draws a mobile with its equipped items. The item list is terminated
/// by a `0x00000000` serial. Each item's `color` field is only present
/// when `graphic & 0x8000` is set on the wire (legacy) or always for
/// clients >= 7.03.31. The stored `graphic` always has bit 15 cleared.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DrawMobile {
    pub serial: u32,
    pub graphic: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub direction: u8,
    pub color: u16,
    pub status: MobileFlags,
    pub notoriety: Notoriety,
    pub items: Vec<EquippedItem>,
}

impl DrawMobile {
    /// Parse a `DrawMobile` packet with explicit client-version context.
    ///
    /// # Parameters
    ///
    /// - `data` — raw packet bytes (including the `0x78` id and length header).
    /// - `modern_client` — set to `true` for clients >= 7.03.31 (CV_70331).
    ///   When `true`, each equipped item always carries a 2-byte hue field on
    ///   the wire regardless of the `graphic & 0x8000` bit.  When `false`
    ///   (legacy behaviour), the hue field is only present when that bit is set.
    pub fn parse(data: &[u8], modern_client: bool) -> Result<Self, PacketError> {
        // Minimum: id(1)+len(2)+serial(4)+graphic(2)+x(2)+y(2)+z(1)+dir(1)
        //          +color(2)+status(1)+notoriety(1)+terminator(4) = 23 bytes
        let mut reader = packet_reader(data, Self::ID, 23, true)?;

        let hdr = decode_mobile_header(&mut reader)?;
        let items = parse_equipment_list(&mut reader, modern_client)?;

        Ok(Self {
            serial: hdr.serial, graphic: hdr.graphic, x: hdr.x, y: hdr.y,
            z: hdr.z, direction: hdr.direction, color: hdr.color,
            status: hdr.status, notoriety: hdr.notoriety, items,
        })
    }
}

impl ManualPacket for DrawMobile {
    const ID: u8 = 0x78;
    const SIZE: PacketSize = PacketSize::Dynamic;

    /// Parse assuming a legacy client (< 7.03.31).
    ///
    /// For modern clients use [`DrawMobile::parse`] with
    /// `modern_client = true`.
    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Auto-detect modern vs legacy by checking whether the item bytes
        // (total length minus the 19-byte fixed header) are divisible by 9.
        // Modern items are always 9 bytes each; legacy items vary (7 or 9).
        let item_bytes = data.len().saturating_sub(19);
        let modern = detect_modern_client(item_bytes);
        Self::parse(data, modern)
    }
}

impl Encode<BE> for DrawMobile {
    fn encode(&self, writer: &mut BinaryWriter<BE>) {
        writer.put_u8(Self::ID);
        writer.put_u16(0); // length placeholder

        encode_mobile_header(writer, self.serial, self.graphic,
            self.x, self.y, self.z, self.direction, self.color,
            &self.status, &self.notoriety);

        encode_equipment_list(writer, &self.items, false);
    }
}

impl DrawMobile {
    /// Encode the packet choosing the equipment-list format based on
    /// `version`.  Clients >= 7.0.33.1 (`CV_70331`) receive the modern
    /// fixed 9-byte-per-item format; older clients receive the legacy
    /// variable-stride format.
    pub fn to_bytes_versioned(&self, version: u_core::ProtocolVersion) -> bytes::Bytes {
        let modern = version >= u_core::ProtocolVersion::CV_70331;
        let mut writer = BinaryWriter::<BE>::new();
        writer.put_u8(Self::ID);
        writer.put_u16(0); // length placeholder
        encode_mobile_header(&mut writer, self.serial, self.graphic,
            self.x, self.y, self.z, self.direction, self.color,
            &self.status, &self.notoriety);
        encode_equipment_list(&mut writer, &self.items, modern);
        writer.set_u16_at(1, writer.len() as u16);
        writer.finish()
    }
}

// ── 0xD3 DrawMobileExtended (dynamic, S→C) ────────────────────────────────

/// Packet 0xD3 — Draw Mobile Extended (dynamic, S→C)
///
/// Introduced for the now-defunct UO 3D client.  The wire format is
/// identical to [`DrawMobile`] (0x78) except that three additional `u16`
/// fields (purpose unknown, always `0` in practice) are inserted between
/// `notoriety` and the equipment list.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]   0xD3
/// BYTE[2]   total packet length
/// BYTE[4]   serial
/// BYTE[2]   graphic
/// BYTE[2]   x
/// BYTE[2]   y
/// BYTE[1]   z (signed)
/// BYTE[1]   direction
/// BYTE[2]   color / hue
/// BYTE[1]   status flags
/// BYTE[1]   notoriety
/// BYTE[2]   unknown_1   — always 0
/// BYTE[2]   unknown_2   — always 0
/// BYTE[2]   unknown_3   — always 0
/// Loop (equipment list, same as 0x78):
///   BYTE[4]   item serial  (0x00000000 = end)
///   BYTE[2]   graphic      (bit 15 set → color follows)
///   BYTE[1]   layer
///  [BYTE[2]]  color        (legacy: only when bit 15 set; modern: always)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DrawMobileExtended {
    pub serial: u32,
    pub graphic: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub direction: u8,
    pub color: u16,
    pub status: MobileFlags,
    pub notoriety: Notoriety,
    /// Three unknown `u16` values specific to the 3D client.
    /// Always `[0, 0, 0]` in practice.
    pub unknown: [u16; 3],
    pub items: Vec<EquippedItem>,
}

impl DrawMobileExtended {
    /// Parse a `DrawMobileExtended` packet with explicit client-version context.
    ///
    /// See [`DrawMobile::parse`] for the `modern_client` semantics.
    pub fn parse(data: &[u8], modern_client: bool) -> Result<Self, PacketError> {
        // Minimum: id(1)+len(2)+serial(4)+graphic(2)+x(2)+y(2)+z(1)+dir(1)
        //          +color(2)+status(1)+notoriety(1)+unknown(6)+terminator(4) = 29 bytes
        let mut reader = packet_reader(data, Self::ID, 29, true)?;

        let hdr = decode_mobile_header(&mut reader)?;

        // Three unknown u16 fields (3D-client specific, always 0 in practice).
        let u0: u16 = Decode::decode(&mut reader)?;
        let u1: u16 = Decode::decode(&mut reader)?;
        let u2: u16 = Decode::decode(&mut reader)?;

        let items = parse_equipment_list(&mut reader, modern_client)?;

        Ok(Self {
            serial: hdr.serial, graphic: hdr.graphic, x: hdr.x, y: hdr.y,
            z: hdr.z, direction: hdr.direction, color: hdr.color,
            status: hdr.status, notoriety: hdr.notoriety,
            unknown: [u0, u1, u2], items,
        })
    }
}

impl ManualPacket for DrawMobileExtended {
    const ID: u8 = 0xD3;
    const SIZE: PacketSize = PacketSize::Dynamic;

    /// Parse assuming a legacy client (< 7.03.31).
    ///
    /// For modern clients use [`DrawMobileExtended::parse`] with
    /// `modern_client = true`.
    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Auto-detect modern vs legacy (same logic as DrawMobile, but the
        // fixed header is 6 bytes larger due to the three unknown u16 fields).
        let item_bytes = data.len().saturating_sub(25);
        let modern = detect_modern_client(item_bytes);
        Self::parse(data, modern)
    }
}

impl Encode<BE> for DrawMobileExtended {
    fn encode(&self, writer: &mut BinaryWriter<BE>) {
        writer.put_u8(Self::ID);
        writer.put_u16(0); // length placeholder

        encode_mobile_header(writer, self.serial, self.graphic,
            self.x, self.y, self.z, self.direction, self.color,
            &self.status, &self.notoriety);

        writer.put_u16(self.unknown[0]);
        writer.put_u16(self.unknown[1]);
        writer.put_u16(self.unknown[2]);

        encode_equipment_list(writer, &self.items, false);
    }
}

impl DrawMobileExtended {
    /// Encode the packet choosing the equipment-list format based on
    /// `version`.  See [`DrawMobile::to_bytes_versioned`] for details.
    pub fn to_bytes_versioned(&self, version: u_core::ProtocolVersion) -> bytes::Bytes {
        let modern = version >= u_core::ProtocolVersion::CV_70331;
        let mut writer = BinaryWriter::<BE>::new();
        writer.put_u8(Self::ID);
        writer.put_u16(0); // length placeholder
        encode_mobile_header(&mut writer, self.serial, self.graphic,
            self.x, self.y, self.z, self.direction, self.color,
            &self.status, &self.notoriety);
        writer.put_u16(self.unknown[0]);
        writer.put_u16(self.unknown[1]);
        writer.put_u16(self.unknown[2]);
        encode_equipment_list(&mut writer, &self.items, modern);
        writer.set_u16_at(1, writer.len() as u16);
        writer.finish()
    }
}

// ── 0x70 GraphicalEffect (28 bytes, fixed, S→C) ──────────────────────────

/// Packet 0x70 — Graphical Effect (28 bytes, fixed, S→C)
///
/// Displays a visual effect (projectile, lightning, area effect, etc.)
/// in the game world.
///
/// # Direction type
///
/// | Value | Meaning                                     |
/// |-------|---------------------------------------------|
/// | 0x00  | Projectile — go from source to destination  |
/// | 0x01  | Lightning strike at source                  |
/// | 0x02  | Stay at current x, y, z                     |
/// | 0x03  | Stay with source character                  |
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = 0x70, size = fixed(28), endian = "be")]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GraphicalEffect {
    pub id: u8,
    /// Effect direction type (0x00–0x03).
    pub direction_type: u8,
    /// Serial of the source character/object.
    pub source_serial: u32,
    /// Serial of the target character/object.
    pub target_serial: u32,
    /// Graphic model of the first frame of the effect.
    pub model: u16,
    /// Source X coordinate.
    pub x: u16,
    /// Source Y coordinate.
    pub y: u16,
    /// Source Z coordinate (signed).
    pub z: i8,
    /// Target X coordinate.
    pub target_x: u16,
    /// Target Y coordinate.
    pub target_y: u16,
    /// Target Z coordinate (signed).
    pub target_z: i8,
    /// Speed of the animation.
    pub speed: u8,
    /// Duration: `0` = very long, `1` = shortest.
    pub duration: u8,
    #[binary(pad = 2)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    /// Adjust direction during animation: `0` = yes, `1` = no.
    pub fixed_direction: u8,
    /// Explode on impact: `0` = no, `1` = yes.
    pub explode: u8,
}

impl GraphicalEffect {
    /// Create a projectile effect from source to target.
    pub fn projectile(
        source_serial: u32,
        target_serial: u32,
        model: u16,
        x: u16,
        y: u16,
        z: i8,
        target_x: u16,
        target_y: u16,
        target_z: i8,
        speed: u8,
        duration: u8,
    ) -> Self {
        Self {
            id: Self::ID,
            direction_type: 0x00,
            source_serial,
            target_serial,
            model,
            x,
            y,
            z,
            target_x,
            target_y,
            target_z,
            speed,
            duration,
            _pad0: (),
            fixed_direction: 0,
            explode: 0,
        }
    }

    /// Create a lightning strike effect at the source location.
    pub fn lightning(source_serial: u32, x: u16, y: u16, z: i8) -> Self {
        Self {
            id: Self::ID,
            direction_type: 0x01,
            source_serial,
            target_serial: 0,
            model: 0,
            x,
            y,
            z,
            target_x: 0,
            target_y: 0,
            target_z: 0,
            speed: 0,
            duration: 1,
            _pad0: (),
            fixed_direction: 0,
            explode: 0,
        }
    }

    /// Create a stationary effect at the given coordinates.
    pub fn stationary(
        model: u16,
        x: u16,
        y: u16,
        z: i8,
        speed: u8,
        duration: u8,
    ) -> Self {
        Self {
            id: Self::ID,
            direction_type: 0x02,
            source_serial: 0,
            target_serial: 0,
            model,
            x,
            y,
            z,
            target_x: x,
            target_y: y,
            target_z: z,
            speed,
            duration,
            _pad0: (),
            fixed_direction: 0,
            explode: 0,
        }
    }
}

// ── 0xC7 Particle3DEffect (49 bytes, fixed, S→C) ──────────────────────────

/// Packet 0xC7 — 3D Particle Effect (49 bytes, fixed, S→C)
///
/// Displays a 3D particle system effect in the game world. Used by the
/// 3D (Third Dawn / KR) client. The first 35 bytes after the command byte
/// mirror the body of [`GraphicalEffect`] (0xC0) exactly, including the
/// `hue` and `renderMode` fields introduced in that packet.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  cmd                    — 0xC7
/// ── preamble (35 bytes, same as 0xC0 without cmd) ──────────────────────
/// BYTE[1]  direction_type         — 0x00 projectile, 0x01 lightning,
///                                   0x02 stationary, 0x03 follow source
/// BYTE[4]  source_serial
/// BYTE[4]  target_serial
/// BYTE[2]  model                  — graphic / itemID of effect
/// BYTE[2]  x
/// BYTE[2]  y
/// BYTE[1]  z                      — signed
/// BYTE[2]  target_x
/// BYTE[2]  target_y
/// BYTE[1]  target_z               — signed
/// BYTE[1]  speed
/// BYTE[1]  duration
/// BYTE[2]  unk                    — on OSI flamestrike packets = 0x0100
/// BYTE[1]  fixed_direction        — 0 = rotate during travel, 1 = fixed
/// BYTE[1]  explode                — 0 = no explosion, 1 = explode
/// BYTE[4]  hue
/// BYTE[4]  render_mode
/// ── 3D-particle extras (13 bytes) ──────────────────────────────────────
/// BYTE[2]  particle_effect        — tile ID of the particle effect
/// BYTE[2]  particle_explode       — explode effect tile ID (0 = none)
/// BYTE[2]  particle_move_effect   — additional effect for moving effects
///                                   (0 otherwise)
/// BYTE[4]  particle_item_id       — item serial when type == 0x02, else 0
/// BYTE[1]  layer                  — character layer (0–4), 0xFF for moving
///                                   effects or when target is not a char
/// BYTE[2]  particle_unk_effect    — unknown; set only for moving effects
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = 0xC7, size = fixed(49), endian = "be")]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Particle3DEffect {
    pub id: u8,
    // ── preamble — identical to 0xC0 body ─────────────────────────────
    /// Effect direction type (0x00–0x03).
    pub direction_type: u8,
    /// Serial of the source character/object.
    pub source_serial: u32,
    /// Serial of the target character/object.
    pub target_serial: u32,
    /// Graphic model of the first frame of the effect.
    pub model: u16,
    /// Source X coordinate.
    pub x: u16,
    /// Source Y coordinate.
    pub y: u16,
    /// Source Z coordinate (signed).
    pub z: i8,
    /// Target X coordinate.
    pub target_x: u16,
    /// Target Y coordinate.
    pub target_y: u16,
    /// Target Z coordinate (signed).
    pub target_z: i8,
    /// Speed of the animation.
    pub speed: u8,
    /// Duration (`0` = very long, `1` = shortest).
    pub duration: u8,
    /// Unknown word; flamestrike packets use `0x0100`.
    pub unk: u16,
    /// Whether to keep facing fixed during travel (`0` = rotate, `1` = fixed).
    pub fixed_direction: u8,
    /// Explode on impact (`0` = no, `1` = yes).
    pub explode: u8,
    /// Hue / colour tint of the effect.
    pub hue: u32,
    /// Render mode / blending mode of the effect.
    pub render_mode: u32,
    // ── 3D-particle extras ─────────────────────────────────────────────
    /// Tile ID of the particle system effect.
    pub particle_effect: u16,
    /// Tile ID of the explosion particle effect (0 = none).
    pub particle_explode: u16,
    /// Additional moving-effect particle graphic (0 otherwise).
    pub particle_move_effect: u16,
    /// Item serial when `direction_type` == 0x02 (target is item), else 0.
    pub particle_item_id: u32,
    /// Character layer, or [`Layer::MovingEffect`] (0xFF) when target is not
    /// a character / for moving effects.
    pub layer: Layer,
    /// Unknown additional effect; only set for moving effects.
    pub particle_unk_effect: u16,
}

// ── 0xF3 ObjectInfoSA (24 bytes, fixed, S→C) ─────────────────────────────

/// Data type discriminator for [`ObjectInfoSA`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[repr(u8)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ObjectDataType {
    /// Regular item.
    #[wire_enum(0x00, "item")]
    Item,
    /// Multi object (house, boat, …).
    #[wire_enum(0x02, "multi")]
    Multi,
    #[wire_enum(unknown)]
    Unknown(u8),
}

/// Packet 0xF3 — Object Information SA (26 bytes, fixed, S→C)
///
/// Introduced with client 7.0.0.0 and used by OSI to send **all** world
/// objects, including those with graphics > 0x3FFF which the older
/// [`ObjectInfo`] (0x1A) packet cannot represent.
///
/// Also delivered as sub-packets inside [`PacketList`] (0xF7).
///
/// # Notes
///
/// - For Multi objects: `graphic_inc = 0`, `direction = 0`, `amount` fields
///   = 1, `hue = 0`, `flags = 0`, `highlight = 0`.
/// - `amount` is sent twice on the wire (`amount` and `amount2`); both fields
///   are decoded and stored but should normally be equal.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xF3
/// BYTE[2]  0x0001       — always 1 on OSI
/// BYTE[1]  data_type    — 0x00 = Item, 0x02 = Multi
/// BYTE[4]  serial
/// BYTE[2]  graphic
/// BYTE[1]  graphic_inc  — graphic modifier / animation index (0 for Multi)
/// BYTE[2]  amount        — 0x0001 for Multi
/// BYTE[2]  amount2       — 0x0001 for Multi (sent twice, reason unknown)
/// BYTE[2]  x
/// BYTE[2]  y
/// BYTE[1]  z
/// BYTE[1]  direction    — facing direction (0 for Multi)
/// BYTE[2]  hue          — colour / hue (0 for Multi)
/// BYTE[1]  flags        — 0x20 = movable (if normally not), 0x80 = hidden
///                         0x00 for Multi
/// BYTE[2]  highlight    — notoriety/highlight colour (0 normally)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = 0xF3, size = fixed(26), endian = "be")]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObjectInfoSA {
    pub id: u8,
    #[binary(const_value = 0x0001u16)]
    pub _header: u16,
    pub data_type: ObjectDataType,
    pub serial: u32,
    pub graphic: u16,
    /// Graphic modifier / animation index (0 for Multi).
    pub graphic_inc: u8,
    /// Item count (1 for Multi).
    pub amount: u16,
    /// Duplicate of `amount` — sent twice by the server; reason unknown.
    pub amount2: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    /// Facing direction (0 for Multi).
    pub direction: u8,
    /// Hue / colour (0 for Multi).
    pub hue: u16,
    /// Status flags: 0x20 = movable, 0x80 = hidden.
    pub flags: u8,
    /// Notoriety / highlight colour (normally 0).
    pub highlight: u16,
}

impl ObjectInfoSA {
    /// Create a packet for a regular item.
    #[allow(clippy::too_many_arguments)]
    pub fn item(
        serial: u32,
        graphic: u16,
        graphic_inc: u8,
        amount: u16,
        x: u16,
        y: u16,
        z: i8,
        direction: u8,
        hue: u16,
        flags: u8,
        highlight: u16,
    ) -> Self {
        Self {
            id: Self::ID,
            _header: 0x0001,
            data_type: ObjectDataType::Item,
            serial,
            graphic,
            graphic_inc,
            amount,
            amount2: amount,
            x,
            y,
            z,
            direction,
            hue,
            flags,
            highlight,
        }
    }

    /// Create a packet for a Multi object (house, boat, etc.).
    pub fn multi(serial: u32, graphic: u16, x: u16, y: u16, z: i8) -> Self {
        Self {
            id: Self::ID,
            _header: 0x0001,
            data_type: ObjectDataType::Multi,
            serial,
            graphic,
            graphic_inc: 0,
            amount: 1,
            amount2: 1,
            x,
            y,
            z,
            direction: 0,
            hue: 0,
            flags: 0,
            highlight: 0,
        }
    }
}

// ── 0xF7 PacketList (dynamic, S→C) ────────────────────────────────────────

/// Packet 0xF7 — Packet List (dynamic, S→C)
///
/// A batch container introduced with High Seas that carries one or more
/// [`ObjectInfoSA`] (0xF3) sub-packets in a single TCP write.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xF7
/// BYTE[2]  length         — total packet length including header
/// BYTE[2]  count          — number of sub-packets
/// × count:
///   BYTE[1]  0xF3         — sub-packet id (only 0xF3 is currently known)
///   BYTE[2]  0x0001       — always 1 (ObjectInfoSA._header)
///   BYTE[1]  data_type
///   BYTE[4]  serial
///   BYTE[2]  graphic
///   BYTE[1]  graphic_inc
///   BYTE[2]  amount
///   BYTE[2]  amount2
///   BYTE[2]  x
///   BYTE[2]  y
///   BYTE[1]  z
///   BYTE[1]  direction
///   BYTE[2]  hue
///   BYTE[1]  flags
///   BYTE[2]  highlight
/// ```
///
/// Sub-packets with an id other than 0xF3 are silently skipped — decoding
/// stops at the first unknown id so as not to misinterpret the remaining
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PacketList {
    /// The decoded sub-packets. May be fewer than the advertised `count` if
    /// an unknown sub-packet id was encountered.
    pub items: Vec<ObjectInfoSA>,
}

impl PacketList {
    /// Convenience constructor.
    pub fn new(items: Vec<ObjectInfoSA>) -> Self {
        Self { items }
    }
}

impl ManualPacket for PacketList {
    const ID: u8 = 0xF7;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Header: id(1) + len(2) + count(2) = 5 bytes minimum.
        let mut r = packet_reader(data, 0xF7, 5, false)?;
        let _pkt_len: u16 = Decode::<BE>::decode(&mut r)?;
        let count: u16 = Decode::<BE>::decode(&mut r)?;

        let mut items = Vec::with_capacity(count as usize);

        for _ in 0..count {
            // Each sub-packet starts with a sub-id byte.
            let sub_id: u8 = Decode::<BE>::decode(&mut r)?;
            if sub_id != ObjectInfoSA::ID {
                // Unknown sub-packet — cannot determine its length, stop here.
                break;
            }
            // Read the remaining 25 bytes of ObjectInfoSA (after its id byte).
            let _header: u16 = Decode::<BE>::decode(&mut r)?; // always 0x0001
            let data_type: ObjectDataType = Decode::<BE>::decode(&mut r)?;
            let serial: u32 = Decode::<BE>::decode(&mut r)?;
            let graphic: u16 = Decode::<BE>::decode(&mut r)?;
            let graphic_inc: u8 = Decode::<BE>::decode(&mut r)?;
            let amount: u16 = Decode::<BE>::decode(&mut r)?;
            let amount2: u16 = Decode::<BE>::decode(&mut r)?;
            let x: u16 = Decode::<BE>::decode(&mut r)?;
            let y: u16 = Decode::<BE>::decode(&mut r)?;
            let z: i8 = Decode::<BE>::decode(&mut r)?;
            let direction: u8 = Decode::<BE>::decode(&mut r)?;
            let hue: u16 = Decode::<BE>::decode(&mut r)?;
            let flags: u8 = Decode::<BE>::decode(&mut r)?;
            let highlight: u16 = Decode::<BE>::decode(&mut r)?;

            items.push(ObjectInfoSA {
                id: ObjectInfoSA::ID,
                _header: 0x0001,
                data_type,
                serial,
                graphic,
                graphic_inc,
                amount,
                amount2,
                x,
                y,
                z,
                direction,
                hue,
                flags,
                highlight,
            });
        }

        Ok(Self { items })
    }

    fn to_bytes(&self) -> bytes::Bytes {
        // 5-byte header + 26 bytes per sub-packet.
        let cap = 5 + self.items.len() * 26;
        let mut w = BinaryWriter::<BE>::with_capacity(cap);
        self.encode(&mut w);
        w.set_u16_at(1, w.len() as u16);
        w.finish()
    }
}

impl Encode<BE> for PacketList {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()
        w.put_u16(self.items.len() as u16);
        for item in &self.items {
            item.encode(w);
        }
    }
}

// ── 0xE6 RemoveWaypoint (5 bytes, fixed, S→C) ─────────────────────────────

/// Packet 0xE6 — Remove Waypoint (5 bytes, fixed, S→C)
///
/// Instructs the client to remove a previously displayed waypoint.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xE6
/// BYTE[4]  serial       — waypoint serial to remove
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = 0xE6, size = fixed(5), endian = "be")]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RemoveWaypoint {
    pub id: u8,
    pub serial: u32,
}

// ── WaypointsType ──────────────────────────────────────────────────────────

/// Waypoint category, embedded in [`DisplayWaypoint`].
///
/// | Wire | Meaning      |
/// |------|--------------|
/// | 0    | Corpse       |
/// | 1    | Party member |
/// | 2    | Quest        |
/// | 3    | Objective    |
/// | 4    | Unknown      |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[repr(u16)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WaypointType {
    #[wire_enum(0x0000, "corpse")]
    Corpse,
    #[wire_enum(0x0001, "party_member")]
    PartyMember,
    #[wire_enum(0x0002, "quest")]
    Quest,
    #[wire_enum(0x0003, "objective")]
    Objective,
    #[wire_enum(unknown)]
    Unknown(u16),
}

// ── 0xE5 DisplayWaypoint (dynamic, S→C) ───────────────────────────────────

/// Packet 0xE5 — Display Waypoint (dynamic, S→C)
///
/// Instructs the client to show a named waypoint marker on the map.
/// The `name` field is a null-terminated **little-endian** UTF-16 string,
/// matching ClassicUO's `ReadUnicodeLE()`.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xE5
/// BYTE[2]  length         — total packet length including header
/// BYTE[4]  serial
/// BYTE[2]  x
/// BYTE[2]  y
/// BYTE[1]  z              — signed
/// BYTE[1]  map
/// BYTE[2]  waypoint_type
/// BYTE[2]  ignore_object  — non-zero = true
/// BYTE[4]  cliloc
/// BYTE[*]  name           — null-terminated LE UTF-16
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DisplayWaypoint {
    pub serial: u32,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub map: u8,
    pub waypoint_type: WaypointType,
    pub ignore_object: bool,
    pub cliloc: u32,
    pub name: String,
}

impl ManualPacket for DisplayWaypoint {
    const ID: u8 = 0xE5;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Header: id(1) + len(2) + serial(4) + x(2) + y(2) + z(1) + map(1)
        //         + type(2) + ignore(2) + cliloc(4) + min 2 (null terminator) = 21
        let mut r = packet_reader(data, 0xE5, 21, false)?;
        let _len: u16 = Decode::<BE>::decode(&mut r)?;
        let serial: u32 = Decode::<BE>::decode(&mut r)?;
        let x: u16 = Decode::<BE>::decode(&mut r)?;
        let y: u16 = Decode::<BE>::decode(&mut r)?;
        let z: i8 = Decode::<BE>::decode(&mut r)?;
        let map: u8 = Decode::<BE>::decode(&mut r)?;
        let waypoint_type: WaypointType = Decode::<BE>::decode(&mut r)?;
        let ignore_raw: u16 = Decode::<BE>::decode(&mut r)?;
        let cliloc: u32 = Decode::<BE>::decode(&mut r)?;

        // Remaining bytes are a null-terminated LE UTF-16 string.
        let name = decode_le_utf16_str(&mut r)?;

        Ok(Self {
            serial,
            x,
            y,
            z,
            map,
            waypoint_type,
            ignore_object: ignore_raw != 0,
            cliloc,
            name,
        })
    }

    fn to_bytes(&self) -> bytes::Bytes {
        let name_units: Vec<u16> = self.name.encode_utf16().collect();
        // id(1) + len(2) + serial(4) + x(2) + y(2) + z(1) + map(1)
        // + type(2) + ignore(2) + cliloc(4) + (units+1)*2
        let cap = 19 + (name_units.len() + 1) * 2;
        let mut w = BinaryWriter::<BE>::with_capacity(cap);
        self.encode(&mut w);
        w.set_u16_at(1, w.len() as u16);
        w.finish()
    }
}

impl Encode<BE> for DisplayWaypoint {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()
        w.put_u32(self.serial);
        w.put_u16(self.x);
        w.put_u16(self.y);
        w.put_i8(self.z);
        w.put_u8(self.map);
        w.put_u16(self.waypoint_type.to_wire());
        w.put_u16(if self.ignore_object { 1 } else { 0 });
        w.put_u32(self.cliloc);
        // LE UTF-16 null-terminated name
        encode_le_utf16_str(&self.name, w);
    }
}
