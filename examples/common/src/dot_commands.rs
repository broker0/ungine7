//! Shared dot-command utilities.
//!
//! Provides the building blocks used by per-project `DotCommands` state
//! machines (e.g. in `replay-proxy` and `rpc-proxy`):
//!
//! - [`PacketSink`] — abstract S→C packet sender with impls for
//!   `network::session::Session` and `tokio::sync::mpsc::Sender`.
//! - Speech extraction ([`extract_speech_text`]).
//! - System-message helpers ([`system_message_packet`], [`send_system_message`]).
//! - Gump/target-cursor send helpers ([`send_gump`], [`send_target_cursor`],
//!   [`close_gump`]).
//!
//! Project-specific types (`Handled`, `PendingTarget`, `GumpKind`,
//! `DotCommands`) remain in their respective crates.

use async_trait::async_trait;
use log::info;
use tokio::sync::mpsc;

use network::error as fw_error;
use network::session::Session;
use packets::gump::{GumpTextLine, SendGumpDialog};
use packets::interaction::TargetCursor;
use packets::speech::{SendSpeech, SpeechRequest, SpeechType, TalkRequest};
use packets::system::GeneralInfo;
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

// ── Constants ─────────────────────────────────────────────────────────────

/// Base cursor ID for dot-command target requests.
pub const CMD_CURSOR_BASE: u32 = 0x0E91_D000;

/// Base gump ID for command gumps.
pub const CMD_GUMP_BASE: u32 = 0x0E91_A000;

// ── PacketSink ────────────────────────────────────────────────────────────

/// Abstraction over different ways to send S→C packets to a client.
///
/// Implemented for [`Session`] (propagates transport errors) and for
/// [`mpsc::Sender<RawPacket>`] (fire-and-forget, never returns an error).
#[async_trait]
pub trait PacketSink: Send {
    /// Send a server-to-client packet.
    async fn send_packet(&mut self, packet: RawPacket) -> fw_error::Result<()>;
}

#[async_trait]
impl PacketSink for Session {
    async fn send_packet(&mut self, packet: RawPacket) -> fw_error::Result<()> {
        self.send(packet).await?;
        Ok(())
    }
}

#[async_trait]
impl PacketSink for mpsc::Sender<RawPacket> {
    async fn send_packet(&mut self, packet: RawPacket) -> fw_error::Result<()> {
        // Fire-and-forget: if the receiver is dropped the message is
        // silently lost — consistent with existing rpc-proxy behaviour.
        let _ = self.send(packet).await;
        Ok(())
    }
}

// ── Speech extraction ─────────────────────────────────────────────────────

/// Extract the message text from a C→S speech packet (`0x03` or `0xAD`).
pub fn extract_speech_text(packet: &RawPacket) -> Option<String> {
    match packet.id() {
        id if id == TalkRequest::ID => TalkRequest::from_bytes(&packet.data)
            .ok()
            .map(|r| r.message),
        id if id == SpeechRequest::ID => SpeechRequest::from_bytes(&packet.data)
            .ok()
            .map(|r| match r {
                SpeechRequest::Plain { message, .. } => message.0,
                SpeechRequest::WithKeywords { message, .. } => message.0,
            }),
        _ => None,
    }
}

// ── System message helpers ────────────────────────────────────────────────

/// Build a system-message packet (lower-left corner text).
pub fn system_message_packet(text: &str) -> RawPacket {
    let msg = SendSpeech {
        serial: 0xFFFF_FFFF,
        model: 0xFFFF,
        speech_type: SpeechType::System,
        color: 0x03B2,
        font: 3,
        name: String::new(),
        message: text.to_string(),
    };
    RawPacket::s2c(msg.to_bytes())
}

