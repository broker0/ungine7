//! `ControllerGameLogicHandler` — implements `GameLogicHandler` for the
//! controller-session mode.
//!
//! This is a thin session-side handler that translates client packets into
//! [`GameCommand`]s and forwards them fire-and-forget to the player's
//! [`LuaController`](crate::lua_script::LuaController) (or any `EntityController`) running inside the worker.
//!
//! Game logic (combat, spells, regen) lives entirely in the controller.
//! The session only handles infrastructure (login, movement, items,
//! containers, entity streaming) and protocol translation.

use std::sync::Arc;

use log::debug;

use protocol::RawPacket;
use packets::traits::{BasicPacket, ManualPacket};

use network::error;
use network::session::Session;

use framework::continuum::{WorkerCommand, WorldEvent};
use framework::diorama::ObserverPipeline;

use common::uo_engine::controller::GameCommand;

use crate::{DemoCommand, DemoWorkerTx, WorldData};

use super::{
    parsed_packet::ParsedPacket,
    pending_cursor::CursorKind,
    game_logic::{GameLogicHandler, InfraState, TimerEvent},
};

// ── ControllerGameLogicHandler ───────────────────────────────────────────

/// Controller-session implementation of `GameLogicHandler`.
///
/// Owns only `InfraState` — all game-logic state lives in the controller
/// on the worker side.  This handler is intentionally minimal: it parses
/// game-relevant packets, converts them to `GameCommand`, and sends them
/// as `ControllerCommand` to the worker.
pub(super) struct ControllerGameLogicHandler {
    pub(super) infra: InfraState,
    /// Cloned worker channel for sending AttachController on spawn.
    worker_tx: DemoWorkerTx,
    /// Path to the Lua controller script to attach on player spawn.
    controller_script: Option<std::path::PathBuf>,
}

impl ControllerGameLogicHandler {
    pub fn new(
        worker_tx: DemoWorkerTx,
        observer: Option<ObserverPipeline>,
        controller_script: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            infra: InfraState::new(observer, &worker_tx, None, u_core::ProtocolVersion::SA_CLIENT),
            worker_tx,
            controller_script,
        }
    }

    /// Send a command to the player's controller (fire-and-forget).
    async fn send_cmd(
        worker_tx: &DemoWorkerTx,
        world: u8,
        serial: u32,
        cmd: GameCommand,
    ) {
        let _ = worker_tx.send(WorkerCommand::MapCommand(
            world,
            DemoCommand::ControllerCommand(serial, cmd),
        )).await;
    }
}

