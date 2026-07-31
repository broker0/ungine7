//! Speech and text packets.

use u_io::{BE, BinaryWriter, Decode, Encode, FixedString, NullString, NullUnicodeString, ReadPrimitives, packet_reader, encode_le_utf16_str, decode_le_utf16_str};
use macros::{Packet, WireEnum};

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── SpeechType ─────────────────────────────────────────────────────────────

/// Type of speech / text message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum SpeechType {
    #[wire_enum(0x00, "normal")]
    Normal,
    #[wire_enum(0x01, "broadcast/system")]
    Broadcast,
    #[wire_enum(0x02, "emote")]
    Emote,
    #[wire_enum(0x06, "system/lower corner")]
    System,
    #[wire_enum(0x07, "message/corner with name")]
    MessageCorner,
    #[wire_enum(0x08, "whisper")]
    Whisper,
    #[wire_enum(0x09, "yell")]
    Yell,
    #[wire_enum(0x0A, "spell")]
    Spell,
    #[wire_enum(0x0D, "guild chat")]
    GuildChat,
    #[wire_enum(0x0E, "alliance chat")]
    AllianceChat,
    #[wire_enum(0x0F, "command prompts")]
    CommandPrompts,
    #[wire_enum(unknown)]
    Unknown(u8),
}

// ── 0x1C SendSpeech (dynamic, S→C) ────────────────────────────────────────

/// Packet 0x1C — Send Speech (dynamic, S→C)
///
/// Used by the server to display text above objects/NPCs or in the
/// system message area. The `serial` is 0xFFFFFFFF for system messages,
/// and `model` is 0xFFFF for system messages.
///
/// Some non-standard shards pack the entire message into the `name` buffer
/// (with no separate message bytes after the 44-byte header). In that case
/// the parser extracts the message from the `name` field itself: everything
/// after the first `\0` inside the fixed 30-byte buffer is treated as the
/// message. Invalid UTF-8 in the message is replaced with U+FFFD.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SendSpeech {
    pub serial: u32,
    pub model: u16,
    pub speech_type: SpeechType,
    pub color: u16,
    pub font: u16,
    pub name: String,
    pub message: String,
}

impl ManualPacket for SendSpeech {
    const ID: u8 = 0x1C;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1)+len(2)+serial(4)+model(2)+type(1)+color(2)+font(2)+name(30) = 44
        let mut r = packet_reader(data, 0x1C, 44, true)?;

        let serial: u32 = Decode::decode(&mut r)?;
        let model: u16 = Decode::decode(&mut r)?;
        let type_byte: u8 = Decode::decode(&mut r)?;
        let color: u16 = Decode::decode(&mut r)?;
        let font: u16 = Decode::decode(&mut r)?;

        // Read the raw 30-byte name buffer.
        let mut name_buf = [0u8; 30];
        r.read_bytes(&mut name_buf)?;

        let (name, message) = if r.remaining_len() > 0 {
            // Standard layout: name is null-terminated inside the 30-byte
            // buffer; message follows as a separate null-terminated string.
            let name_end = name_buf.iter().position(|&b| b == 0).unwrap_or(30);
            let name = String::from_utf8_lossy(&name_buf[..name_end]).into_owned();

            // Read remaining bytes as message (lossy — some shards send
            // non-UTF-8 data here).
            let mut msg_bytes = Vec::new();
            loop {
                match r.read_u8() {
                    Ok(0) | Err(_) => break,
                    Ok(b) => msg_bytes.push(b),
                }
            }
            let message = String::from_utf8_lossy(&msg_bytes).into_owned();
            (name, message)
        } else {
            // Non-standard layout: message is packed inside the name buffer.
            // Split at the first \0: before = name, after (up to next \0) = message.
            let first_null = name_buf.iter().position(|&b| b == 0).unwrap_or(30);
            let name = String::from_utf8_lossy(&name_buf[..first_null]).into_owned();
            let rest = &name_buf[first_null + 1..];
            let msg_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            let message = String::from_utf8_lossy(&rest[..msg_end]).into_owned();
            (name, message)
        };

        Ok(Self {
            serial,
            model,
            speech_type: SpeechType::from_wire(type_byte),
            color,
            font,
            name,
            message,
        })
    }
}

