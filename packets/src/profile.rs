//! Character profile packets (0xB8).
//!
//! | Packet | Name            | Direction |
//! |--------|-----------------|-----------|
//! | 0xB8   | [`CharProfile`] | Both      |
//!
//! The 0xB8 packet serves three distinct purposes depending on direction and
//! content, distinguished by the `mode` byte (client-only) and `cmd_type`
//! field:
//!
//! | Mode / trigger                     | Variant                              |
//! |------------------------------------|--------------------------------------|
//! | C→S, no cmd_type                   | [`CharProfile::Request`]             |
//! | C→S, cmd_type = 0x0001             | [`CharProfile::Update`]              |
//! | S→C                                | [`CharProfile::Response`]            |

use std::fmt;

use u_io::{BE, BinaryWriter, Decode, Encode, NullString, NullUnicodeString, packet_reader};

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── 0xB8 CharProfile ──────────────────────────────────────────────────────

/// Packet 0xB8 — Request / Char Profile (variable, both directions)
///
/// # Wire layout — Request (C→S, mode = 0x00 or 0x01)
///
/// ```text
/// BYTE[1]  0xB8
/// BYTE[2]  length        — total packet length
/// BYTE[1]  mode          — client-only byte (0 = request, never 0x0001 here)
/// BYTE[4]  serial        — serial of the character to inspect
/// ```
///
/// # Wire layout — Update (C→S, mode byte present, followed by cmd_type 0x0001)
///
/// ```text
/// BYTE[1]  0xB8
/// BYTE[2]  length
/// BYTE[1]  mode          — (present in all C→S packets)
/// BYTE[4]  serial
/// BYTE[2]  cmd_type      — 0x0001 = update
/// BYTE[2]  msglen        — number of UTF-16 code units (not byte count)
/// BYTE[msglen * 2] new_profile — UTF-16 BE, NOT null-terminated
/// ```
///
/// # Wire layout — Response (S→C, no mode byte)
///
/// ```text
/// BYTE[1]  0xB8
/// BYTE[2]  length
/// BYTE[4]  serial
/// BYTE[?]  title          — ASCII/Latin-1, null-terminated
/// BYTE[?*2] static_profile — UTF-16 BE, null-terminated (read-only)
/// BYTE[?*2] profile        — UTF-16 BE, null-terminated (editable)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CharProfile {
    /// C→S: client requests to view a character's profile.
    Request {
        /// Serial of the character whose profile is being requested.
        serial: u32,
    },
    /// C→S: client submits a new profile text.
    Update {
        /// Serial of the character being edited.
        serial: u32,
        /// New profile text (UTF-16 BE on the wire, not null-terminated).
        new_profile: String,
    },
    /// S→C: server sends the full profile data for display.
    Response {
        /// Serial of the character.
        serial: u32,
        /// Character title (ASCII/Latin-1, null-terminated on wire).
        title: String,
        /// Read-only static portion of the profile (UTF-16 BE, null-terminated).
        static_profile: String,
        /// Editable profile text (UTF-16 BE, null-terminated).
        profile: String,
    },
}

impl CharProfile {
    pub const ID: u8 = 0xB8;
}

impl fmt::Display for CharProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { serial } =>
                write!(f, "CharProfile::Request(serial=0x{serial:08X})"),
            Self::Update { serial, new_profile } =>
                write!(f, "CharProfile::Update(serial=0x{serial:08X}, len={})", new_profile.len()),
            Self::Response { serial, title, .. } =>
                write!(f, "CharProfile::Response(serial=0x{serial:08X}, title={title:?})"),
        }
    }
}

impl ManualPacket for CharProfile {
    const ID: u8 = 0xB8;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: cmd(1) + len(2) + serial(4) = 7 bytes (server response)
        // Client adds mode(1) before serial: cmd(1) + len(2) + mode(1) + serial(4) = 8 bytes
        let mut r = packet_reader(data, Self::ID, 7, true)?;

        // Peek at byte index 3 (after cmd + len[2]):
        // - C→S packets always have a `mode` byte here before the serial
        // - S→C packets go straight to the serial (4 bytes)
        //
        // We detect direction by total length vs content: both the client
        // request (8 bytes) and update (>8) have a `mode` byte. The server
        // response (≥ 8 bytes) has no mode byte, but we cannot distinguish
        // purely by length.
        //
        // The reliable heuristic: if data[3] is 0x00 or 0x01 (plausible mode
        // values) AND data[4..8] is a plausible serial, treat as C→S.
        // In practice the server response goes: serial (4 bytes), then a
        // non-null ASCII character — so data[3] would be the high byte of
        // the serial (almost always 0x40..0x7F for player serials, never 0x00
        // or 0x01). We therefore use:
        //   mode byte present ↔ data[3] <= 0x01
        //
        // This matches all known UO client sends (mode is always 0 or 1)
        // and is safe because valid serials never start with 0x00 or 0x01.

        let is_client = data.len() >= 4 && data[3] <= 0x01;

        if is_client {
            let _mode: u8 = Decode::decode(&mut r)?;
            let serial: u32 = Decode::decode(&mut r)?;

            if r.remaining_len() < 2 {
                // No cmd_type — pure request
                return Ok(Self::Request { serial });
            }

            let cmd_type: u16 = Decode::decode(&mut r)?;

            if cmd_type == 0x0001 {
                // Update request: msglen + UTF-16 BE chars (no null terminator)
                let msglen: u16 = Decode::decode(&mut r)?;
                let byte_len = (msglen as usize) * 2;
                let raw = r.read_slice(byte_len)?;
                let units: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                let new_profile = String::from_utf16_lossy(&units).to_owned();
                Ok(Self::Update { serial, new_profile })
            } else {
                // Unknown sub-command — treat as bare request to avoid data loss
                Ok(Self::Request { serial })
            }
        } else {
            // Server response: serial, then null-terminated strings
            let serial: u32 = Decode::decode(&mut r)?;
            let title_ns: NullString = Decode::decode(&mut r)?;
            let static_ns: NullUnicodeString = Decode::decode(&mut r)?;
            let profile_ns: NullUnicodeString = Decode::decode(&mut r)?;

            Ok(Self::Response {
                serial,
                title: title_ns.0,
                static_profile: static_ns.0,
                profile: profile_ns.0,
            })
        }
    }
}

impl Encode<BE> for CharProfile {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        // Length placeholder (backfilled at a higher layer or left 0 for
        // callers that use a length-patching writer).
        w.put_u16(0);

        match self {
            Self::Request { serial } => {
                w.put_u8(0x00); // mode
                w.put_u32(*serial);
            }
            Self::Update { serial, new_profile } => {
                w.put_u8(0x01); // mode (update)
                w.put_u32(*serial);
                w.put_u16(0x0001); // cmd_type
                let units: Vec<u16> = new_profile.encode_utf16().collect();
                w.put_u16(units.len() as u16); // msglen (char count)
                for unit in &units {
                    w.put_u16(*unit); // BE UTF-16
                }
            }
            Self::Response { serial, title, static_profile, profile } => {
                w.put_u32(*serial);
                // title: null-terminated ASCII
                w.put_slice(title.as_bytes());
                w.put_u8(0x00);
                // static_profile: null-terminated UTF-16 BE
                for unit in static_profile.encode_utf16() {
                    w.put_u16(unit);
                }
                w.put_u16(0x0000);
                // profile: null-terminated UTF-16 BE
                for unit in profile.encode_utf16() {
                    w.put_u16(unit);
                }
                w.put_u16(0x0000);
            }
        }
    }
}