#[async_trait::async_trait]
impl GameLogicHandler for ControllerGameLogicHandler {
    async fn handle_packet(
        &mut self,
        parsed: &ParsedPacket,
        raw: &RawPacket,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<bool> {
        let Some(p) = &self.infra.player else {
            return Ok(false);
        };
        let serial = p.serial;
        let world = p.world;

        match parsed {
            // ── AttackRequest (0x05) ─────────────────────────────────
            ParsedPacket::AttackRequest { target } => {
                if *target == 0 {
                    Self::send_cmd(worker_tx, world, serial,
                                   GameCommand::CancelAttack).await;
                } else {
                    Self::send_cmd(worker_tx, world, serial,
                                   GameCommand::Attack { target_serial: *target }).await;
                }
                // Echo attack acknowledgement immediately (client expects sync).
                session.send(crate::combat::attack_response(*target)).await?;
                Ok(true)
            }

            // ── WarMode (0x72) ───────────────────────────────────────
            ParsedPacket::WarMode { fighting } => {
                Self::send_cmd(worker_tx, world, serial,
                               GameCommand::ToggleWarMode { fighting: *fighting }).await;
                // Echo war mode response immediately (client expects sync).
                session.send(crate::combat::war_mode_response(*fighting)).await?;
                Ok(true)
            }

            // ── TextCommand (0x12) — spells, skills ──────────────────
            ParsedPacket::TextCommand(_) => {
                if let Ok(cmd) = packets::action::TextCommand::from_bytes(&raw.data) {
                    match cmd {
                        packets::action::TextCommand::CastSpell { spell } => {
                            if let Ok(spell_id) = spell.0.trim().parse::<u16>() {
                                Self::send_cmd(worker_tx, world, serial,
                                               GameCommand::CastSpell { spell_id, target_serial: 0 }).await;
                            }
                        }
                        packets::action::TextCommand::UseSkill { skill } => {
                            if let Some(sid) = skill.0.trim().split_whitespace().next()
                                .and_then(|s| s.parse::<u16>().ok())
                            {
                                Self::send_cmd(worker_tx, world, serial,
                                               GameCommand::UseSkill { skill_id: sid }).await;
                            }
                        }
                        _ => {} // Action, OpenDoor, etc. — ignored for now
                    }
                }
                Ok(true)
            }

            // ── CastTargetedSpell (0xBF:0x002D) ─────────────────────
            ParsedPacket::CastTargetedSpell { .. } => {
                if raw.data.len() >= 11 {
                    let spell_id = u16::from_be_bytes([raw.data[5], raw.data[6]]);
                    let target = u32::from_be_bytes([raw.data[7], raw.data[8], raw.data[9], raw.data[10]]);
                    Self::send_cmd(worker_tx, world, serial,
                                   GameCommand::CastSpell { spell_id, target_serial: target }).await;
                }
                Ok(true)
            }

            // ── TargetCursor (0x6C) ──────────────────────────────────
            ParsedPacket::TargetCursor(_) => {
                log::info!(
                    "[ctrl] TargetCursor received, pending_cursor={:?}",
                    self.infra.pending_cursor.as_ref().map(|pc| (pc.cursor_id, &pc.kind)),
                );
                if let Some(pending) = self.infra.pending_cursor.take() {
                    if let Ok(tc) = packets::interaction::TargetCursor::from_bytes(&raw.data) {
                        log::info!(
                            "[ctrl] parsed: cursor_id=0x{:08X} (pending=0x{:08X}), target=0x{:08X}, cursor_type={}",
                            tc.cursor_id, pending.cursor_id, tc.target_serial, tc.cursor_type,
                        );
                        if tc.cursor_id == pending.cursor_id {
                            // DotCommand cursors are handled before us in session_loop.
                            if matches!(pending.kind, CursorKind::DotCommand(_)) {
                                return Ok(true);
                            }
                            // Controller cursors: always forward (including cancel)
                            // so the Lua script can clear pending state.
                            // Other kinds: skip cancel (handled session-side).
                            let cancelled = tc.cursor_type == 3 || tc.target_serial == 0;
                            if matches!(pending.kind, CursorKind::Controller) || !cancelled {
                                Self::send_cmd(worker_tx, world, serial,
                                               GameCommand::TargetResponse {
                                        cursor_id: tc.cursor_id,
                                        target_serial: tc.target_serial,
                                        x: tc.x,
                                        y: tc.y,
                                        z: tc.z as i16,
                                    }).await;
                            }
                            return Ok(true);
                        }
                    }
                    // Cursor ID mismatch — stale, drop.
                    return Ok(true);
                }
                Ok(false)
            }

            // Not game-logic — let infra handle it.
            _ => Ok(false),
        }
    }

    fn handle_world_events(
        &mut self,
        _events: &[Arc<WorldEvent>],
        _out: &mut Vec<RawPacket>,
    ) {
        // No game-logic event processing on session side.
        // The controller handles everything inside the worker.
    }

    async fn poll_timer(&mut self) -> TimerEvent {
        // No session-side timers — all timing is in the controller.
        std::future::pending().await
    }

    async fn handle_timer_event(
        &mut self,
        _event: TimerEvent,
        _session: &mut Session,
        _worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) {
        // Nothing to clean up — controller is detached automatically
        // by BaseHandler when the entity is removed (RemoveEntity).
    }

    // ── Infrastructure state access ────────────────────────────────

    fn infra(&self) -> &InfraState { &self.infra }
    fn infra_mut(&mut self) -> &mut InfraState { &mut self.infra }

    // ── Hooks ────────────────────────────────────────────────────────

    async fn on_player_spawned(
        &mut self,
        world_data: &WorldData,
        addr: std::net::SocketAddr,
    ) {
        // Attach LuaController to the player entity.
        if let (Some(p), Some(script_path)) = (&self.infra.player, &self.controller_script) {
            match crate::lua_script::LuaController::from_file(script_path, Some(&world_data.scripts_dir)) {
                Ok(controller) => {
                    debug!(
                        "[{addr}] attaching player controller {} to 0x{:08X}",
                        script_path.display(), p.serial,
                    );
                    let _ = self.worker_tx.send(WorkerCommand::MapCommand(
                        p.world,
                        DemoCommand::AttachController(p.serial, Box::new(controller)),
                    )).await;
                }
                Err(e) => {
                    log::error!(
                        "[{addr}] failed to load controller {}: {}",
                        script_path.display(), e,
                    );
                }
            }
        }
    }
}
