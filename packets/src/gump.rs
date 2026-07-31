//! Gump (generic dialog) packets.
//!
//! | Packet | Name                              | Direction |
//! |--------|-----------------------------------|-----------|
//! | 0x7C   | [`OpenDialogBox`]                 | S→C       |
//! | 0x7D   | [`ResponseToDialogBox`]           | C→S       |
//! | 0xB0   | [`SendGumpDialog`]                | S→C       |
//! | 0xB1   | [`GumpMenuSelection`]             | C→S       |
//! | 0xDD   | [`SendCompressedGump`]            | S→C       |

use u_io::{BE, BinaryReader, BinaryWriter, Decode, Encode, ReadPrimitives, packet_reader};
use u_io::DecodeError;

use crate::compress::{zlib_compress, zlib_decompress};
use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── 0xB0 SendGumpDialog (dynamic, S→C) ────────────────────────────────────

/// A single text line in the gump dialog (big-endian UTF-16 on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct GumpTextLine(pub String);

/// Packet 0xB0 — Send Gump Menu Dialog (dynamic, S→C)
///
/// Sends a generic gump dialog to the client. The `commands` string
/// contains the GUMP layout language, and `text_lines` carries the
/// text entries referenced by the layout.
///
/// # Notes on `trailing_pad`
///
/// Some servers (e.g. ServUO / RunUO derivatives) append one or more
/// zero-bytes after the last text line. These bytes have no documented
/// meaning; they are captured verbatim in `trailing_pad` so that
/// roundtrip re-encoding reproduces the original packet exactly.
/// For server-generated packets this field should be empty (`vec![]`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SendGumpDialog {
    pub serial: u32,
    pub gump_id: u32,
    pub x: u32,
    pub y: u32,
    /// Null-terminated layout/command string (gump layout language).
    pub layout: String,
    /// Text lines — each is a length-prefixed big-endian UTF-16 string.
    pub text_lines: Vec<GumpTextLine>,
    /// Any bytes that follow the last text line (server padding / quirk).
    pub trailing_pad: Vec<u8>,
}

impl ManualPacket for SendGumpDialog {
    const ID: u8 = 0xB0;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 24, true)?;
        let serial: u32 = Decode::decode(&mut r)?;
        let gump_id: u32 = Decode::decode(&mut r)?;
        let x: u32 = Decode::decode(&mut r)?;
        let y: u32 = Decode::decode(&mut r)?;

        // Command section: length-prefixed, zero-terminated ASCII string.
        let cmd_len: u16 = Decode::decode(&mut r)?;
        let mut cmd_buf = vec![0u8; cmd_len as usize];
        r.read_bytes(&mut cmd_buf)?;
        // Strip the trailing null if present.
        let commands = if cmd_buf.last() == Some(&0) {
            String::from_utf8_lossy(&cmd_buf[..cmd_buf.len() - 1]).into_owned()
        } else {
            String::from_utf8_lossy(&cmd_buf).into_owned()
        };

        // Text lines.
        let num_lines: u16 = Decode::decode(&mut r)?;
        let mut text_lines = Vec::with_capacity(num_lines as usize);
        for _ in 0..num_lines {
            let text_len: u16 = Decode::decode(&mut r)?; // length in u16 chars
            let mut units = Vec::with_capacity(text_len as usize);
            for _ in 0..text_len {
                units.push(r.read_u16()?);
            }
            // Some shards pad strings to a fixed size with null + garbage.
            // Truncate at the first NUL before decoding.
            let units = match units.iter().position(|&c| c == 0) {
                Some(pos) => &units[..pos],
                None => &units[..],
            };
            let s = String::from_utf16_lossy(units).to_owned();
            text_lines.push(GumpTextLine(s));
        }

        // Consume any trailing padding bytes the server may have appended.
        let trailing_pad = r.read_slice(r.remaining_len())
            .unwrap_or(&[])
            .to_vec();

        Ok(Self { serial, gump_id, x, y, layout: commands, text_lines, trailing_pad })
    }
}