impl Encode<BE> for SendSpeech {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(0x1C); // ID
        w.put_u16(0); // length placeholder
        w.put_u32(self.serial);
        w.put_u16(self.model);
        w.put_u8(self.speech_type.to_wire());
        w.put_u16(self.color);
        w.put_u16(self.font);

        // name: fixed 30 bytes, null-padded
        let nb = self.name.as_bytes();
        let nlen = nb.len().min(30);
        w.put_slice(&nb[..nlen]);
        w.put_bytes(0, 30 - nlen);

        // message: null-terminated
        w.put_slice(self.message.as_bytes());
        w.put_u8(0);
    }
}

// ── 0xAE UnicodeSpeech (dynamic, S→C) ─────────────────────────────────────

/// Packet 0xAE — Unicode Speech Message (dynamic, S→C)
///
/// Like [`SendSpeech`] (0x1C) but carries a UTF-16 BE message and a
/// 4-byte language code (e.g. `"ENU\0"`).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xAE, size = dynamic, endian = "be")]
pub struct UnicodeSpeech {
    pub id: u8,
    pub len: u16,
    pub serial: u32,
    pub model: u16,
    pub speech_type: SpeechType,
    pub color: u16,
    pub font: u16,
    pub language: FixedString<4>,
    pub name: FixedString<30>,
    pub message: NullUnicodeString,
}

// ── 0xAD SpeechRequest (dynamic, C→S) ─────────────────────────────────────

/// Packet 0xAD — Unicode/ASCII Speech Request (dynamic, C→S)
///
/// Sent by the client when the player types text. Two sub-formats exist
/// based on `type & 0xC0`:
///
/// - **Plain** (`type & 0xC0 == 0`): message is null-terminated UTF-16 BE.
/// - **WithKeywords** (`type & 0xC0 != 0`): contains packed 12-bit keyword
///   indices from `speech.mul`, followed by a null-terminated ASCII message.
///   Clients >= 2.0.7 use this format for server-side speech trigger matching.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpeechRequest {
    /// Plain unicode message (pre-2.0.7 or when no keywords match).
    Plain {
        speech_type: SpeechType,
        color: u16,
        font: u16,
        language: FixedString<4>,
        message: NullUnicodeString,
    },
    /// Message with packed keyword indices (clients >= 2.0.7).
    WithKeywords {
        /// Raw type byte (high bits 0xC0 set).
        speech_type: SpeechType,
        color: u16,
        font: u16,
        language: FixedString<4>,
        /// 12-bit keyword indices from speech.mul.
        keywords: Vec<u16>,
        /// ASCII text message.
        message: NullString,
    },
}

impl ManualPacket for SpeechRequest {
    const ID: u8 = 0xAD;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 12, true)?;
        let type_byte: u8 = Decode::decode(&mut r)?;
        let color: u16 = Decode::decode(&mut r)?;
        let font: u16 = Decode::decode(&mut r)?;
        let language: FixedString<4> = Decode::decode(&mut r)?;

        let speech_type = SpeechType::from_wire(type_byte);

        if type_byte & 0xC0 != 0 {
            // Keyword mode: packed 12-bit values.
            let b0: u8 = Decode::decode(&mut r)?;
            let b1: u8 = Decode::decode(&mut r)?;
            let num_keywords = ((b0 as u16) << 4) | ((b1 as u16) >> 4);

            let mut bit_buf: u32 = (b1 as u32) & 0x0F;
            let mut bits_in_buf: u32 = 4;

            let mut keywords = Vec::with_capacity(num_keywords as usize);
            for _ in 0..num_keywords {
                // Ensure at least 12 bits available
                while bits_in_buf < 12 {
                    let next: u8 = Decode::decode(&mut r)?;
                    bit_buf = (bit_buf << 8) | (next as u32);
                    bits_in_buf += 8;
                }
                bits_in_buf -= 12;
                let kw = ((bit_buf >> bits_in_buf) & 0xFFF) as u16;
                keywords.push(kw);
            }

            let message: NullString = Decode::decode(&mut r)?;

            Ok(Self::WithKeywords {
                speech_type,
                color,
                font,
                language,
                keywords,
                message,
            })
        } else {
            // Plain unicode message
            let message: NullUnicodeString = Decode::decode(&mut r)?;

            Ok(Self::Plain {
                speech_type,
                color,
                font,
                language,
                message,
            })
        }
    }
}

