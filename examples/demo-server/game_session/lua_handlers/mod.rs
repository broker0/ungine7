//! Per-session Lua handler infrastructure.
//!
//! Manages a per-session Lua VM that handles game logic (combat, magic,
//! skills, regen, bandaging, mounting, interaction).  Rust forwards
//! game-relevant packets and world events to the Lua VM, and executes
//! actions (send packets, engine RPCs) that Lua requests.

mod api;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use log::{debug, error, info, warn};
use mlua::prelude::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use protocol::RawPacket;

use network::session::Session;

use framework::continuum::WorldEvent;

use crate::DemoWorkerTx;
use super::PlayerState;
use super::parsed_packet::ParsedPacket;

// ── Session event (Lua-facing) ────────────────────────────────────────────

/// Events delivered to the Lua VM: client packets + world events.
///
/// Typed variants carry pre-parsed fields so Lua never needs to parse
/// raw bytes.  The legacy `Packet` variant is kept for backward
/// compatibility during migration.
pub(crate) enum SessionLuaEvent {
    // ── Typed packet events (preferred) ──────────────────────────────

    /// Player wants to cast a spell (from TextCommand 0x12).
    CastSpell { spell_id: u16 },
    /// Player used a skill (from TextCommand 0x12).
    UseSkill { skill_id: u16 },
    /// Player responded to a target cursor (0x6C).
    TargetCursor {
        cursor_type: u8,
        cursor_id: u32,
        target_serial: u32,
        target_x: u16,
        target_y: u16,
        target_z: i8,
        target_graphic: u16,
    },
    /// Player double-clicked an object (0x06).
    DoubleClick { serial: u32, paperdoll: bool },
    /// War mode toggle (0x72).
    WarMode { fighting: bool },
    /// Attack request (0x05).
    AttackRequest { target: u32 },
    /// Cast spell with embedded target (0xBF:0x002D).
    CastTargetedSpell { spell_id: u16, target: u32 },
    /// Emote action (from TextCommand 0x12).
    Emote { action: String },

    // ── Legacy ───────────────────────────────────────────────────────

    /// A client packet forwarded for Lua handling (legacy, raw bytes).
    #[allow(dead_code)]
    Packet {
        /// UO packet ID (first byte).
        id: u8,
        /// Raw packet data (full, including ID and length).
        data: Vec<u8>,
    },

    // ── World events ─────────────────────────────────────────────────

    /// A world event relevant to this session.
    WorldEvent(Arc<WorldEvent>),
}

// ── Lua session action ────────────────────────────────────────────────────

/// Actions that the Lua VM can request the session to perform.
///
/// Each variant maps to a specific S→C packet or engine broadcast.
/// No raw byte buffers — all packets are assembled by Rust.
pub(crate) enum LuaSessionAction {
    // ── Client packets (S→C) ─────────────────────────────────────────

    /// Echo war-mode state back to the client (0x72).
    SendWarMode { fighting: bool },
    /// Confirm or cancel attack target (0xAA).
    SendAttackResponse { target_serial: u32 },
    /// Notify that a melee swing landed (0x2F).
    SendFightOccurring { attacker: u32, defender: u32 },
    /// Show a target cursor to the player (0x6C).
    SendTargetCursor { cursor_id: u32, cursor_type: u8 },
    /// Cancel an outstanding target cursor (0x6C with cancel flag).
    SendCancelTarget { cursor_id: u32 },
    /// System message in the bottom-left corner (red, 0x1C System).
    SendSystemMessage { message: String },
    /// Overhead speech bubble on an entity (0x1C Normal).
    SendOverheadMessage { serial: u32, message: String, color: u16 },
    /// Fizzle effect: system text + sound + visual (3 packets).
    SendFizzle { serial: u32, x: u16, y: u16, z: i8, message: String },
    /// Unicode speech with full control (0xAE) — for heal feedback etc.
    SendUnicodeSpeech {
        serial: u32, graphic: u16,
        color: u16, font: u16,
        name: String, message: String,
    },
    /// Equip an item on a mobile (0x2E) — for mount visual etc.
    SendEquipItem {
        item_serial: u32,
        graphic: u16,
        layer: u8,
        mobile_serial: u32,
        color: u16,
    },
    /// Delete an object from the client's view (0x1D) — for dismount visual etc.
    SendDeleteObject {
        serial: u32,
    },

    // ── Broadcasts (via worker → all observers) ──────────────────────

