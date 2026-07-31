//! `GameLogicHandler` trait — the unified interface for game-logic
//! implementations (Rust or Lua).
//!
//! Infrastructure (login, movement, items, containers, entity streaming)
//! is handled **before** the game-logic handler is called — in the shared
//! `infra.rs` code.  The handler only receives packets and events that
//! may contain game-logic decisions (spells, combat, skills, bandaging,
//! mounting, etc.).

use std::sync::Arc;

use protocol::RawPacket;

use network::error;
use network::session::Session;

use framework::continuum::WorldEvent;
use framework::diorama::ObserverPipeline;
use framework::ecumene::StaticDataProvider;

use common::uo_engine::auth::AccessLevel;

use common::uo_engine::rpc::EngineProxy;

use crate::{DemoCommand, DemoWorkerTx};

use super::{
    PlayerState,
    containers, items, spawn,
    pending_cursor::PendingCursor,
    parsed_packet::ParsedPacket,
    vendor_session::VendorSession,
};

// ── GameLogicHandler trait ────────────────────────────────────────────────

/// Unified interface for game-logic implementations.
///
/// Both `RustGameLogicHandler` and (future) `LuaGameLogicHandler` implement
/// this trait.  The session loop calls these methods in a defined order:
///
/// 1. `handle_packet` — for each client packet (after infra)
/// 2. `handle_world_event` — for each world event (after infra collection)
/// 3. `poll_timer` + `handle_timer_event` — in the `select!` loop
/// 4. `shutdown` — on disconnect
#[async_trait::async_trait]
pub(super) trait GameLogicHandler: Send {
    /// Process a client packet that may contain game-logic.
    ///
    /// Called after infrastructure handlers.  `parsed` carries pre-parsed
    /// fields; `raw` is available for sub-handlers not yet migrated.
    ///
    /// Returns `true` if the packet was consumed (infra should skip it).
    async fn handle_packet(
        &mut self,
        parsed: &ParsedPacket,
        raw: &RawPacket,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<bool>;

    /// Process a batch of world events for game-logic purposes.
    ///
    /// Called after infrastructure event collection.  `out` collects any
    /// additional packets to send (e.g. fizzle on damage, attack cancel).
    fn handle_world_events(
        &mut self,
        events: &[Arc<WorldEvent>],
        out: &mut Vec<RawPacket>,
    );

    /// Wait for the next game-logic timer to fire.
    ///
    /// This is called as one arm of the main `tokio::select!`.  It must
    /// NOT borrow `Session` (which is borrowed by `session.recv()` in
    /// another arm).  Instead, it returns a `TimerEvent` that the session
    /// loop processes via `handle_timer_event`.
    ///
    /// If no timers are active, this should pend forever (never resolve).
    async fn poll_timer(&mut self) -> TimerEvent;

    /// Handle a timer event returned by `poll_timer`.
    ///
    /// This is called with full access to `session` and `worker_tx`.
    async fn handle_timer_event(
        &mut self,
        event: TimerEvent,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()>;

    /// Cleanup on disconnect.
    async fn shutdown(&mut self);

    // ── Infrastructure state access ──────────────────────────────────
    //
    // Returns a mutable reference to all infrastructure state at once,
    // avoiding the split-borrow problem with individual accessor methods.

    fn infra(&self) -> &InfraState;
    fn infra_mut(&mut self) -> &mut InfraState;

    // ── Hooks ────────────────────────────────────────────────────────

    /// Called after a successful LoginCharacter (0x5D).
    async fn on_player_spawned(
        &mut self,
        _world_data: &crate::WorldData,
        _addr: std::net::SocketAddr,
    ) {}

    /// Called after world events are processed.
    async fn post_world_events(
        &mut self,
        _session: &mut Session,
        _worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        Ok(())
    }

    /// Called after each packet is processed.
    async fn post_packet(
        &mut self,
        _session: &mut Session,
        _worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        Ok(())
    }

    /// Handle .slua dot-commands.  Returns `true` if consumed.
    async fn handle_session_command(
        &mut self,
        _packet: &RawPacket,
        _session: &mut Session,
    ) -> error::Result<bool> {
        Ok(false)
    }
}

// ── InfraState ────────────────────────────────────────────────────────────

/// Infrastructure state shared between the session loop and the handler.
///
/// Grouped into a single struct so the borrow checker allows simultaneous
/// access to multiple fields (split borrows on struct fields).
pub(super) struct InfraState {
    pub player: Option<PlayerState>,
    pub test_account: Option<spawn::TestAccountInfo>,
    /// Authenticated account username (set on GameLogin 0x91).  Used to
    /// associate created characters with the account in `WorldData`.
    pub account_name: Option<String>,
    pub access_level: AccessLevel,
    pub observer: Option<ObserverPipeline>,
    pub pending_cursor: Option<PendingCursor>,
    pub held_item: Option<items::HeldItem>,
    pub open_containers: containers::OpenContainers,
    /// When set, a blocking gump is open — spells and skills are blocked.
    /// Tuple: `(source_serial, gump_id)` for matching gump responses.
    pub blocking_gump: Option<(u32, u32)>,
    /// `true` when the player is dead (a ghost).  Blocks combat, casting,
    /// skills, bandaging, and most interactions until resurrected.
    pub dead: bool,
    /// Currently open vendor (the NPC serial), if a buy/sell window is up.
    pub open_vendor: Option<VendorSession>,
    /// House serial whose management gump is currently open (via sign click).
    /// Used to route the gump response (Demolish, etc.) to the right house.
    pub open_house_gump: Option<u32>,
    /// Currently open blacksmithing-gump category (via smith's hammer).
    /// `None` when no craft gump is open.
    pub open_craft: Option<crate::crafting::CraftCategory>,
    /// Cached engine proxy — avoids constructing a new proxy per call.
    /// The `world` field is updated when the player's map changes.
    pub engine: EngineProxy<DemoCommand>,
    /// Static world data (tiledata + multi.mul + map geometry).
    ///
    /// Used by session-side logic that needs file data without a worker
    /// round-trip — e.g. computing a ship's footprint and deck height from
    /// its multi definition.  `None` when the server runs without `--data`.
    pub static_data: Option<Arc<dyn StaticDataProvider>>,
    /// Pending cross-world teleport request.
    ///
    /// Set by handlers that don't own the observer / event-rx plumbing
    /// (movement step onto a teleporter tile, double-click on a teleporter,
    /// recall to a marked rune in another world).  Drained and executed once
    /// in the session loop, which owns `observer`, `event_rx` and the
    /// observer event sender needed by
    /// [`transfer_player`](super::transfer::transfer_player).
    pub pending_teleport: Option<PendingTeleport>,
    /// Client version of the connected session — used to select the correct
    /// wire format for version-dependent packets (e.g. DrawMobile equipment
    /// list).
    pub client_version: u_core::ProtocolVersion,
}

/// A deferred teleport to be executed by the session loop.
#[derive(Debug, Clone, Copy)]
pub(super) struct PendingTeleport {
    pub world: u8,
    pub x: u16,
    pub y: u16,
    pub z: i8,
}

impl InfraState {
    pub fn new(
        observer: Option<ObserverPipeline>,
        worker_tx: &DemoWorkerTx,
        static_data: Option<Arc<dyn StaticDataProvider>>,
        client_version: u_core::ProtocolVersion,
    ) -> Self {
        Self {
            player: None,
            test_account: None,
            account_name: None,
            access_level: AccessLevel::Player,
            observer,
            pending_cursor: None,
            held_item: None,
            open_containers: containers::OpenContainers::new(),
            blocking_gump: None,
            dead: false,
            open_vendor: None,
            open_house_gump: None,
            open_craft: None,
            engine: EngineProxy::new(worker_tx.clone(), 0),
            static_data,
            pending_teleport: None,
            client_version,
        }
    }

    /// Update the cached engine proxy's world to match the player's map.
    pub fn sync_engine_world(&mut self) {
        if let Some(p) = &self.player {
            self.engine.world = p.world;
        }
    }
}

// ── TimerEvent ────────────────────────────────────────────────────────────

/// Event emitted by `GameLogicHandler::poll_timer` when a timer fires.
///
/// The session loop passes this to `handle_timer_event` for processing.
#[derive(Debug)]
#[allow(dead_code)] // LuaAction only used in lua-session mode
pub(super) enum TimerEvent {
    /// A spell cast completed.
    CastComplete,
    /// A skill use completed.
    SkillComplete,
    /// A bandage heal completed.
    BandageComplete,
    /// Periodic regen tick.
    RegenTick,
    /// Melee swing charged (ready to attempt a strike).
    SwingCharged,
    /// A Lua session action is ready (buffered in the handler).
    LuaAction,
}
