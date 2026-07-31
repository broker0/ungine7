//! `LuaGameLogicHandler` — implements `GameLogicHandler` for the Lua
//! game-logic mode.
//!
//! Wraps `SessionLuaManager` and the per-session infrastructure state.
//! Game-relevant packets are forwarded to the Lua VM as typed events;
//! actions requested by Lua are returned via `poll_timer` / `handle_timer_event`.

use std::sync::Arc;

use protocol::RawPacket;
use packets::traits::encode_packet;

use network::error;
use network::session::Session;

use framework::continuum::WorldEvent;
use framework::diorama::ObserverPipeline;

use crate::{DemoWorkerTx, WorldData};

use super::{
    parsed_packet::ParsedPacket,
    game_logic::{GameLogicHandler, InfraState, TimerEvent},
    lua_handlers::{self, LuaSessionAction, SessionLuaManager},
};

// ── LuaGameLogicHandler ──────────────────────────────────────────────────

pub(super) struct LuaGameLogicHandler {
    lua_mgr: SessionLuaManager,
    /// Buffered action from `poll_timer` — consumed by `handle_timer_event`.
    buffered_action: Option<LuaSessionAction>,

    // ── Infrastructure state ─────────────────────────────────────────
    pub(super) infra: InfraState,
}

impl LuaGameLogicHandler {
    pub fn new(
        worker_tx: DemoWorkerTx,
        observer: Option<ObserverPipeline>,
    ) -> Self {
        Self {
            infra: InfraState::new(observer, &worker_tx, None, u_core::ProtocolVersion::SA_CLIENT),
            lua_mgr: SessionLuaManager::new(worker_tx),
            buffered_action: None,
        }
    }
}

#[async_trait::async_trait]
impl GameLogicHandler for LuaGameLogicHandler {
    async fn handle_packet(
        &mut self,
        parsed: &ParsedPacket,
        _raw: &RawPacket,
        session: &mut Session,
        _worker_tx: &DemoWorkerTx,
    ) -> error::Result<bool> {
        // Forward game-relevant packets to Lua as typed events.
        let is_game_packet = matches!(
            parsed,
            ParsedPacket::AttackRequest { .. }
            | ParsedPacket::DoubleClick { .. }
            | ParsedPacket::TextCommand(_)
            | ParsedPacket::TargetCursor(_)
            | ParsedPacket::WarMode { .. }
            | ParsedPacket::CastTargetedSpell { .. }
        );
        if is_game_packet {
            self.lua_mgr.forward_parsed(parsed);
        }

        // WarMode — also echo as infrastructure (Lua handles the game-logic part).
        if let ParsedPacket::WarMode { fighting } = parsed {
            let reply = packets::system::WarMode::new(*fighting);
            session.send(RawPacket::s2c(encode_packet(&reply))).await?;
        }

        // Lua handler never "consumes" packets — infra always runs too.
        // (DoubleClick needs container handling, WarMode needs echo, etc.)
        Ok(false)
    }

    fn handle_world_events(
        &mut self,
        events: &[Arc<WorldEvent>],
        _out: &mut Vec<RawPacket>,
    ) {
        // Forward all events to the Lua VM.
        if let Some(p) = &self.infra.player {
            let serial = p.serial;
            for event in events {
                self.lua_mgr.forward_event(event, serial);
            }
        }
        // Sync blocking-gump flag so Lua can query it.
        self.lua_mgr.sync_blocking_gump(self.infra.blocking_gump.is_some());
    }

    async fn post_packet(
        &mut self,
        _session: &mut Session,
        _worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        // Sync after infra handled GumpMenuSelection (which may clear blocking_gump).
        self.lua_mgr.sync_blocking_gump(self.infra.blocking_gump.is_some());
        Ok(())
    }

    async fn poll_timer(&mut self) -> TimerEvent {
        // Lua's "timer" is receiving action results from the Lua VM.
        // Buffer the action so handle_timer_event can process it.
        match self.lua_mgr.recv_action().await {
            Some(action) => {
                self.buffered_action = Some(action);
                TimerEvent::LuaAction
            }
            None => {
                // Channel closed — pend forever.
                std::future::pending::<TimerEvent>().await
            }
        }
    }

    async fn handle_timer_event(
        &mut self,
        event: TimerEvent,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        if let TimerEvent::LuaAction = event {
            if let Some(action) = self.buffered_action.take() {
                self.lua_mgr.execute_action(action, session, worker_tx).await?;
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) {
        self.lua_mgr.stop().await;
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
        if let Some(p) = &self.infra.player {
            if let Some(ref script_path) = world_data.session_script {
                self.lua_mgr.start(script_path.clone(), p.serial, p.world);
                log::info!("[{addr}] auto-started session script: {}", script_path.display());
            }
        }
    }

    async fn handle_session_command(
        &mut self,
        packet: &RawPacket,
        session: &mut Session,
    ) -> error::Result<bool> {
        lua_handlers::handle_session_lua_command(
            packet, &self.infra.player, &mut self.lua_mgr, session,
        ).await
    }
}
