//! Tooltip / property list packets (Mega Cliloc, 0xD6; SE Revision, 0xDC).
//!
//! Two distinct wire formats share the same packet ID 0xD6:
//!
//! - [`MegaClilocRequest`] — sent **client → server** to request tooltip
//!   data for one or more serials.
//! - [`MegaClilocResponse`] — sent **server → client** carrying the ordered
//!   list of cliloc properties for a single object.
//!
//! [`TooltipRevision`] (0xDC) is a compact server-to-client packet that
//! carries the revision hash of an object's tooltip, allowing the client
//! to determine whether its cached tooltip is still current.

use u_io::{BE, BinaryWriter, Decode, Encode, BasicPacket, packet_reader, decode_le_utf16_bytes, encode_le_utf16_bytes};
use macros::Packet;

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── ClilocEntry ────────────────────────────────────────────────────────────

/// A single property entry inside a [`MegaClilocResponse`].
///
/// Each entry consists of a cliloc number and an optional argument string
/// that is interpolated into the localised text at the client side.
/// Arguments are encoded as **little-endian UTF-16** on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClilocEntry {
    /// Cliloc string number (e.g. `1042971`).
    pub cliloc_id: u32,
    /// Optional argument string, interpolated into the cliloc text.
    /// `None` when the wire length field is `0`.
    pub text: Option<String>,
}

// ── 0xD6 MegaClilocRequest (dynamic, C→S) ─────────────────────────────────

/// Packet 0xD6 — Mega Cliloc Request (dynamic, C→S)
///
/// Sent by the client to ask the server for tooltip/property information
/// about one or more objects identified by their serials.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xD6
/// BYTE[2]  total packet length
/// Loop:
///   BYTE[4] object serial
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MegaClilocRequest {
    /// Serials of the objects whose tooltip data is requested.
    pub serials: Vec<u32>,
}

impl MegaClilocRequest {
    /// Create a request for a single serial.
    pub fn new(serial: u32) -> Self {
        Self { serials: vec![serial] }
    }

    /// Create a request for multiple serials.
    pub fn with_serials(serials: Vec<u32>) -> Self {
        Self { serials }
    }
}

impl ManualPacket for MegaClilocRequest {
    const ID: u8 = 0xD6;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) = 3 bytes (empty request is valid)
        let mut r = packet_reader(data, Self::ID, 3, true)?;

        // Each remaining 4-byte group is a serial.
        let count = r.remaining_len() / 4;
        let mut serials = Vec::with_capacity(count);
        for _ in 0..count {
            let serial: u32 = Decode::decode(&mut r)?;
            serials.push(serial);
        }

        Ok(Self { serials })
    }
}

impl Encode<BE> for MegaClilocRequest {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()
        for &serial in &self.serials {
            w.put_u32(serial);
        }
    }
}

// ── 0xD6 MegaClilocResponse (dynamic, S→C) ────────────────────────────────

/// Packet 0xD6 — Mega Cliloc Response (dynamic, S→C)
///
/// Sent by the server to deliver the ordered list of cliloc properties for
/// a single object (e.g. an item or mobile).  The name is always the first
/// entry in [`entries`](MegaClilocResponse::entries).
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xD6
/// BYTE[2]  total packet length
/// BYTE[2]  0x0001  (constant)
/// BYTE[4]  serial           — the object's own serial
/// BYTE[2]  0x0000           (constant, always zero in all known captures)
/// BYTE[4]  tooltip_serial   — serial of the item the tooltip appears over
///                             (equals serial in all known captures)
/// Loop until cliloc_id == 0:
///   BYTE[4]  cliloc_id
///   BYTE[2]  text_len       — byte length of the UTF-16 LE argument string
///   BYTE[?]  text           — little-endian UTF-16, NOT null-terminated
///                             (omitted when text_len == 0)
/// BYTE[4]  0x00000000       — end-of-packet marker
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MegaClilocResponse {
    /// Serial of the object whose properties are described.
    pub serial: u32,
    /// Serial of the item the tooltip appears over.
    ///
    /// In all known server captures this equals [`serial`](Self::serial).
    /// It may differ when a tooltip is displayed relative to a different
    /// object (e.g. a contained item shown over its container).
    pub tooltip_serial: u32,
    /// Ordered list of cliloc property entries.  The object name is always
    /// the first entry.
    pub entries: Vec<ClilocEntry>,
}

