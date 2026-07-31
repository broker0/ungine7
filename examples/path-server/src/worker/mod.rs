//! Worker module for path-server: command handler and observer registry.
//!
//! Uses [`BaseHandler`] from common for the shared infrastructure
//! (engine, controllers, observers, event coalescing) and adds
//! path-server–specific commands on top.

use std::sync::Arc;

use framework::continuum::{CommandHandler, HashContainerStore, Zone, WorldEvent};
use framework::continuum::WorkerCommand;
use framework::ecumene::Entity;

use common::uo_engine::base_handler::{BaseCommand, BaseHandler};
use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::EngineCommand;
use common::uo_engine::rpc::WrapEngineCommand;
use common::uo_engine::serial_alloc::SerialAllocator;

// ── PathServerCommand ────────────────────────────────────────────────────

/// Commands handled by [`PathServerHandler`].
#[allow(dead_code)]
pub enum PathServerCommand {
    /// Shared command (engine, controllers, observers).
    Base(BaseCommand),

    /// Batch-remove entities by serial list.
    ///
    /// Unlike individual `RemoveEntity` commands, this removes all entities
    /// from the zone in one go and routes `EntityRemoved` events directly
    /// through the observer registry (per-session mpsc channels) instead of
    /// the broadcast channel.  This avoids broadcast overflow when removing
    /// thousands of visual markers at once.
    RemoveEntitiesBatch {
        serials: Vec<u32>,
    },

    /// Schedule (or cancel) the automatic closing of an open door.
    ///
    /// Door open/close is driven from the per-client session task via the
    /// `EngineProxy` RPC, but the auto-close schedule lives in the worker
    /// (single-threaded, owns the zone).  When a session opens a door it
    /// sends `ScheduleDoorClose { serial, at: Some(close_at_ms) }`; when it
    /// closes one manually it sends `at: None` to cancel any pending close.
    ///
    /// `at` is a monotonic-millisecond timestamp from [`door_clock_now_ms`].
    ScheduleDoorClose {
        serial: u32,
        at: Option<i64>,
    },
}

/// Convenience constructors for common base command patterns.
#[allow(non_snake_case)]
impl PathServerCommand {
    pub fn Engine(cmd: EngineCommand) -> Self {
        Self::Base(BaseCommand::Engine(cmd))
    }

    pub fn RegisterObserver(
        session_id: common::uo_engine::observer::SessionId,
        map_id: u8,
        view_rect: framework::ecumene::TileRect,
        tx: tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
        reply: tokio::sync::oneshot::Sender<()>,
    ) -> Self {
        Self::Base(BaseCommand::RegisterObserver { session_id, map_id, view_rect, tx, reply })
    }

    pub fn UpdateObserverView(
        session_id: common::uo_engine::observer::SessionId,
        new_view_rect: framework::ecumene::TileRect,
    ) -> Self {
        Self::Base(BaseCommand::UpdateObserverView { session_id, new_view_rect })
    }

    pub fn UnregisterObserver(
        session_id: common::uo_engine::observer::SessionId,
    ) -> Self {
        Self::Base(BaseCommand::UnregisterObserver { session_id })
    }
}

impl WrapEngineCommand for PathServerCommand {
    fn wrap(cmd: EngineCommand) -> Self {
        Self::Base(BaseCommand::Engine(cmd))
    }
}

// ── PathServerHandler ────────────────────────────────────────────────────

/// [`CommandHandler`] that delegates shared commands to [`BaseHandler`]
/// and handles path-server–specific commands (`RemoveEntitiesBatch`).
pub struct PathServerHandler {
    pub base: BaseHandler,

    /// Pending door auto-close schedule: door serial → monotonic-ms time at
    /// which it should close.  Populated via [`PathServerCommand::ScheduleDoorClose`]
    /// and drained by [`tick_auto_close_doors`].
    door_schedule: std::collections::HashMap<u32, i64>,

    /// Next instant at which an open door is due to auto-close, fed into
    /// [`CommandHandler::next_tick_at`] so the worker wakes precisely when a
    /// door needs servicing.
    next_door_tick: Option<tokio::time::Instant>,
}

impl PathServerHandler {
    pub fn new(
        event_rx: tokio::sync::mpsc::UnboundedReceiver<WorldEvent>,
        serial_alloc: Arc<SerialAllocator>,
    ) -> Self {
        Self {
            base: BaseHandler::new(event_rx, serial_alloc),
            door_schedule: std::collections::HashMap::new(),
            next_door_tick: None,
        }
    }
}

/// Shared monotonic clock for door scheduling (milliseconds).
///
/// The session task (which opens/closes doors) and the worker tick (which
/// auto-closes them) must agree on "now", so both call this single clock.
pub(crate) fn door_clock_now_ms() -> i64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_millis() as i64
}

impl CommandHandler<DemoEntity, HashContainerStore> for PathServerHandler {
    type Command = PathServerCommand;

