//! Single-pass packet parsing: `ParsedPacket` enum + `parse_packet()`.
//!
//! Every client packet is parsed **once** into a [`ParsedPacket`] variant,
//! eliminating the duplicate parsing that previously occurred in both the
//! Rust infrastructure code and the Lua `parse_packet_fields()` bridge.
//!
//! The parser is a **pure function** — no session state, no async RPC.
//! Stateful routing (e.g. which handler should process a `TargetCursor`)
//! is the responsibility of the session loop and the `GameLogicHandler`.

use packets::action::TextCommand;
use packets::gump::GumpMenuSelection;
use packets::interaction::{
    DoubleClick, GetMobileStatus, MobileStatusRequest, RequestAttack,
    SingleClick, TargetCursor,
    BuyItems, SellListReply,
};
use packets::movement::MoveRequest;
use packets::skills::{SetSkillLock, SkillLock};
use packets::system::{ClientViewRange, Ping, WarMode};
use packets::traits::{ManualPacket, BasicPacket};

use protocol::RawPacket;

// ── ParsedPacket ──────────────────────────────────────────────────────────

/// Result of a single-pass packet parse.
///
/// Variants cover every packet type handled by the demo-server session.
/// Login packets (0x91, 0x5D) are left as opaque IDs — they are complex
/// multi-step flows that parse the raw data internally.
#[derive(Debug)]
#[allow(dead_code)] // Fields will be used when sub-handlers migrate from &RawPacket
pub(super) enum ParsedPacket {
    // ── Movement ─────────────────────────────────────────────────────
    MoveRequest {
        sequence: u8,
        direction: u8,
        running: bool,
    },

    // ── System ───────────────────────────────────────────────────────
    Ping(Ping),
    ClientViewRange {
        range: u8,
    },
    ResyncRequest,

    // ── Interaction ──────────────────────────────────────────────────
    SingleClick {
        serial: u32,
    },
    DoubleClick {
        serial: u32,
        paperdoll: bool,
    },
    GetStatus {
        serial: u32,
        request_type: MobileStatusRequest,
    },

    // ── Combat / game-logic ──────────────────────────────────────────
    WarMode {
        fighting: bool,
    },
    AttackRequest {
        target: u32,
    },

    // ── Skills ───────────────────────────────────────────────────────
    /// SetSkillLock (0x3A, C→S) — client changed a skill's lock state.
    SetSkillLock {
        skill_id: u16,
        lock: SkillLock,
    },

    // ── Commands ─────────────────────────────────────────────────────
    TextCommand(TextCommand),
    TargetCursor(TargetCursor),

    // ── GeneralInfo sub-commands (0xBF) ──────────────────────────────
    CastTargetedSpell {
        spell_id: u16,
        target: u32,
    },

    // ── Gump response ───────────────────────────────────────────────
    GumpMenuSelection {
        serial: u32,
        gump_id: u32,
        button_id: u32,
        switches: Vec<u32>,
    },

    // ── Items ────────────────────────────────────────────────────────
    /// Item pick-up / drop / wear — not parsed further here because
    /// the existing `items.rs` handlers do their own detailed parsing
    /// with validation.  We just tag the packet ID.
    ItemPacket {
        id: u8,
    },

    // ── Vendor ───────────────────────────────────────────────────────
    /// BuyItems (0x3B) — client purchase request.  Empty `items` means
    /// the buy window was cancelled.
    BuyItems {
        vendor_id: u32,
        items: Vec<(u32, u16)>,
    },
    /// SellListReply (0x9F) — client sell request.
    SellReply {
        shopkeeper_id: u32,
        items: Vec<(u32, u16)>,
    },

    // ── Login (opaque — parsed internally by spawn.rs) ───────────────
    GameLogin,
    LoginCharacter,
    /// CreateCharacter (0x00) — parsed internally by spawn.rs.
    CreateCharacter,

    /// Packet recognised but not handled by the session (silently ignored).
    Ignored(u8),
    /// Packet parsing failed.
    ParseError(u8),
}

// ── parse_packet ──────────────────────────────────────────────────────────

