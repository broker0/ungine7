//! Demo-server listener: [`DemoServer`] and shared [`WorldData`].

use std::path::PathBuf;
use std::sync::Arc;

use protocol::Protocol;
use protocol::RawPacket;

use network::error;
use network::handler::HandlerChain;
use network::listener::{
    ConnectionContext, ListenerHandler, SessionPhase,
};
use network::session::Session;

use framework::ecumene::StaticDataProvider;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::serial_alloc::SerialAllocator;

use crate::commands::DemoWorkerTx;
use crate::game_session;
use crate::game_session::SessionMode;

// ── WorldData ─────────────────────────────────────────────────────────────

/// Shared immutable world data extracted from the log.
pub(crate) struct WorldData {
    /// Primary player serial from the log (used for bootstrap position).
    pub player_serial: u32,
    pub player_world: u8,
    /// Serials that can be selected as playable characters.
    /// Slot index in the character list maps to this vec.
    pub playable_serials: Vec<u32>,
    /// Entities (cloned for each connecting client's reference).
    #[allow(dead_code)]
    pub entities: std::collections::HashMap<u8, std::collections::HashMap<u32, DemoEntity>>,
    /// Bootstrap packets generated from the log via [`ObserverPipeline`](framework::diorama::ObserverPipeline).
    /// Contains the canonical S→C packet sequence (0x1B, 0xB9, 0xBF SetMap,
    /// 0x20 DrawGamePlayer, visible entities, 0x55 LoginComplete).
    pub bootstrap_packets: Vec<RawPacket>,
    /// If set, test accounts spawn in a cluster of this half-size (tiles)
    /// around Britain bank (1438, 1696).  `None` = full map.
    pub cluster: Option<u16>,
    /// Per-entity UpdateMobile throttle interval (ms).  0 = disabled.
    pub move_throttle_ms: u64,
    /// Path to a Lua session script to auto-load for every player.
    /// `None` = no auto-load (use `.slua` command manually).
    #[cfg(feature = "lua")]
    pub session_script: Option<PathBuf>,
    /// Path to a Lua controller script to attach to every player.
    #[cfg(feature = "lua")]
    pub controller_script: Option<PathBuf>,
    /// Base directory for Lua scripts.
    #[cfg(feature = "lua")]
    pub scripts_dir: PathBuf,
    /// Default session mode applied to **new** connections.
    ///
    /// Encoded via [`SessionMode::as_u8`]; an administrator can change it at
    /// runtime with the `.session` dot-command.  Changing it does not affect
    /// already-running sessions — they keep their mode until they reconnect.
    pub default_session_mode: std::sync::atomic::AtomicU8,
    /// Mapping from test account name → allocated mobile serial.
    ///
    /// Populated lazily on first spawn; used to give the same character
    /// back on reconnect and to ensure test-account serials never collide
    /// with Lua-spawned NPC serials (which also use `alloc_mobile()`).
    pub test_serials: tokio::sync::RwLock<std::collections::HashMap<String, u32>>,
    /// Mapping from normal account name → characters created via the
    /// client's character-creation screen (packet 0x00).
    ///
    /// Populated when a player creates a character; used to offer the same
    /// characters on the selection screen on every reconnect.
    pub account_characters:
        tokio::sync::RwLock<std::collections::HashMap<String, Vec<common::spawn::CharacterRecord>>>,
    /// Sender to the logout-reaper task.
    ///
    /// Sessions send [`crate::logout::ReaperCmd::Arm`] on disconnect and
    /// [`crate::logout::ReaperCmd::Cancel`] on re-login.
    pub reaper_tx: tokio::sync::mpsc::Sender<crate::logout::ReaperCmd>,
}

impl WorldData {
    /// Current default session mode for new connections.
    pub(crate) fn session_mode(&self) -> SessionMode {
        use std::sync::atomic::Ordering;
        SessionMode::from_u8(self.default_session_mode.load(Ordering::Relaxed))
    }

    /// Set the default session mode for **future** connections.
    pub(crate) fn set_session_mode(&self, mode: SessionMode) {
        use std::sync::atomic::Ordering;
        self.default_session_mode.store(mode.as_u8(), Ordering::Relaxed);
    }
}

// ── DemoServer ────────────────────────────────────────────────────────────

pub(crate) struct DemoServer {
    /// Channel to the worker.
    pub(crate) worker_tx: DemoWorkerTx,
    /// World data loaded from the log.
    pub(crate) world_data: Arc<WorldData>,
    /// Static world data for movement validation / observer.
    pub(crate) static_data: Option<Arc<dyn StaticDataProvider>>,
    /// Shared login handler (moira login phase).
    pub(crate) login_handler: common::login_handler::LoginHandler,
    /// Centralised serial allocator shared across all sessions.
    pub(crate) serial_alloc: Arc<SerialAllocator>,
    /// Channel to the Lua script manager.
    #[cfg(feature = "lua")]
    pub(crate) lua_cmd_tx: tokio::sync::mpsc::Sender<crate::lua_script::LuaCommand>,
}

#[async_trait::async_trait]
impl ListenerHandler for DemoServer {
    fn configure_handlers(
        &self,
        _phase: SessionPhase,
        _ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        (HandlerChain::new(), HandlerChain::new())
    }

    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut session: Session,
    ) -> error::Result<()> {
        let addr = ctx.addr;
        let is_game = matches!(&ctx.protocol, Protocol::Game(_));

        if is_game {
            // Prefer the version recorded by the login-phase binder (authoritative),
            // fall back to the version inferred by the game-phase detector.
            let client_version = ctx.bound_connection
                .as_ref()
                .map(|b| b.client_version)
                .unwrap_or_else(|| ctx.protocol.client_version());

            // Create a per-session mpsc channel for routed world events.
            // The sender will be registered with the ObserverRegistry
            // when the player spawns (inside game_session).
            let (observer_tx, observer_rx) = tokio::sync::mpsc::channel(4096);
            #[cfg(feature = "lua")]
            let lua_cmd_tx = self.lua_cmd_tx.clone();
            game_session::run_game_session(
                &mut session,
                &self.worker_tx,
                &self.world_data,
                &self.static_data,
                &self.login_handler.session_manager,
                &self.serial_alloc,
                addr,
                client_version,
                observer_rx,
                observer_tx,
                #[cfg(feature = "lua")]
                lua_cmd_tx,
            )
            .await?;
        } else {
            // Login session — delegated to the shared login handler.
            self.login_handler.run_login_session(&mut session, ctx).await?;
        }

        session.close().await;
        Ok(())
    }
}