    fn handle(
        &mut self,
        zone: &mut Zone<DemoEntity, HashContainerStore>,
        cmd: PathServerCommand,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) {
        match cmd {
            PathServerCommand::Base(base_cmd) => {
                self.base.handle_base_command(
                    zone,
                    base_cmd,
                    event_tx,
                    &mut |cmd, _zone| Some(cmd), // no interception
                );
            }

            PathServerCommand::RemoveEntitiesBatch { serials } => {
                let map_id = zone.map_id;
                for serial in serials {
                    let last_pos = zone.get(serial)
                        .map(|e| e.pos())
                        .unwrap_or(u_core::Pos3D::new(0, 0, 0));
                    zone.remove(serial);
                    self.base.observer_registry.route_event(Arc::new(WorldEvent::EntityRemoved {
                        map_id,
                        serial,
                        last_pos,
                    }));
                }
            }

            PathServerCommand::ScheduleDoorClose { serial, at } => {
                match at {
                    Some(close_at) => { self.door_schedule.insert(serial, close_at); }
                    None => { self.door_schedule.remove(&serial); }
                }
                // Recompute the wake instant so the change takes effect promptly.
                self.next_door_tick = next_door_instant(&self.door_schedule);
            }
        }
    }

    fn tick(
        &mut self,
        zone: &mut Zone<DemoEntity, HashContainerStore>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) {
        self.base.tick(zone, event_tx);
        self.next_door_tick =
            tick_auto_close_doors(&mut self.door_schedule, zone, event_tx);
    }

    fn next_tick_at(&mut self) -> Option<tokio::time::Instant> {
        [self.base.next_tick_at(), self.next_door_tick]
            .into_iter()
            .flatten()
            .min()
    }

    fn post_command(&mut self) {
        self.base.flush_events();
    }
}

// ── Door auto-close ────────────────────────────────────────────────────────

/// Compute the next wake instant for the earliest pending door close.
fn next_door_instant(
    schedule: &std::collections::HashMap<u32, i64>,
) -> Option<tokio::time::Instant> {
    let earliest = schedule.values().copied().min()?;
    let now = door_clock_now_ms();
    let delay_ms = (earliest - now).max(0) as u64;
    Some(tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms))
}

/// Close any doors whose scheduled close time has arrived.
///
/// Runs in the worker task with direct, synchronous access to the zone.
/// A door is not closed while a mobile stands on its closed tile — instead
/// the close is deferred by [`crate::doors::DOOR_RETRY_CLOSE_MS`] so it shuts
/// promptly once the doorway clears.  Emits `EntityUpdated` so observers see
/// the leaf swing back.  Returns the next wake instant (earliest remaining
/// pending close), or `None` if nothing is scheduled.
fn tick_auto_close_doors(
    schedule: &mut std::collections::HashMap<u32, i64>,
    zone: &mut Zone<DemoEntity, HashContainerStore>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
) -> Option<tokio::time::Instant> {
    use framework::ecumene::TileRect;
    use u_core::Pos3D;

    if schedule.is_empty() {
        return None;
    }

    let now_ms = door_clock_now_ms();

    // Gather doors that are due (we mutate the zone/schedule below).
    let due: Vec<u32> = schedule
        .iter()
        .filter_map(|(&serial, &close_at)| (now_ms >= close_at).then_some(serial))
        .collect();

    for serial in due {
        // The door must still exist and currently be an item.
        let (graphic, x, y, z, color, amount, is_container, hidden, facing) =
            match zone.get(serial).and_then(|e| e.item()) {
                Some(i) => (
                    i.graphic, i.x, i.y, i.z, i.color, i.amount,
                    i.is_container, i.hidden, i.facing,
                ),
                None => {
                    // Door vanished — drop the stale schedule entry.
                    schedule.remove(&serial);
                    continue;
                }
            };

        let (closed_graphic, dx, dy) = crate::doors::close_target(graphic);
        if closed_graphic == graphic {
            // Already closed (e.g. closed manually); clear.
            schedule.remove(&serial);
            continue;
        }

        let new_x = (x as i32 + dx as i32) as u16;
        let new_y = (y as i32 + dy as i32) as u16;

        // Defer if a mobile is standing on the closed tile.
        let rect = TileRect { x_min: new_x, y_min: new_y, x_max: new_x, y_max: new_y };
        let blocked = zone.query_area(&rect).iter().any(|e| e.is_mobile());
        if blocked {
            schedule.insert(serial, now_ms + crate::doors::DOOR_RETRY_CLOSE_MS);
            continue;
        }

        // Apply the close: update the entity and notify observers.
        let updated = DemoEntity::Item {
            serial,
            graphic: closed_graphic,
            color,
            amount,
            x: new_x,
            y: new_y,
            z,
            is_container,
            hidden,
            facing,
        };
        let snap = Entity::snapshot(&updated);
        zone.update(serial, updated);
        let _ = event_tx.send(WorldEvent::EntityUpdated {
            map_id: zone.map_id,
            serial,
            pos: Pos3D { x: new_x, y: new_y, z },
            entity: snap,
        });

        schedule.remove(&serial);
    }

    next_door_instant(schedule)
}

/// Type alias for the worker channel sender.
pub type PathServerWorkerTx =
    tokio::sync::mpsc::Sender<WorkerCommand<DemoEntity, PathServerCommand>>;