/// Send a system message (lower-left corner) to the client.
pub async fn send_system_message(
    sink: &mut (dyn PacketSink + '_),
    text: &str,
) -> fw_error::Result<()> {
    sink.send_packet(system_message_packet(text)).await
}

// ── Gump helpers ──────────────────────────────────────────────────────────

/// Send a gump dialog to the client.
///
/// The gump is sent as `0xB0` (uncompressed) for maximum compatibility —
/// pre-AoS clients (< ~4.0) do not support the compressed `0xDD` format,
/// while all known clients accept `0xB0`.
///
/// Callers are responsible for tracking the gump in their own
/// `active_gumps` map; this function only handles serialisation and
/// sending.
pub async fn send_gump(
    gump_id: u32,
    serial: u32,
    commands: &str,
    text_lines: &[GumpTextLine],
    sink: &mut (dyn PacketSink + '_),
) -> fw_error::Result<()> {
    info!("[cmd] sending gump (id={:#010X})", gump_id);
    let dialog = SendGumpDialog {
        serial,
        gump_id,
        x: 0,
        y: 0,
        layout: commands.to_string(),
        text_lines: text_lines.to_vec(),
        trailing_pad: vec![],
    };
    sink.send_packet(RawPacket::s2c(dialog.to_bytes())).await
}

/// Send a `TargetCursor` request to the client.
///
/// - `cursor_id` — unique ID for matching the response.
/// - `cursor_target` — `0` = any object, `1` = ground/tile only.
///
/// Callers are responsible for storing `pending_target` state.
pub async fn send_target_cursor(
    cursor_id: u32,
    cursor_target: u8,
    sink: &mut (dyn PacketSink + '_),
) -> fw_error::Result<()> {
    let cursor = TargetCursor {
        id: TargetCursor::ID,
        cursor_target,
        cursor_id,
        cursor_type: 0,
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };
    sink.send_packet(RawPacket::s2c(cursor.to_bytes())).await
}

/// Close a `{ noclose }` gump on the client side.
///
/// Uses `GeneralInfo::CloseGump` (sub-command of `0xBF`).
pub async fn close_gump(
    gump_id: u32,
    sink: &mut (dyn PacketSink + '_),
) -> fw_error::Result<()> {
    info!("[cmd] closing gump (id={:#010X})", gump_id);
    let close = GeneralInfo::CloseGump {
        dialog_id: gump_id,
        button_id: 0,
    };
    sink.send_packet(RawPacket::s2c(close.to_bytes())).await
}

// ── Target cursor helpers ─────────────────────────────────────────────────

/// Check whether a `TargetCursor` response represents a cancellation.
///
/// The client sends `cursor_type == 3` for explicit cancel, or sentinel
/// coordinates (`0xFFFF`) when the cursor is dismissed.
pub fn is_target_cancelled(tc: &TargetCursor) -> bool {
    tc.cursor_type == 3 || tc.x == 0xFFFF || tc.y == 0xFFFF
}

// ── Snapshot command helpers ──────────────────────────────────────────────

/// Save a zone snapshot to a JSON file and send a result message.
///
/// The caller is responsible for requesting the `ZoneSaveData` from the
/// engine — this function handles the file I/O and user feedback.
pub async fn save_snapshot_to_file(
    zone_data: crate::uo_engine::snapshot::ZoneSaveData,
    player_serial: u32,
    player_world: u8,
    path: &str,
    sink: &mut (dyn PacketSink + '_),
) -> fw_error::Result<()> {
    let world_data = crate::uo_engine::snapshot::WorldSaveData {
        zones: vec![zone_data],
        player_serial,
        player_world,
    };
    match crate::uo_engine::snapshot::save_to_file(
        &world_data,
        std::path::Path::new(path),
    ) {
        Ok(()) => {
            send_system_message(sink, &format!("World saved to {}", path)).await?;
            info!("[cmd] .save — done");
        }
        Err(e) => {
            send_system_message(sink, &format!("Save failed: {}", e)).await?;
            log::error!("[cmd] .save — {}", e);
        }
    }
    Ok(())
}

/// Load a zone snapshot from a JSON file for a specific world.
///
/// On success, returns the `ZoneSaveData` plus (entity_count, container_count).
/// On error (file not found, bad format, wrong world), sends a message
/// to the sink and returns `Ok(None)`.
pub async fn load_snapshot_from_file(
    path: &str,
    world: u8,
    sink: &mut (dyn PacketSink + '_),
) -> fw_error::Result<Option<(crate::uo_engine::snapshot::ZoneSaveData, usize, usize)>> {
    match crate::uo_engine::snapshot::load_from_file(std::path::Path::new(path)) {
        Ok(world_data) => {
            let zone_data = world_data
                .zones
                .into_iter()
                .find(|z| z.map_id == world);

            if let Some(data) = zone_data {
                let entity_count = data.entities.len();
                let container_count = data.containers.len();
                Ok(Some((data, entity_count, container_count)))
            } else {
                send_system_message(
                    sink,
                    &format!("No zone for world {} in save file", world),
                ).await?;
                Ok(None)
            }
        }
        Err(e) => {
            send_system_message(sink, &format!("Load failed: {}", e)).await?;
            log::error!("[cmd] .load — {}", e);
            Ok(None)
        }
    }
}