impl Encode<BE> for SendGumpDialog {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.serial);
        w.put_u32(self.gump_id);
        w.put_u32(self.x);
        w.put_u32(self.y);

        // Command section: length (including null terminator) + data + null.
        let cmd_bytes = self.layout.as_bytes();
        w.put_u16((cmd_bytes.len() + 1) as u16); // +1 for null
        w.put_slice(cmd_bytes);
        w.put_u8(0); // null terminator

        // Text lines.
        w.put_u16(self.text_lines.len() as u16);
        for line in &self.text_lines {
            let units: Vec<u16> = line.0.encode_utf16().collect();
            w.put_u16(units.len() as u16);
            for u in &units {
                w.put_u16(*u);
            }
        }

        // Re-emit any trailing padding bytes captured during decode.
        if !self.trailing_pad.is_empty() {
            w.put_slice(&self.trailing_pad);
        }
    }
}

// ── 0xB1 GumpMenuSelection (dynamic, C→S) ─────────────────────────────────

/// A text entry response from a gump dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GumpTextEntry {
    pub text_id: u16,
    /// Unicode text (big-endian UTF-16, not null-terminated on the wire).
    pub text: String,
}

/// Packet 0xB1 — Gump Menu Selection (dynamic, C→S)
///
/// Sent by the client when the player interacts with a gump dialog.
/// Contains the button pressed, any active switches (checkboxes/radios),
/// and any text entry responses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GumpMenuSelection {
    pub serial: u32,
    pub gump_id: u32,
    pub button_id: u32,
    /// Switch IDs that are turned on (radio buttons / checkboxes).
    pub switches: Vec<u32>,
    /// Text entry responses.
    pub text_entries: Vec<GumpTextEntry>,
}

impl ManualPacket for GumpMenuSelection {
    const ID: u8 = 0xB1;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 23, true)?;
        let serial: u32 = Decode::decode(&mut r)?;
        let gump_id: u32 = Decode::decode(&mut r)?;
        let button_id: u32 = Decode::decode(&mut r)?;

        // Switches.
        let switch_count: u32 = Decode::decode(&mut r)?;
        let mut switches = Vec::with_capacity(switch_count as usize);
        for _ in 0..switch_count {
            switches.push(Decode::<BE>::decode(&mut r)?);
        }

        // Text entries.
        let text_count: u32 = Decode::decode(&mut r)?;
        let mut text_entries = Vec::with_capacity(text_count as usize);
        for _ in 0..text_count {
            let text_id: u16 = Decode::decode(&mut r)?;
            let text_len: u16 = Decode::decode(&mut r)?; // length in u16 chars
            let mut units = Vec::with_capacity(text_len as usize);
            for _ in 0..text_len {
                units.push(r.read_u16()?);
            }
            // Truncate at the first NUL (some shards pad with null + garbage).
            let units = match units.iter().position(|&c| c == 0) {
                Some(pos) => &units[..pos],
                None => &units[..],
            };
            let text = String::from_utf16_lossy(units).to_owned();
            text_entries.push(GumpTextEntry { text_id, text });
        }

        Ok(Self { serial, gump_id, button_id, switches, text_entries })
    }
}

impl Encode<BE> for GumpMenuSelection {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.serial);
        w.put_u32(self.gump_id);
        w.put_u32(self.button_id);

        // Switches.
        w.put_u32(self.switches.len() as u32);
        for &sw in &self.switches {
            w.put_u32(sw);
        }

        // Text entries.
        w.put_u32(self.text_entries.len() as u32);
        for entry in &self.text_entries {
            w.put_u16(entry.text_id);
            let units: Vec<u16> = entry.text.encode_utf16().collect();
            w.put_u16(units.len() as u16);
            for &u in &units {
                w.put_u16(u);
            }
        }
    }
}

// ── 0x7C OpenDialogBox (dynamic, S→C) ─────────────────────────────────────

/// A single selectable entry in an [`OpenDialogBox`] dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DialogEntry {
    /// Graphic/model ID of the item shown next to the entry.
    /// For grey menus the MSB is always `0x00`.
    pub model_id: u16,
    /// Hue/color of the shown item.
    pub color: u16,
    /// Display text for the entry.
    pub text: String,
}

