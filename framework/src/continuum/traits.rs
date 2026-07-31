
use crate::vessel::objects::Entity;
use super::container::{ZoneContainers, NoContainers};
use super::item_props::{ZoneItemProps, NoItemProps};
use super::world_event::WorldEvent;
use super::zone::Zone;

use tokio::time::Instant;

/// Zone command handler.
///
/// The `event_tx` parameter allows the handler to publish [`WorldEvent`]s
/// that are routed to all subscribed sessions.  Use
/// `let _ = event_tx.send(event);` — the send is non-blocking and the
/// result can be safely ignored (returns `Err` only when the receiver
/// has been dropped).
pub trait CommandHandler<E: Entity, C: ZoneContainers = NoContainers, P: ZoneItemProps = NoItemProps>: Send + 'static {
    type Command: Send + 'static;
    fn handle(
        &mut self,
        zone: &mut Zone<E, C, P>,
        cmd: Self::Command,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    );
    fn tick(&mut self, zone: &mut Zone<E, C, P>, _event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>) {
        let _ = zone;
    }

    /// Return the [`Instant`] at which the next periodic tick should fire.
    ///
    /// The worker uses this to sleep precisely until the next scheduled
    /// task instead of polling at a fixed interval.  Return `None` if no
    /// periodic tick is needed (e.g. no AI controllers, no scheduler
    /// tasks) — the worker will only wake for incoming commands.
    ///
    /// The default implementation returns `None` (no periodic tick).
    fn next_tick_at(&mut self) -> Option<Instant> { None }

    /// Called by the worker after processing a batch of commands.
    ///
    /// Use this to flush side-effects (e.g. drain and route events)
    /// immediately instead of waiting for the next periodic tick.
    /// The default implementation is a no-op.
    fn post_command(&mut self) { }

    /// Called by the worker when an entity is about to be transferred
    /// out of a zone.
    ///
    /// Implementations should detach any associated state (AI controllers,
    /// per-entity timers, etc.) and return it as opaque data.
    /// The default implementation returns `None` (no state to transfer).
    ///
    /// The returned `Box<dyn std::any::Any + Send>` will be passed to
    /// [`on_entity_entering_zone`](Self::on_entity_entering_zone) on the destination side.
    fn on_entity_leaving_zone(
        &mut self,
        _serial: u32,
    ) -> Option<Box<dyn std::any::Any + Send>> {
        None
    }

    /// Called by the worker after an entity has been transferred into a zone.
    ///
    /// `controller_state` is the opaque data returned by
    /// [`on_entity_leaving_zone`](Self::on_entity_leaving_zone) on the source side — typically a
    /// detached AI controller.  `to_map` is the destination map the entity
    /// is entering, so implementations can re-bind controllers/timers to the
    /// correct world.  The default implementation drops the state.
    fn on_entity_entering_zone(
        &mut self,
        _serial: u32,
        _controller_state: Option<Box<dyn std::any::Any + Send>>,
        _to_map: u8,
    ) {
    }
}

/// Store of all entities in a Zone (Arena / SlotMap)
pub trait EntityStore<E: Entity>: Send + Sync {
    fn new() -> Self where Self: Sized;
    fn insert(&mut self, id: u32, data: E);
    fn remove(&mut self, id: u32) -> Option<E>;
    fn get(&self, id: u32) -> Option<&E>;
    fn get_mut(&mut self, id: u32) -> Option<&mut E>;
    fn iter(&self) -> Box<dyn Iterator<Item = (&u32, &E)> + '_>;
    /// Remove all entities from the store.
    fn clear(&mut self);
}