impl Encode<BE> for SpeechRequest {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        match self {
            Self::Plain {
                speech_type,
                color,
                font,
                language,
                message,
            } => {
                w.put_u8(speech_type.to_wire());
                w.put_u16(*color);
                w.put_u16(*font);
                language.encode(w);
                message.encode(w);
            }
            Self::WithKeywords {
                speech_type,
                color,
                font,
                language,
                keywords,
                message,
            } => {
                // Ensure the high bits are set on the type byte
                w.put_u8(speech_type.to_wire() | 0xC0);
                w.put_u16(*color);
                w.put_u16(*font);
                language.encode(w);

                // Pack keyword count + keywords as 12-bit values.
                // First 12 bits = count, then each keyword = 12 bits.
                let num = keywords.len() as u16;
                let mut values: Vec<u16> = Vec::with_capacity(1 + keywords.len());
                values.push(num);
                values.extend_from_slice(keywords);

                // Pack all 12-bit values into bytes
                let total_bits = values.len() * 12;
                let total_bytes = (total_bits + 7) / 8;
                let mut buf = vec![0u8; total_bytes];

                let mut bit_pos: usize = 0;
                for &val in &values {
                    // Write 12 bits of val starting at bit_pos
                    let v = val & 0xFFF;
                    for i in 0..12 {
                        let bit = (v >> (11 - i)) & 1;
                        let byte_idx = (bit_pos + i) / 8;
                        let bit_idx = 7 - ((bit_pos + i) % 8);
                        buf[byte_idx] |= (bit as u8) << bit_idx;
                    }
                    bit_pos += 12;
                }

                w.put_slice(&buf);
                message.encode(w);
            }
        }
    }
}

// ── 0xC1 ClilocMessage (dynamic, S→C) ─────────────────────────────────────

/// Packet 0xC1 — Cliloc Message (dynamic, S→C)
///
/// Sends a localized message from the server using a cliloc number.
/// Arguments are tab-separated (`'\t'`) and encoded as **little-endian**
/// UTF-16, unlike the rest of the packet which is big-endian.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClilocMessage {
    pub serial: u32,
    pub body: u16,
    pub speech_type: SpeechType,
    pub hue: u16,
    pub font: u16,
    pub message_number: u32,
    pub name: FixedString<30>,
    /// Tab-separated argument string (little-endian UTF-16 on the wire).
    pub arguments: String,
}

impl ManualPacket for ClilocMessage {
    const ID: u8 = 0xC1;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 48, true)?;
        let serial: u32 = Decode::decode(&mut r)?;
        let body: u16 = Decode::decode(&mut r)?;
        let type_byte: u8 = Decode::decode(&mut r)?;
        let hue: u16 = Decode::decode(&mut r)?;
        let font: u16 = Decode::decode(&mut r)?;
        let message_number: u32 = Decode::decode(&mut r)?;
        let name: FixedString<30> = Decode::decode(&mut r)?;

        // Arguments are little-endian UTF-16, null-terminated.
        let arguments = decode_le_utf16_str(&mut r)?;

        Ok(Self {
            serial,
            body,
            speech_type: SpeechType::from_wire(type_byte),
            hue,
            font,
            message_number,
            name,
            arguments,
        })
    }
}

