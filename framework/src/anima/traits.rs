use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::continuum::WorldEvent;
use super::context::ControlContext;

/// Level of controller access to the game world.
///
/// Declared by the controller via [`EntityController::access_level`].
/// Determines which operations are available through [`ControlContext`].
///
/// In the current implementation — a single context for all levels.
/// In the future it will affect validation and filtering of available methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessLevel {
    /// Read-only: queries, entity lookup, statistics.
    ReadOnly,

    /// Manage own entity with engine validation.
    /// Recommended level for scripts and AI.
    Safe,

    /// Full access to all zone internals.
    /// Only for trusted core-scripts and system mechanics.
    Full,
}

/// Definition of types for the controller system.
///
/// The consumer implements this trait, binding specific enums
/// for events and commands. Similar to `CommandHandler { type Command }`
/// in `continuum` — the framework does not know the concrete variants,
/// but provides infrastructure for dispatch.
///
/// # Example
///
/// ```ignore
/// pub struct MyDef;
///
/// impl ControllerDef for MyDef {
///     type Event = MyEntityEvent;
///     type GlobalEvent = MyGameEvent;
///     type Command = MyCommand;
/// }
/// ```
pub trait ControllerDef: Send + 'static {
    /// Event directed to a specific entity.
    /// Routed to the controller that owns this entity.
    type Event: Send + Debug;

    /// Global zone event, sent to all controllers (broadcast).
    /// Requires `Clone`, because each controller receives its own copy.
    type GlobalEvent: Send + Clone + Debug;

    /// External command directed to a specific entity's controller.
    /// Sources: player, GM, web panel, another controller, etc.
    type Command: Send + Debug;

    /// Create event for timer firing.
    ///
    /// Called by the `Scheduler` when `TaskAction::FireTimer` fires.
    /// Consumer defines how timer_id is turned into its `Event`.
    fn timer_event(entity_serial: u32, timer_id: u64) -> Self::Event;
}

/// Main entity controller trait.
///
/// Controller manages the behavior of a single entity: AI of mobs,
/// NPC scripts, item game mechanics, etc.
///
/// Generic over `D: ControllerDef` — the consumer defines their
/// own event and command types. `Box<dyn EntityController<D>>` works
/// because `D` is fixed at the `ControllerHost<D>` level.
pub trait EntityController<D: ControllerDef>: Send {
    /// Called every game tick.
    ///
    /// `dt` — time elapsed since the previous tick.
    fn tick(&mut self, ctx: &mut ControlContext, dt: Duration) {
        let _ = (ctx, dt);
    }

    /// Event directed to this entity.
    fn on_event(&mut self, ctx: &mut ControlContext, event: D::Event) {
        let _ = (ctx, event);
    }

    /// Global zone event (broadcast to all controllers).
    fn on_global_event(&mut self, ctx: &mut ControlContext, event: D::GlobalEvent) {
        let _ = (ctx, event);
    }

    /// External command (from player, GM, web panel, another controller).
    fn on_command(&mut self, ctx: &mut ControlContext, cmd: D::Command) {
        let _ = (ctx, cmd);
    }

    /// A world event occurred within this controller's subscription area.
    ///
    /// Only called if the controller has an active world-event subscription
    /// (registered via [`ControlContext::subscribe_world_events`]).
    /// The event is shared via `Arc` to avoid cloning.
    fn on_world_event(&mut self, ctx: &mut ControlContext, event: &Arc<WorldEvent>) {
        let _ = (ctx, event);
    }

    /// Desired access level of the controller.
    fn access_level(&self) -> AccessLevel {
        AccessLevel::Safe
    }

    /// Human-readable name of the controller (for logging and debugging).
    fn name(&self) -> &str {
        "unnamed"
    }

    /// Return the [`Instant`] at which this controller needs its next
    /// `tick()` call.
    ///
    /// The [`ControllerHost`](super::ControllerHost) aggregates the minimum across all
    /// controllers and the scheduler to determine the worker's next
    /// wake-up time.  Return `None` (the default) if the controller
    /// does not need periodic ticks — it relies on scheduler timers
    /// or events instead.
    fn next_tick_at(&self) -> Option<Instant> { None }
}
