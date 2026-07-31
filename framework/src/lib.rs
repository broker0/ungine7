pub mod continuum;
pub mod anima;
pub mod diorama;
pub mod ecumene;
pub mod moira;
#[cfg(feature = "lua")]
pub mod mitos;
pub mod rythmos;
pub mod vessel;

// ── Prelude ────────────────────────────────────────────────────────────────

/// Prelude — the most commonly needed framework types.
///
/// ```rust,ignore
/// use framework::prelude::*;
/// ```
pub mod prelude {
    // vessel — core entity & tile abstractions
    pub use crate::vessel::{EntitySnapshot, StaticDataProvider, TileShape, Entity};

    // ecumene — world data, movement, tile types
    pub use crate::ecumene::{
        CollisionSnapshot, DiffAwareDataProvider, DiffOverlay,
        EntityRegistry, LosRay, LosTrace, LosValidator, MovementValidator,
        ShapeProvider, SpatialIndex, StaticTileProvider, StaticWorldData,
        TileBlock, TileProvider, TileRect,
    };

    // continuum — server zone infrastructure
    pub use crate::continuum::{
        CommandHandler, ContainerItem, ContainerInfo, ContainerStore, EntityStore,
        HashContainerStore, HashItemProps, NoContainers, NoItemProps,
        ObserverRegistry, ObserverId,
        Worker, Zone, WorkerCommand,
        WorldEvent, WorldSnapshot, ZoneContainers, ZoneItemProps, ZoneSnapshot,
    };

    // anima — entity controller system
    pub use crate::anima::{
        AccessLevel, ControlContext, ControllerDef, ControllerError,
        ControllerHost, EntityController, EntityInfo, Scheduler,
        TaskAction, TaskId, ZoneAccess,
    };

    // diorama — session observation
    pub use crate::diorama::{
        CompositeTileProvider, EntityData, ObserverEvent, ObserverPipeline,
        PopupMenuEntry, SessionView, StalenessTracker,
        VisibleItem, VisibleKind, VisibleSet, VisibleWorld, WorldEntity,
        generate_bootstrap,
    };

    // moira — identity & access
    pub use crate::moira::{
        Account, AccountStatus, AccountStore, AccountStoreError,
        AccessPolicy, AuthError, Authenticator,
        SessionError, SessionManager, SessionToken,
    };

    // rythmos — movement primitives
    pub use crate::rythmos::{
        ActiveMover, ArbiterResult, ClientId, ClientResponse, MoveArbiter,
        MovePacer, MoveSpeed, MovementTracker, PendingQueue, PendingStep,
        PositionTracker, StepOrigin, ZResolver,
    };
}