/// Packet 0x7C — Open Dialog Box (dynamic, S→C)
///
/// Legacy menu/dialog packet.  The server sends a question string and a list
/// of selectable entries; the client replies with [`ResponseToDialogBox`]
/// (0x7D), echoing `dialog_id` and `menu_id` and supplying the chosen index.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]   0x7C
/// BYTE[2]   total packet length
/// BYTE[4]   dialog_id         — echoed back in 0x7D
/// BYTE[2]   menu_id           — echoed back in 0x7D
/// BYTE[1]   question length
/// BYTE[len] question text      — ASCII, not null-terminated
/// BYTE[1]   number of entries
/// For each entry:
///   BYTE[2]   model_id        — graphic shown (MSB=0 for grey menus)
///   BYTE[2]   color           — hue of the graphic
///   BYTE[1]   text length
///   BYTE[len] entry text      — ASCII, not null-terminated
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OpenDialogBox {
    /// Dialog ID echoed back by the client in [`ResponseToDialogBox`].
    pub dialog_id: u32,
    /// Menu ID echoed back by the client in [`ResponseToDialogBox`].
    pub menu_id: u16,
    /// Question/title text displayed at the top of the dialog.
    pub question: String,
    /// Selectable entries shown in the dialog.
    pub entries: Vec<DialogEntry>,
}

impl ManualPacket for OpenDialogBox {
    const ID: u8 = 0x7C;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + dialog_id(4) + menu_id(2) + qlen(1) + count(1) = 11
        let mut r = packet_reader(data, 0x7C, 11, true)?;

        let dialog_id: u32 = Decode::decode(&mut r)?;
        let menu_id:   u16 = Decode::decode(&mut r)?;

        let q_len: u8 = Decode::decode(&mut r)?;
        let q_bytes = r.read_slice(q_len as usize)
            .map_err(|_| PacketError::Decode(DecodeError::Truncated))?;
        let question = String::from_utf8_lossy(q_bytes).into_owned();

        let count: u8 = Decode::decode(&mut r)?;
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let model_id: u16 = Decode::decode(&mut r)?;
            let color:    u16 = Decode::decode(&mut r)?;
            let t_len:    u8  = Decode::decode(&mut r)?;
            let t_bytes = r.read_slice(t_len as usize)
                .map_err(|_| PacketError::Decode(DecodeError::Truncated))?;
            let text = String::from_utf8_lossy(t_bytes).into_owned();
            entries.push(DialogEntry { model_id, color, text });
        }

        Ok(Self { dialog_id, menu_id, question, entries })
    }
}

impl Encode<BE> for OpenDialogBox {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        w.put_u32(self.dialog_id);
        w.put_u16(self.menu_id);

        let q = self.question.as_bytes();
        w.put_u8(q.len() as u8);
        w.put_slice(q);

        w.put_u8(self.entries.len() as u8);
        for entry in &self.entries {
            w.put_u16(entry.model_id);
            w.put_u16(entry.color);
            let t = entry.text.as_bytes();
            w.put_u8(t.len() as u8);
            w.put_slice(t);
        }
    }
}

// ── 0x7D ResponseToDialogBox (13 bytes, fixed, C→S) ───────────────────────

/// Packet 0x7D — Response To Dialog Box (13 bytes, fixed, C→S)
///
/// Sent by the client in reply to an [`OpenDialogBox`] (0x7C) dialog.
/// `dialog_id` and `menu_id` are echoed from the server packet; `index` is
/// the 1-based selection (0 = cancel/close); `model_id` and `color` reflect
/// the item shown for the chosen entry.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x7D
/// BYTE[4]  dialog_id   — echoed from 0x7C
/// BYTE[2]  menu_id     — echoed from 0x7C
/// BYTE[2]  index       — 1-based choice index (0 = cancel)
/// BYTE[2]  model_id    — model # of the chosen entry
/// BYTE[2]  color       — color of the chosen entry
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResponseToDialogBox {
    /// Dialog ID echoed from [`OpenDialogBox`].
    pub dialog_id: u32,
    /// Menu ID echoed from [`OpenDialogBox`].
    pub menu_id: u16,
    /// 1-based index of the selected entry; `0` means the dialog was cancelled.
    pub index: u16,
    /// Model/graphic ID of the chosen entry.
    pub model_id: u16,
    /// Hue/color of the chosen entry.
    pub color: u16,
}