impl Encode<BE> for ClilocMessage {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder
        w.put_u32(self.serial);
        w.put_u16(self.body);
        w.put_u8(self.speech_type.to_wire());
        w.put_u16(self.hue);
        w.put_u16(self.font);
        w.put_u32(self.message_number);
        self.name.encode(w);

        // Arguments: little-endian UTF-16, null-terminated.
        encode_le_utf16_str(&self.arguments, w);
    }
}

// ── 0x03 TalkRequest (dynamic, C→S) ───────────────────────────────────────

/// Packet 0x03 — Talk Request (dynamic, C→S)
///
/// Sent by the client when the player types a plain ASCII message.
/// For Unicode messages or keyword-indexed speech, the client uses
/// [`SpeechRequest`] (0xAD) instead.
///
/// The message is decoded lossy from the raw bytes (some shards or
/// third-party clients send non-UTF-8 data in this field).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TalkRequest {
    pub speech_type: SpeechType,
    pub color: u16,
    pub font: u16,
    /// Message text. Decoded lossy — invalid bytes are replaced with U+FFFD.
    pub message: String,
}

impl ManualPacket for TalkRequest {
    const ID: u8 = 0x03;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1)+len(2)+type(1)+color(2)+font(2)+null(1) = 9
        let mut r = packet_reader(data, 0x03, 9, true)?;

        let type_byte: u8 = Decode::decode(&mut r)?;
        let color: u16 = Decode::decode(&mut r)?;
        let font: u16 = Decode::decode(&mut r)?;

        // Read bytes until null terminator, decode lossy.
        let mut msg_bytes = Vec::new();
        loop {
            match r.read_u8() {
                Ok(0) | Err(_) => break,
                Ok(b) => msg_bytes.push(b),
            }
        }
        let message = String::from_utf8_lossy(&msg_bytes).into_owned();

        Ok(Self {
            speech_type: SpeechType::from_wire(type_byte),
            color,
            font,
            message,
        })
    }
}

impl Encode<BE> for TalkRequest {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(0x03); // ID
        w.put_u16(0); // length placeholder
        w.put_u8(self.speech_type.to_wire());
        w.put_u16(self.color);
        w.put_u16(self.font);
        w.put_slice(self.message.as_bytes());
        w.put_u8(0);
    }
}

impl TalkRequest {
    /// Create a new talk request.
    pub fn new(speech_type: SpeechType, color: u16, font: u16, message: impl Into<String>) -> Self {
        Self {
            speech_type,
            color,
            font,
            message: message.into(),
        }
    }
}


// ── Utility functions ─────────────────────────────────────────────────────

/// Extract the message text from a C→S speech packet (`0x03` or `0xAD`).
///
/// Returns `None` if the packet is not a speech packet or cannot be parsed.
pub fn extract_speech_text(packet: &u_core::RawPacket) -> Option<String> {
    match packet.id() {
        id if id == <TalkRequest as ManualPacket>::ID =>
            TalkRequest::from_bytes(&packet.data)
                .ok()
                .map(|r| r.message),
        id if id == <SpeechRequest as ManualPacket>::ID =>
            SpeechRequest::from_bytes(&packet.data)
                .ok()
                .map(|r| match r {
                    SpeechRequest::Plain { message, .. } => message.0,
                    SpeechRequest::WithKeywords { message, .. } => message.0,
                }),
        _ => None,
    }
}

/// Build a system-message packet (lower-left corner text).
///
/// Constructs a `SendSpeech` (0x1C) packet with the "system" speech type,
/// which is displayed in the lower-left corner of the game client.
/// Uses serial `0xFFFFFFFF`, model `0xFFFF`, color `0x03B2`, font 3.
pub fn system_message_packet(text: &str) -> u_core::RawPacket {
    let msg = SendSpeech {
        serial: 0xFFFF_FFFF,
        model: 0xFFFF,
        speech_type: SpeechType::System,
        color: 0x03B2,
        font: 3,
        name: String::new(),
        message: text.to_string(),
    };
    u_core::RawPacket::s2c(msg.to_bytes())
}