    /// Broadcast a sound effect.
    BroadcastSound { sound_id: u16, x: u16, y: u16, z: i16 },
    /// Broadcast a graphical effect.
    BroadcastEffect {
        direction_type: u8,
        source_serial: u32,
        target_serial: u32,
        graphic: u16,
        x: u16, y: u16, z: i8,
        target_x: u16, target_y: u16, target_z: i8,
        speed: u8, duration: u8,
        fixed_direction: bool, explode: bool,
    },
    /// Broadcast an animation.
    BroadcastAnimation {
        serial: u32, action: u16, frame_count: u8,
        repeat_count: u16, reverse: bool, repeat: bool,
        frame_delay: u8, x: u16, y: u16,
    },
    /// Broadcast speech (overhead text visible to all nearby players).
    BroadcastSpeech {
        serial: u32, graphic: u16,
        speech_type: u8, color: u16, font: u16,
        name: String, message: String,
        x: u16, y: u16,
    },
}

// ── Session Lua Manager ───────────────────────────────────────────────────

/// Manages the per-session Lua VM lifecycle: spawn, communication, reload.
pub(crate) struct SessionLuaManager {
    worker_tx: DemoWorkerTx,
    /// Channel for receiving actions from the Lua task.
    action_rx: mpsc::Receiver<LuaSessionAction>,
    /// Channel for sending events to the Lua task.
    event_tx: Option<mpsc::Sender<SessionLuaEvent>>,
    /// Current Lua task handle + cancellation token.
    current_task: Option<(tokio::task::JoinHandle<()>, CancellationToken)>,
    /// Path to the currently loaded script (for reload).
    script_path: Option<PathBuf>,
    /// Shared flag: `true` when a blocking gump is open.
    /// Updated by the session loop; read by Lua via `session:has_blocking_gump()`.
    blocking_gump_flag: Arc<AtomicBool>,
}

impl SessionLuaManager {
    pub fn new(worker_tx: DemoWorkerTx) -> Self {
        // Dummy action channel — no Lua task yet.
        let (_tx, action_rx) = mpsc::channel(1);
        Self {
            worker_tx,
            action_rx,
            event_tx: None,
            current_task: None,
            script_path: None,
            blocking_gump_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start a Lua script for this session.
    pub fn start(&mut self, script_path: PathBuf, player_serial: u32, map_id: u8) {
        // Stop any running script first.
        self.stop_sync();

        let (event_tx, event_rx) = mpsc::channel::<SessionLuaEvent>(256);
        let (action_tx, action_rx) = mpsc::channel::<LuaSessionAction>(256);
        let cancel = CancellationToken::new();

        let worker_tx = self.worker_tx.clone();
        let cancel2 = cancel.clone();
        let path = script_path.clone();
        let blocking_flag = self.blocking_gump_flag.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_session_lua(
                &path, worker_tx, event_rx, action_tx,
                cancel2, player_serial, map_id, blocking_flag,
            ).await {
                error!("[session-lua] script error: {}", e);
            }
        });

        self.event_tx = Some(event_tx);
        self.action_rx = action_rx;
        self.current_task = Some((handle, cancel));
        self.script_path = Some(script_path);
    }

    /// Reload the current script (if any).
    pub fn reload(&mut self, player_serial: u32, map_id: u8) {
        if let Some(path) = self.script_path.clone() {
            info!("[session-lua] reloading {}", path.display());
            self.start(path, player_serial, map_id);
        }
    }