impl MegaClilocResponse {
    /// Create a response with `tooltip_serial` equal to `serial`.
    pub fn new(serial: u32, entries: Vec<ClilocEntry>) -> Self {
        Self { serial, tooltip_serial: serial, entries }
    }
}

impl ManualPacket for MegaClilocResponse {
    const ID: u8 = 0xD6;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + 0x0001(2) + serial(4) + pad(2)
        //          + tooltip_serial(4) + terminator(4) = 19 bytes
        let mut r = packet_reader(data, Self::ID, 19, true)?;

        // Skip the constant 0x0001 header word.
        let _header: u16 = Decode::decode(&mut r)?;

        let serial: u32 = Decode::decode(&mut r)?;

        // Skip the two zero bytes between the serials.
        let _pad: u16 = Decode::decode(&mut r)?;

        let tooltip_serial: u32 = Decode::decode(&mut r)?;

        // Decode the property loop.
        let mut entries = Vec::new();
        loop {
            let cliloc_id: u32 = Decode::decode(&mut r)?;
            if cliloc_id == 0 {
                break;
            }

            let text_len: u16 = Decode::decode(&mut r)?;
            let text = if text_len == 0 {
                None
            } else {
                // Decode `text_len` raw bytes as little-endian UTF-16.
                Some(decode_le_utf16_bytes(&mut r, text_len as usize)?)
            };

            entries.push(ClilocEntry { cliloc_id, text });
        }

        Ok(Self { serial, tooltip_serial, entries })
    }
}

impl Encode<BE> for MegaClilocResponse {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()

        w.put_u16(0x0001); // constant header word
        w.put_u32(self.serial);
        w.put_u16(0x0000); // constant pad
        w.put_u32(self.tooltip_serial);

        for entry in &self.entries {
            w.put_u32(entry.cliloc_id);
            match &entry.text {
                None => {
                    w.put_u16(0); // text_len = 0
                }
                Some(s) => {
                    // Encode the argument string as little-endian UTF-16.
                    let units_count: usize = s.encode_utf16().count();
                    let byte_len = (units_count * 2) as u16;
                    w.put_u16(byte_len);
                    encode_le_utf16_bytes(s, w);
                }
            }
        }

        w.put_u32(0x00000000); // end-of-packet marker
    }
}

// ── 0xDC TooltipRevision (9 bytes, fixed, S→C) ────────────────────────────

/// Packet 0xDC — SE Introduced Revision / Tooltip Revision Hash (9 bytes, fixed, S→C)
///
/// Introduced in late 2004 (Samurai Empire era).  Sent by the server to
/// tell the client the current revision hash of an object's tooltip.  The
/// client compares this against its cached revision to decide whether to
/// request fresh tooltip data via [`MegaClilocRequest`] (0xD6 C→S).
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xDC
/// BYTE[4]  serial   — serial of the item or mobile
/// BYTE[4]  revision — tooltip revision hash
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xDC, size = fixed(9), endian = "be")]
pub struct TooltipRevision {
    pub id: u8,
    /// Serial of the item or mobile whose tooltip revision is being reported.
    pub serial: u32,
    /// Revision hash of the tooltip property list.
    pub revision: u32,
}

impl TooltipRevision {
    /// Create a new tooltip revision packet.
    pub fn new(serial: u32, revision: u32) -> Self {
        Self { id: Self::ID, serial, revision }
    }
}
