//! Demo-server command type and worker channel alias.
//!
//! [`DemoCommand`] is the command enum processed by [`super::handler::DemoHandler`].
//! [`DemoWorkerTx`] is the typed sender half of the worker channel.

use framework::anima::EntityController;
use framework::continuum::WorkerCommand;

use common::uo_engine::controller::{GameCommand, DemoControllerDef};
use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::EngineCommand;

use super::WorldEvent;

// ── Resource harvest request ────────────────────────────────────────────────

/// Where a harvest request's target came from, so the worker can validate it
/// against authoritative state (it owns both the static map data and the
/// entity store; the session does not).
#[derive(Debug, Clone, Copy)]
pub(crate) enum GatherSource {
    /// A static map tile the client claims has graphic `graphic` at the
    /// reported `(x, y, z)`.  The worker re-checks this against the loaded
    /// static data before allowing a harvest.
    StaticTile,
    /// A resource-node item entity (carries the `gather_resource` meta).  Its
    /// existence and coordinates are re-checked against the entity store.
    ItemNode { serial: u32 },
}

/// The worker's answer to a [`DemoCommand::TryHarvestResource`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum HarvestReply {
    /// Something was produced — drop it into the player's backpack.
    Yield { graphic: u16, color: u16, amount: u16, name: &'static str },
    /// The node is exhausted; the player must wait for it to recover.
    Depleted,
    /// Nothing this attempt (failed roll or not-yet-matured).
    Nothing,
    /// The source failed server-side validation (wrong graphic / gone / not a
    /// resource node).
    Invalid,
}

// ── DemoCommand ───────────────────────────────────────────────────────────

/// Commands handled by `DemoHandler`.
#[allow(dead_code)]
pub(crate) enum DemoCommand {
    /// Shared command (engine, controllers, observers).
    Base(common::uo_engine::base_handler::BaseCommand),

    /// Broadcast a sound effect to all observers in range.
    BroadcastSound {
        sound_id: u16,
        x: u16,
        y: u16,
        z: i16,
    },
    /// Broadcast a graphical effect to all observers in range.
    #[allow(dead_code)]
    BroadcastEffect {
        direction_type: u8,
        source_serial: u32,
        target_serial: u32,
        graphic: u16,
        x: u16,
        y: u16,
        z: i8,
        target_x: u16,
        target_y: u16,
        target_z: i8,
        speed: u8,
        duration: u8,
        fixed_direction: bool,
        explode: bool,
    },
    /// Broadcast a character animation to all observers in range.
    #[allow(dead_code)]
    BroadcastAnimation {
        serial: u32,
        action: u16,
        frame_count: u8,
        repeat_count: u16,
        reverse: bool,
        repeat: bool,
        frame_delay: u8,
        x: u16,
        y: u16,
    },
    /// Broadcast speech to all observers in range.
    #[allow(dead_code)]
    BroadcastSpeech {
        serial: u32,
        graphic: u16,
        speech_type: u8,
        color: u16,
        font: u16,
        name: String,
        message: String,
        x: u16,
        y: u16,
    },

    /// Attempt to consume from a resource node and return what was harvested.
    ///
    /// Validation and node state both live in the worker (it owns the static
    /// map data and the entity store), so this single request validates the
    /// source, applies depletion/regeneration, and replies with the result.
    /// See [`crate::resource_nodes`] and `complete_gather`.
    TryHarvestResource {
        /// Reported tile coordinates of the source.
        x: u16,
        y: u16,
        z: i8,
        /// Reported tile graphic of the source (validated by the worker).
        graphic: u16,
        /// Resource category (selects the node policy).
        kind: crate::gathering::GatherKind,
        /// Where the target came from (drives validation).
        source: GatherSource,
        /// Maximum units the caller is willing to take this swing.
        want: u16,
        /// Reply channel.
        reply: tokio::sync::oneshot::Sender<HarvestReply>,
    },

    /// Atomically attach a controller and persist its ID in item_props.meta.
    ///
    /// Unlike `Base(AttachController { .. })`, this variant also writes
    /// `meta["controller"] = controller_id` so that the controller can be
    /// restored after `.save` / `.load`.
    AttachControllerPersist {
        serial: u32,
        controller: Box<dyn EntityController<DemoControllerDef>>,
        /// Persistent ID (e.g. `"wander:3"`, `"lua:travel_stone.lua"`).
        controller_id: String,
    },
}

/// Convenience constructors so existing call sites (`DemoCommand::Engine(...)`,
/// `DemoCommand::RegisterObserver { ... }`, etc.) keep working unchanged.
#[allow(non_snake_case)]
impl DemoCommand {
    pub fn Engine(cmd: EngineCommand) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::Engine(cmd))
    }

    pub fn AttachController(
        serial: u32,
        controller: Box<dyn EntityController<DemoControllerDef>>,
    ) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::AttachController { serial, controller })
    }

    pub fn ControllerCommand(serial: u32, cmd: GameCommand) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::ControllerCommand { serial, cmd })
    }

    pub fn RegisterObserver(
        session_id: common::uo_engine::observer::SessionId,
        map_id: u8,
        view_rect: framework::ecumene::TileRect,
        tx: tokio::sync::mpsc::Sender<std::sync::Arc<WorldEvent>>,
        reply: tokio::sync::oneshot::Sender<()>,
    ) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::RegisterObserver {
            session_id, map_id, view_rect, tx, reply,
        })
    }

    pub fn UnregisterObserver(
        session_id: common::uo_engine::observer::SessionId,
    ) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::UnregisterObserver { session_id })
    }

    pub fn UpdateObserverView(
        session_id: common::uo_engine::observer::SessionId,
        new_view_rect: framework::ecumene::TileRect,
    ) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::UpdateObserverView {
            session_id, new_view_rect,
        })
    }
}

impl common::uo_engine::rpc::WrapEngineCommand for DemoCommand {
    fn wrap(cmd: EngineCommand) -> Self {
        Self::Base(common::uo_engine::base_handler::BaseCommand::Engine(cmd))
    }
}

// ── DemoWorkerTx ──────────────────────────────────────────────────────────

pub(crate) type DemoWorkerTx = tokio::sync::mpsc::Sender<WorkerCommand<DemoEntity, DemoCommand>>;