/// Parse a raw client packet into a [`ParsedPacket`] — pure function,
/// no session state, no async.
///
/// Call this exactly once per packet; pass the result to both the
/// infrastructure handler and the game-logic handler.
pub(super) fn parse_packet(packet: &RawPacket) -> ParsedPacket {
    match packet.id() {
        // ── CreateCharacter (0x00) ───────────────────────────────
        //
        // The connection seed (also opcode 0x00) is consumed by the
        // transport layer before the session loop runs, so any 0x00
        // packet seen here is a character-creation request.
        0x00 => ParsedPacket::CreateCharacter,

        // ── Movement (0x02) ──────────────────────────────────────
        0x02 => match MoveRequest::from_bytes(&packet.data) {
            Ok(req) => ParsedPacket::MoveRequest {
                sequence: req.sequence,
                direction: req.direction,
                running: req.is_running(),
            },
            Err(_) => ParsedPacket::ParseError(0x02),
        },

        // ── AttackRequest (0x05) ─────────────────────────────────
        0x05 => match RequestAttack::from_bytes(&packet.data) {
            Ok(ra) => ParsedPacket::AttackRequest {
                target: ra.target_id,
            },
            Err(_) => ParsedPacket::ParseError(0x05),
        },

        // ── DoubleClick (0x06) ───────────────────────────────────
        0x06 => match DoubleClick::from_bytes(&packet.data) {
            Ok(dc) => {
                let paperdoll = dc.serial & 0x8000_0000 != 0;
                let serial = dc.serial & 0x7FFF_FFFF;
                ParsedPacket::DoubleClick { serial, paperdoll }
            }
            Err(_) => ParsedPacket::ParseError(0x06),
        },

        // ── Item packets (0x07, 0x08, 0x13) ──────────────────────
        id @ (0x07 | 0x08 | 0x13) => ParsedPacket::ItemPacket { id },

        // ── SingleClick (0x09) ───────────────────────────────────
        0x09 => match SingleClick::from_bytes(&packet.data) {
            Ok(sc) => ParsedPacket::SingleClick { serial: sc.serial },
            Err(_) => ParsedPacket::ParseError(0x09),
        },

        // ── TextCommand (0x12) ───────────────────────────────────
        0x12 => match TextCommand::from_bytes(&packet.data) {
            Ok(cmd) => ParsedPacket::TextCommand(cmd),
            Err(_) => ParsedPacket::ParseError(0x12),
        },

        // ── ResyncRequest (0x22) ─────────────────────────────────
        0x22 => ParsedPacket::ResyncRequest,

        // ── SetSkillLock (0x3A, C→S) ─────────────────────────────
        0x3A => match SetSkillLock::from_bytes(&packet.data) {
            Ok(s) => ParsedPacket::SetSkillLock {
                skill_id: s.skill_id,
                lock: s.lock,
            },
            Err(_) => ParsedPacket::ParseError(0x3A),
        },

        // ── BuyItems (0x3B) ──────────────────────────────────────
        0x3B => match BuyItems::from_bytes(&packet.data) {
            Ok(b) => ParsedPacket::BuyItems {
                vendor_id: b.vendor_id,
                items: b.items.iter().map(|e| (e.item_id, e.quantity)).collect(),
            },
            Err(_) => ParsedPacket::ParseError(0x3B),
        },

        // ── GetMobileStatus (0x34) ───────────────────────────────
        0x34 => match GetMobileStatus::from_bytes(&packet.data) {
            Ok(req) => ParsedPacket::GetStatus {
                serial: req.serial,
                request_type: req.request_type,
            },
            Err(_) => ParsedPacket::ParseError(0x34),
        },

        // ── LoginCharacter (0x5D) ────────────────────────────────
        0x5D => ParsedPacket::LoginCharacter,

        // ── TargetCursor (0x6C) ──────────────────────────────────
        0x6C => match TargetCursor::from_bytes(&packet.data) {
            Ok(tc) => ParsedPacket::TargetCursor(tc),
            Err(_) => ParsedPacket::ParseError(0x6C),
        },

        // ── SellListReply (0x9F) ─────────────────────────────────
        0x9F => match SellListReply::from_bytes(&packet.data) {
            Ok(s) => ParsedPacket::SellReply {
                shopkeeper_id: s.shopkeeper_id,
                items: s.items.iter().map(|e| (e.item_id, e.quantity)).collect(),
            },
            Err(_) => ParsedPacket::ParseError(0x9F),
        },

        // ── WarMode (0x72) ───────────────────────────────────────
        0x72 => match WarMode::from_bytes(&packet.data) {
            Ok(wm) => ParsedPacket::WarMode {
                fighting: wm.is_fighting(),
            },
            Err(_) => ParsedPacket::ParseError(0x72),
        },

        // ── Ping (0x73) ──────────────────────────────────────────
        0x73 => match Ping::from_bytes(&packet.data) {
            Ok(ping) => ParsedPacket::Ping(ping),
            Err(_) => ParsedPacket::ParseError(0x73),
        },

        // ── GameLogin (0x91) ─────────────────────────────────────
        0x91 => ParsedPacket::GameLogin,

        // ── GumpMenuSelection (0xB1) ────────────────────────────
        0xB1 => match GumpMenuSelection::from_bytes(&packet.data) {
            Ok(pkt) => ParsedPacket::GumpMenuSelection {
                serial: pkt.serial,
                gump_id: pkt.gump_id,
                button_id: pkt.button_id,
                switches: pkt.switches,
            },
            Err(_) => ParsedPacket::ParseError(0xB1),
        },

        // ── GeneralInfo (0xBF) ───────────────────────────────────
        0xBF => parse_general_info(&packet.data),

        // ── ClientViewRange (0xC8) ───────────────────────────────
        0xC8 => match ClientViewRange::from_bytes(&packet.data) {
            Ok(cvr) => ParsedPacket::ClientViewRange { range: cvr.range },
            Err(_) => ParsedPacket::ParseError(0xC8),
        },

        // ── Unknown ──────────────────────────────────────────────
        other => ParsedPacket::Ignored(other),
    }
}

/// Parse GeneralInfo (0xBF) sub-commands.
fn parse_general_info(data: &[u8]) -> ParsedPacket {
    if data.len() < 5 {
        return ParsedPacket::ParseError(0xBF);
    }

    let subcmd = u16::from_be_bytes([data[3], data[4]]);

    match subcmd {
        // CastTargetedSpell (0x002D) — target embedded in the packet.
        0x002D if data.len() >= 11 => {
            let spell_id = u16::from_be_bytes([data[5], data[6]]);
            let target = u32::from_be_bytes([data[7], data[8], data[9], data[10]]);
            ParsedPacket::CastTargetedSpell { spell_id, target }
        }
        // Other sub-commands — ignored for now.
        _ => ParsedPacket::Ignored(0xBF),
    }
}