impl ManualPacket for ResponseToDialogBox {
    const ID: u8 = 0x7D;
    const SIZE: PacketSize = PacketSize::Fixed(13);

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, 0x7D, 13, false)?;

        let dialog_id: u32 = Decode::decode(&mut r)?;
        let menu_id:   u16 = Decode::decode(&mut r)?;
        let index:     u16 = Decode::decode(&mut r)?;
        let model_id:  u16 = Decode::decode(&mut r)?;
        let color:     u16 = Decode::decode(&mut r)?;

        Ok(Self { dialog_id, menu_id, index, model_id, color })
    }
}

impl Encode<BE> for ResponseToDialogBox {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u32(self.dialog_id);
        w.put_u16(self.menu_id);
        w.put_u16(self.index);
        w.put_u16(self.model_id);
        w.put_u16(self.color);
    }
}

// ── 0xDD SendCompressedGump (dynamic, S→C) ────────────────────────────────

/// Packet 0xDD — Send Compressed Gump (dynamic, S→C)
///
/// Compressed counterpart of [`SendGumpDialog`] (0xB0).  The layout string
/// and text lines are stored zlib-compressed on the wire.  This struct
/// keeps them in their decompressed form; compression is applied
/// transparently during [`to_bytes()`](ManualPacket::to_bytes).
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  cmd = 0xDD
/// BYTE[2]  length
/// BYTE[4]  serial
/// BYTE[4]  gump_id
/// BYTE[4]  x
/// BYTE[4]  y
/// BYTE[4]  compressed_layout_length    (CLen)
/// BYTE[4]  decompressed_layout_length  (DLen)
/// BYTE[CLen-4]  zlib-compressed layout data
/// BYTE[4]  number_of_text_lines
/// BYTE[4]  compressed_text_length      (CTxtLen)
/// BYTE[4]  decompressed_text_length    (DTxtLen)
/// BYTE[CTxtLen-4]  zlib-compressed text data
/// ```
///
/// Text lines (after decompression) are encoded as:
///
/// ```text
/// for each line:
///   BYTE[2]  char_count (big-endian)
///   BYTE[char_count * 2]  big-endian UTF-16 text (not null-terminated)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SendCompressedGump {
    pub serial: u32,
    pub gump_id: u32,
    pub x: u32,
    pub y: u32,
    /// Gump layout string (null-terminated ASCII on the wire, stored
    /// without the trailing null).
    pub layout: String,
    /// Text lines (big-endian UTF-16 on the wire).
    pub text_lines: Vec<GumpTextLine>,
}

impl ManualPacket for SendCompressedGump {
    const ID: u8 = 0xDD;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: cmd(1)+len(2)+serial(4)+gump(4)+x(4)+y(4)
        //         +clen(4)+dlen(4)+nlines(4)+ctlen(4)+dtlen(4) = 39
        let mut r = packet_reader(data, Self::ID, 39, true)?;

        let serial:  u32 = Decode::decode(&mut r)?;
        let gump_id: u32 = Decode::decode(&mut r)?;
        let x:       u32 = Decode::decode(&mut r)?;
        let y:       u32 = Decode::decode(&mut r)?;

        // ── Layout section ────────────────────────────────────────────
        let compressed_layout_len: u32 = Decode::decode(&mut r)?;
        let _decompressed_layout_len: u32 = Decode::decode(&mut r)?;