    /// Forward a client packet to the Lua VM (non-blocking).
    #[allow(dead_code)]
    pub fn forward_packet(&self, packet: &RawPacket) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(SessionLuaEvent::Packet {
                id: packet.id(),
                data: packet.data.to_vec(),
            });
        }
    }

    /// Forward a pre-parsed packet to the Lua VM as a typed event.
    ///
    /// Converts [`ParsedPacket`] into a [`SessionLuaEvent`] variant with
    /// pre-extracted fields.  Lua receives ready-to-use data instead of
    /// raw bytes.
    pub fn forward_parsed(&self, parsed: &ParsedPacket) {
        let event = match parsed {
            ParsedPacket::TextCommand(cmd) => {
                match cmd {
                    packets::action::TextCommand::CastSpell { spell } => {
                        match spell.0.trim().parse::<u16>() {
                            Ok(id) => SessionLuaEvent::CastSpell { spell_id: id },
                            Err(_) => return,
                        }
                    }
                    packets::action::TextCommand::UseSkill { skill } => {
                        let sid = skill.0.trim()
                            .split_whitespace()
                            .next()
                            .and_then(|s| s.parse::<u16>().ok());
                        match sid {
                            Some(id) => SessionLuaEvent::UseSkill { skill_id: id },
                            None => return,
                        }
                    }
                    packets::action::TextCommand::Action { action } => {
                        SessionLuaEvent::Emote { action: action.0.clone() }
                    }
                    _ => return,
                }
            }
            ParsedPacket::TargetCursor(tc) => {
                SessionLuaEvent::TargetCursor {
                    cursor_type: tc.cursor_type,
                    cursor_id: tc.cursor_id,
                    target_serial: tc.target_serial,
                    target_x: tc.x,
                    target_y: tc.y,
                    target_z: tc.z,
                    target_graphic: tc.graphic,
                }
            }
            ParsedPacket::DoubleClick { serial, paperdoll } => {
                debug!("[lua-fwd] DoubleClick serial={:#010X} paperdoll={}", serial, paperdoll);
                SessionLuaEvent::DoubleClick {
                    serial: *serial,
                    paperdoll: *paperdoll,
                }
            }
            ParsedPacket::WarMode { fighting } => {
                SessionLuaEvent::WarMode { fighting: *fighting }
            }
            ParsedPacket::AttackRequest { target } => {
                SessionLuaEvent::AttackRequest { target: *target }
            }
            ParsedPacket::CastTargetedSpell { spell_id, target } => {
                SessionLuaEvent::CastTargetedSpell {
                    spell_id: *spell_id,
                    target: *target,
                }
            }
            _ => return, // Not a game-relevant packet.
        };
        if let Some(tx) = &self.event_tx {
            let result = tx.try_send(event);
            if let Err(ref e) = result {
                warn!("[lua-fwd] try_send failed: {}", e);
            }
        } else {
            debug!("[lua-fwd] event_tx is None — Lua VM not started?");
        }
    }

    /// Forward a world event to the Lua VM (non-blocking), filtered for relevance.
    pub fn forward_event(&self, event: &Arc<WorldEvent>, _player_serial: u32) {
        if let Some(tx) = &self.event_tx {
            // Forward all events — Lua filters what it needs.
            let _ = tx.try_send(SessionLuaEvent::WorldEvent(event.clone()));
        }
    }

    /// Receive the next action from the Lua VM.
    pub async fn recv_action(&mut self) -> Option<LuaSessionAction> {
        self.action_rx.recv().await
    }

    /// Execute a Lua-requested action.
    pub async fn execute_action(
        &self,
        action: LuaSessionAction,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> network::error::Result<()> {
        use packets::traits::{encode_packet, BasicPacket};

        match action {
            // ── Client packets (S→C) ─────────────────────────────────

            LuaSessionAction::SendWarMode { fighting } => {
                let pkt = packets::system::WarMode::new(fighting);
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            LuaSessionAction::SendAttackResponse { target_serial } => {
                if target_serial == 0 {
                    let pkt = packets::interaction::AttackResponse::refused();
                    session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
                } else {
                    let pkt = packets::interaction::AttackResponse {
                        id: packets::interaction::AttackResponse::ID,
                        serial: target_serial,
                    };
                    session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
                }
            }

            LuaSessionAction::SendFightOccurring { attacker, defender } => {
                let pkt = packets::interaction::FightOccurring::new(attacker, defender);
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            LuaSessionAction::SendTargetCursor { cursor_id, cursor_type } => {
                let pkt = packets::interaction::TargetCursor {
                    id: 0x6C,
                    cursor_target: 0,
                    cursor_id,
                    cursor_type,
                    target_serial: 0,
                    x: 0,
                    y: 0,
                    _pad0: (),
                    z: 0,
                    graphic: 0,
                };
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            LuaSessionAction::SendCancelTarget { cursor_id } => {
                let pkt = packets::interaction::TargetCursor {
                    id: 0x6C,
                    cursor_target: 3, // cancel
                    cursor_id,
                    cursor_type: 0,
                    target_serial: 0,
                    x: 0,
                    y: 0,
                    _pad0: (),
                    z: 0,
                    graphic: 0,
                };
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            LuaSessionAction::SendSystemMessage { message } => {
                session.send(crate::game_util::system_message(&message)).await?;
            }

            LuaSessionAction::SendOverheadMessage { serial, message, color } => {
                // Resolve the speaker's graphic so the bubble anchors to the
                // right entity; fall back to a neutral model if unknown.
                let model = {
                    use common::uo_engine::rpc::EngineProxy;
                    let engine = EngineProxy::<crate::DemoCommand>::new(worker_tx.clone(), 0);
                    engine
                        .get_entity(serial)
                        .await
                        .and_then(|e| e.mobile().map(|m| m.graphic))
                        .unwrap_or(0x0190)
                };
                session
                    .send(crate::game_util::overhead_speech(serial, model, &message, color))
                    .await?;
            }

            LuaSessionAction::SendFizzle { serial, x, y, z, message } => {
                let pkts = crate::game_util::fizzle_packets(serial, x, y, z, &message);
                for pkt in pkts {
                    session.send(pkt).await?;
                }
            }

            LuaSessionAction::SendUnicodeSpeech {
                serial, graphic, color, font, name, message,
            } => {
                use packets::speech::{UnicodeSpeech, SpeechType};
                use packets::u_io::{FixedString, NullUnicodeString};
                let pkt = UnicodeSpeech {
                    id: UnicodeSpeech::ID,
                    len: 0, // filled by encode
                    serial,
                    model: graphic,
                    speech_type: SpeechType::Normal,
                    color,
                    font,
                    language: FixedString("ENU".to_string()),
                    name: FixedString(name),
                    message: NullUnicodeString(message),
                };
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            LuaSessionAction::SendEquipItem {
                item_serial, graphic, layer, mobile_serial, color,
            } => {
                let pkt = packets::interaction::EquipItem {
                    id: packets::interaction::EquipItem::ID,
                    item_serial,
                    graphic,
                    _pad0: (),
                    layer: packets::layer::Layer::from_wire(layer),
                    player_serial: mobile_serial,
                    color,
                };
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            LuaSessionAction::SendDeleteObject { serial } => {
                let pkt = packets::interaction::DeleteObject {
                    id: packets::interaction::DeleteObject::ID,
                    serial,
                };
                session.send(RawPacket::s2c(encode_packet(&pkt))).await?;
            }

            // ── Broadcasts ───────────────────────────────────────────

            LuaSessionAction::BroadcastSound { sound_id, x, y, z } => {
                crate::game_util::send_sound(worker_tx, 0, sound_id, x, y, z).await;
            }

            LuaSessionAction::BroadcastEffect {
                direction_type, source_serial, target_serial, graphic,
                x, y, z, target_x, target_y, target_z,
                speed, duration, fixed_direction, explode,
            } => {
                crate::game_util::send_effect(
                    worker_tx, 0,
                    direction_type, source_serial, target_serial, graphic,
                    x, y, z, target_x, target_y, target_z,
                    speed, duration, fixed_direction, explode,
                ).await;
            }

            LuaSessionAction::BroadcastAnimation {
                serial, action, frame_count, repeat_count, reverse, repeat, frame_delay, x, y,
            } => {
                let (bx, by) = if x == 0 && y == 0 {
                    resolve_entity_pos(worker_tx, serial).await
                } else {
                    (x, y)
                };
                crate::game_util::send_animation(
                    worker_tx, 0, serial, action, frame_count, repeat_count,
                    reverse, repeat, frame_delay, bx, by,
                ).await;
            }

            LuaSessionAction::BroadcastSpeech {
                serial, graphic, speech_type, color, font,
                name, message, x, y,
            } => {
                let (bx, by) = if x == 0 && y == 0 {
                    resolve_entity_pos(worker_tx, serial).await
                } else {
                    (x, y)
                };
                let _ = worker_tx.send(framework::continuum::WorkerCommand::MapCommand(
                    0,
                    crate::DemoCommand::BroadcastSpeech {
                        serial, graphic, speech_type, color, font,
                        name, message, x: bx, y: by,
                    },
                )).await;
            }
        }
        Ok(())
    }

    /// Stop the Lua VM (cleanup on disconnect).
    pub async fn stop(&mut self) {
        if let Some((handle, cancel)) = self.current_task.take() {
            cancel.cancel();
            let _ = handle.await;
        }
        self.event_tx = None;
    }

    /// Update the shared blocking-gump flag so Lua can query it via
    /// `session:has_blocking_gump()`.
    pub fn sync_blocking_gump(&self, is_blocking: bool) {
        self.blocking_gump_flag.store(is_blocking, Ordering::Relaxed);
    }

    /// Stop synchronously (cancel only, don't await).
    fn stop_sync(&mut self) {
        if let Some((handle, cancel)) = self.current_task.take() {
            cancel.cancel();
            handle.abort();
        }
        self.event_tx = None;
    }
}

impl Drop for SessionLuaManager {
    fn drop(&mut self) {
        self.stop_sync();
    }
}

// ── Broadcast position resolution ─────────────────────────────────────────

/// Resolve entity position for broadcast routing.
///
/// When Lua sends a broadcast action with `x=0, y=0` (unknown position),
/// we look up the entity to get its actual coordinates.  This ensures the
/// observer registry delivers the event to nearby players.
async fn resolve_entity_pos(worker_tx: &DemoWorkerTx, serial: u32) -> (u16, u16) {
    use common::uo_engine::rpc::EngineProxy;

    let engine = EngineProxy::<crate::DemoCommand>::new(worker_tx.clone(), 0);
    if let Some(entity) = engine.get_entity(serial).await {
        let (x, y, _) = entity.xyz();
        (x, y)
    } else {
        (0, 0)
    }
}

// ── Session Lua runtime ───────────────────────────────────────────────────

/// Run a Lua script as the game logic handler for a session.
///
/// The script receives events (client packets + world events) through
/// `event_rx` and sends actions (packets, engine commands) through
/// `action_tx`.
async fn run_session_lua(
    script_path: &std::path::Path,
    worker_tx: DemoWorkerTx,
    event_rx: mpsc::Receiver<SessionLuaEvent>,
    action_tx: mpsc::Sender<LuaSessionAction>,
    cancel: CancellationToken,
    player_serial: u32,
    map_id: u8,
    blocking_gump_flag: Arc<AtomicBool>,
) -> Result<(), LuaError> {
    let script_name = script_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());

    info!("[session-lua:{}] starting script (player={:#010X})", script_name, player_serial);

    let source = match std::fs::read_to_string(script_path) {
        Ok(s) => s,
        Err(e) => {
            error!("[session-lua:{}] failed to read script: {}", script_name, e);
            return Err(LuaError::external(e));
        }
    };

    let lua = Lua::new();

    // Register API.
    api::register_session_globals(
        &lua, &script_name,
        worker_tx, event_rx, action_tx,
        cancel.clone(), player_serial, map_id,
        blocking_gump_flag,
    )?;

    // Run the script.
    let chunk = lua.load(&source).set_name(&script_name);

    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            info!("[session-lua:{}] cancelled (reload)", script_name);
            return Ok(());
        }
        r = chunk.exec_async() => r,
    };

    match &result {
        Ok(()) => info!("[session-lua:{}] script finished", script_name),
        Err(e) => error!("[session-lua:{}] script error: {}", script_name, e),
    }

    result
}

// ── .slua dot-command handler ─────────────────────────────────────────────

/// Handle `.slua` dot-commands for per-session Lua script management.
///
/// Returns `true` if the packet was consumed (handled as a dot-command).
pub(crate) async fn handle_session_lua_command(
    packet: &RawPacket,
    player: &Option<PlayerState>,
    lua_mgr: &mut SessionLuaManager,
    session: &mut Session,
) -> network::error::Result<bool> {
    // Only process speech packets.
    if packet.id() != 0xAD && packet.id() != 0x03 {
        return Ok(false);
    }

    let text = match common::dot_commands::extract_speech_text(packet) {
        Some(t) => t,
        None => return Ok(false),
    };

    if !text.starts_with(".slua") {
        return Ok(false);
    }

    let args = text[5..].trim();
    let p = player.as_ref();

    if args.is_empty() || args == " help" {
        let msg = crate::game_util::system_message(
            "Usage: .slua <path> | .slua reload | .slua stop"
        );
        session.send(msg).await?;
        return Ok(true);
    }

    if args.trim() == "reload" {
        if let Some(p) = p {
            lua_mgr.reload(p.serial, p.world);
            session.send(crate::game_util::system_message("Session script reloaded.")).await?;
        }
        return Ok(true);
    }

    if args.trim() == "stop" {
        lua_mgr.stop().await;
        session.send(crate::game_util::system_message("Session script stopped.")).await?;
        return Ok(true);
    }

    // .slua <path>
    let path = PathBuf::from(args.trim());
    if let Some(p) = p {
        lua_mgr.start(path.clone(), p.serial, p.world);
        let msg = format!("Session script started: {}", path.display());
        session.send(crate::game_util::system_message(&msg)).await?;
    }

    Ok(true)
}
