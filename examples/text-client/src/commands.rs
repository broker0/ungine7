//! Text command parser for the input bar.
//!
//! Commands start with `.`; anything else is sent as normal speech.

use packets::interaction::{DoubleClick, RequestAttack, SingleClick};
use packets::speech::{SpeechType, TalkRequest};
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

/// Result of parsing a user command.
pub enum Command {
    /// Send a speech packet.
    Say { text: String, speech_type: SpeechType },
    /// SingleClick on a serial.
    Click { serial: u32 },
    /// DoubleClick on a serial.
    DoubleClick { serial: u32 },
    /// Attack a serial.
    Attack { serial: u32 },
    /// Request status for a serial.
    Status { serial: u32 },
    /// Print current position to chat.
    Pos,
    /// List nearby mobiles.
    Who,
    /// Quit the client.
    Quit,
    /// Show help.
    Help,
    /// Unknown command.
    Unknown(String),
}

/// Parse a line of user input into a [`Command`].
pub fn parse(line: &str) -> Command {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Command::Unknown(String::new());
    }

    // Commands start with `.`
    if let Some(rest) = trimmed.strip_prefix('.') {
        let mut parts = rest.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().unwrap_or("").trim();

        match cmd.as_str() {
            "say" | "s" => Command::Say {
                text: arg.to_string(),
                speech_type: SpeechType::Normal,
            },
            "yell" | "y" => Command::Say {
                text: arg.to_string(),
                speech_type: SpeechType::Yell,
            },
            "whisper" | "w" => Command::Say {
                text: arg.to_string(),
                speech_type: SpeechType::Whisper,
            },
            "emote" | "em" => Command::Say {
                text: arg.to_string(),
                speech_type: SpeechType::Emote,
            },
            "click" | "c" => match parse_serial(arg) {
                Some(s) => Command::Click { serial: s },
                None => Command::Unknown(format!(".click: invalid serial '{arg}'")),
            },
            "dclick" | "dc" | "use" => match parse_serial(arg) {
                Some(s) => Command::DoubleClick { serial: s },
                None => Command::Unknown(format!(".dclick: invalid serial '{arg}'")),
            },
            "attack" | "atk" => match parse_serial(arg) {
                Some(s) => Command::Attack { serial: s },
                None => Command::Unknown(format!(".attack: invalid serial '{arg}'")),
            },
            "status" | "st" => match parse_serial(arg) {
                Some(s) => Command::Status { serial: s },
                None => Command::Unknown(format!(".status: invalid serial '{arg}'")),
            },
            "pos" | "position" => Command::Pos,
            "who" | "nearby" => Command::Who,
            "quit" | "exit" | "q" => Command::Quit,
            "help" | "h" | "?" => Command::Help,
            _ => Command::Unknown(format!("unknown command: .{cmd}")),
        }
    } else {
        // No prefix → normal speech.
        Command::Say {
            text: trimmed.to_string(),
            speech_type: SpeechType::Normal,
        }
    }
}

/// Build a C→S packet for a command that produces one.
pub fn build_packet(cmd: &Command) -> Option<RawPacket> {
    match cmd {
        Command::Say { text, speech_type } => {
            let talk = TalkRequest::new(*speech_type, 0x003B, 3, text.as_str());
            Some(RawPacket::c2s(talk.to_bytes()))
        }
        Command::Click { serial } => {
            let pkt = SingleClick { id: SingleClick::ID, serial: *serial };
            Some(RawPacket::c2s(pkt.to_bytes()))
        }
        Command::DoubleClick { serial } => {
            let pkt = DoubleClick { id: DoubleClick::ID, serial: *serial };
            Some(RawPacket::c2s(pkt.to_bytes()))
        }
        Command::Attack { serial } => {
            let pkt = RequestAttack::new(*serial);
            Some(RawPacket::c2s(pkt.to_bytes()))
        }
        Command::Status { serial } => {
            // 0x34 GetMobileStatus: 10 bytes.
            let mut buf = vec![0x34u8, 0x00, 0x0A, 0xED, 0xED, 0xED, 0xED, 0x04];
            // serial at offset 3..7
            let bytes = serial.to_be_bytes();
            buf[3] = bytes[0];
            buf[4] = bytes[1];
            buf[5] = bytes[2];
            buf[6] = bytes[3];
            // type: 4 = full status
            buf.push(0x00);
            buf.push(0x00);
            // Pad to 10 bytes.
            while buf.len() < 10 {
                buf.push(0x00);
            }
            Some(RawPacket::c2s(buf.into()))
        }
        _ => None,
    }
}

/// Parse a serial from a string.  Supports `0x` prefix and plain decimal.
fn parse_serial(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}