        let layout = if compressed_layout_len > 4 {
            let blob = r.read_slice((compressed_layout_len - 4) as usize)
                .map_err(|_| PacketError::Decode(DecodeError::Truncated))?;
            let decompressed = zlib_decompress(blob)?;
            // Null-terminated ASCII — strip trailing null.
            let end = decompressed.iter().position(|&b| b == 0)
                .unwrap_or(decompressed.len());
            String::from_utf8_lossy(&decompressed[..end]).into_owned()
        } else {
            String::new()
        };

        // ── Text lines section ────────────────────────────────────────
        let num_lines: u32 = Decode::decode(&mut r)?;
        let compressed_text_len: u32 = Decode::decode(&mut r)?;
        let _decompressed_text_len: u32 = Decode::decode(&mut r)?;

        let text_lines = if compressed_text_len > 4 {
            let blob = r.read_slice((compressed_text_len - 4) as usize)
                .map_err(|_| PacketError::Decode(DecodeError::Truncated))?;
            let decompressed = zlib_decompress(blob)?;
            decode_gump_text_lines(&decompressed, num_lines)?
        } else {
            Vec::new()
        };

        Ok(Self { serial, gump_id, x, y, layout, text_lines })
    }
}

impl Encode<BE> for SendCompressedGump {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()
        w.put_u32(self.serial);
        w.put_u32(self.gump_id);
        w.put_u32(self.x);
        w.put_u32(self.y);

        // ── Layout section ────────────────────────────────────────────
        let mut layout_raw = self.layout.as_bytes().to_vec();
        layout_raw.push(0); // null terminator
        let layout_compressed = zlib_compress(&layout_raw);
        w.put_u32((layout_compressed.len() + 4) as u32);
        w.put_u32(layout_raw.len() as u32);
        w.put_slice(&layout_compressed);

        // ── Text lines section ────────────────────────────────────────
        let text_raw = encode_gump_text_lines(&self.text_lines);
        let text_compressed = zlib_compress(&text_raw);
        w.put_u32(self.text_lines.len() as u32);
        w.put_u32((text_compressed.len() + 4) as u32);
        w.put_u32(text_raw.len() as u32);
        w.put_slice(&text_compressed);
    }
}

// ── From conversions between SendGumpDialog and SendCompressedGump ─────────

impl From<&SendGumpDialog> for SendCompressedGump {
    fn from(g: &SendGumpDialog) -> Self {
        Self {
            serial: g.serial,
            gump_id: g.gump_id,
            x: g.x,
            y: g.y,
            layout: g.layout.clone(),
            text_lines: g.text_lines.clone(),
        }
    }
}

impl From<&SendCompressedGump> for SendGumpDialog {
    fn from(g: &SendCompressedGump) -> Self {
        Self {
            serial: g.serial,
            gump_id: g.gump_id,
            x: g.x,
            y: g.y,
            layout: g.layout.clone(),
            text_lines: g.text_lines.clone(),
            trailing_pad: Vec::new(),
        }
    }
}

// ── Gump text-line helpers (shared by 0xB0 and 0xDD) ──────────────────────

/// Decode text lines from a decompressed byte buffer.
///
/// Each line: `u16 char_count` + `char_count * 2` bytes of big-endian UTF-16.
fn decode_gump_text_lines(data: &[u8], count: u32) -> Result<Vec<GumpTextLine>, PacketError> {
    let mut r = BinaryReader::<BE>::new(data);
    let mut lines = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let char_count: u16 = Decode::decode(&mut r)?;
        let mut units = Vec::with_capacity(char_count as usize);
        for _ in 0..char_count {
            units.push(r.read_u16()?);
        }
        let s = String::from_utf16_lossy(&units);
        lines.push(GumpTextLine(s));
    }
    Ok(lines)
}

/// Encode text lines into a byte buffer: `u16 char_count` + BE-UTF16 per line.
fn encode_gump_text_lines(lines: &[GumpTextLine]) -> Vec<u8> {
    let mut w = BinaryWriter::<BE>::new();
    for line in lines {
        let units: Vec<u16> = line.0.encode_utf16().collect();
        w.put_u16(units.len() as u16);
        for &u in &units {
            w.put_u16(u);
        }
    }
    w.finish().to_vec()
}
