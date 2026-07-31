use crate::vessel::objects::Entity;
use super::container::{ContainerInfo, ZoneContainers, NoContainers};
use super::item_props::{ZoneItemProps, NoItemProps};
use super::traits::CommandHandler;
use super::world_event::WorldEvent;
use super::zone::Zone;
use log::{info, warn};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tokio::time::Instant;
use std::marker::PhantomData;
use u_core::Pos3D;

pub enum WorkerCommand<E: Entity, Cmd> {
    MapCommand(u8, Cmd),
    GlobalCommand(Cmd),
    /// Cross-zone operation that touches two zones atomically within a
    /// single worker tick.
    CrossZone(CrossZoneOp<E>),
    _Marker(PhantomData<E>),
}

// ── Cross-zone operations ────────────────────────────────────────────────

/// Operations that involve two zones and must be executed atomically.
pub enum CrossZoneOp<E: Entity> {
    /// Move an entity (and all its associated data — containers, item
    /// properties) from one zone to another.
    ///
    /// The entity is removed from `from_map`, its position is updated,
    /// and it is spawned into `to_map`.  Container hierarchies (backpack
    /// and nested containers) and item properties are transferred along
    /// with the entity.
    ///
    /// Emits `EntityRemoved` on the source zone and `EntitySpawned` on
    /// the destination zone so that observers see the change.
    TransferEntity {
        from_map: u8,
        to_map: u8,
        serial: u32,
        new_x: u16,
        new_y: u16,
        new_z: i8,
        new_direction: Option<u8>,
        reply: tokio::sync::oneshot::Sender<Result<TransferResult<E>, TransferError>>,
    },
}

/// Result of a successful cross-zone entity transfer.
pub struct TransferResult<E: Entity> {
    /// The entity after transfer (with updated position).
    pub entity: E,
    /// Source zone map_id.
    pub from_map: u8,
    /// Destination zone map_id.
    pub to_map: u8,
}

/// Errors that can occur during a cross-zone transfer.
#[derive(Debug, Clone)]
pub enum TransferError {
    /// The entity was not found in the source zone.
    EntityNotFound,
    /// The source zone does not exist and could not be created.
    SourceZoneMissing,
    /// The destination zone does not exist and could not be created.
    DestinationZoneMissing,
}

/// Optional factory closure that creates a new [`Zone`] for a given
/// `map_id`.  Used by [`Worker`] to auto-create zones on first access.
pub type ZoneFactory<E, C = NoContainers, P = NoItemProps> = Box<dyn Fn(u8) -> Zone<E, C, P> + Send>;

pub struct Worker<E: Entity, C: ZoneContainers, H: CommandHandler<E, C, P>, P: ZoneItemProps = NoItemProps> {
    pub zones: HashMap<u8, Zone<E, C, P>>,
    pub rx: Receiver<WorkerCommand<E, H::Command>>,
    pub handler: H,
    zone_factory: Option<ZoneFactory<E, C, P>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<WorldEvent>,
}

impl<E: Entity, C: ZoneContainers, H: CommandHandler<E, C, P>, P: ZoneItemProps> Worker<E, C, H, P> {
    pub fn new(rx: Receiver<WorkerCommand<E, H::Command>>, handler: H) -> Self {
        let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
        Self {
            zones: HashMap::new(),
            rx,
            handler,
            zone_factory: None,
            event_tx,
        }
    }

    /// Create a worker with a zone factory.  When a [`WorkerCommand::MapCommand`] arrives
    /// for an unknown `map_id`, the factory is called to create the zone
    /// on the fly — no need to pre-register every possible map.
    pub fn with_factory(
        rx: Receiver<WorkerCommand<E, H::Command>>,
        handler: H,
        factory: ZoneFactory<E, C, P>,
    ) -> Self {
        let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
        Self {
            zones: HashMap::new(),
            rx,
            handler,
            zone_factory: Some(factory),
            event_tx,
        }
    }

    /// Create a worker with a zone factory and an externally-created
    /// event sender.
    ///
    /// This is useful when the event channel is created outside the
    /// worker so that other components (e.g. the command handler) can
    /// own the receiver side before the worker starts running.
    pub fn with_factory_and_sender(
        rx: Receiver<WorkerCommand<E, H::Command>>,
        handler: H,
        factory: ZoneFactory<E, C, P>,
        event_tx: tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) -> Self {
        Self {
            zones: HashMap::new(),
            rx,
            handler,
            zone_factory: Some(factory),
            event_tx,
        }
    }

    /// Ensure a zone exists for `map_id`, creating one via the factory if
    /// necessary.  Returns `true` if the zone is available afterwards.
    fn ensure_zone(&mut self, map_id: u8) -> bool {
        if self.zones.contains_key(&map_id) {
            return true;
        }
        if let Some(factory) = &self.zone_factory {
            let zone = factory(map_id);
            self.zones.insert(map_id, zone);
            info!("[worker] auto-created zone for map {}", map_id);
            true
        } else {
            false
        }
    }

    pub async fn run(self) {
        self.run_adaptive().await;
    }

    /// Run the worker loop with a configurable tick interval.
    ///
    /// Combines command processing with periodic ticks:
    /// - Commands are processed as they arrive via the mpsc channel.
    /// - `handler.tick(zone)` is called on every zone at each interval.
    ///
    /// The loop exits when the channel is closed (all senders dropped).
    pub async fn run_with_tick(mut self, tick_interval: Duration) {
        let mut interval = tokio::time::interval(tick_interval);
        // Don't try to "catch up" if ticks are slow.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;

                cmd = self.rx.recv() => {
                    match cmd {
                        Some(WorkerCommand::MapCommand(map_id, map_cmd)) => {
                            self.ensure_zone(map_id);
                            if let Some(zone) = self.zones.get_mut(&map_id) {
                                self.handler.handle(zone, map_cmd, &self.event_tx);
                            }
                        }
                        Some(WorkerCommand::GlobalCommand(glob_cmd)) => {
                            for zone in self.zones.values_mut() {
                                self.handler.handle(zone, glob_cmd, &self.event_tx);
                                break;
                            }
                        }
                        Some(WorkerCommand::CrossZone(op)) => {
                            self.handle_cross_zone(op);
                        }
                        Some(WorkerCommand::_Marker(_)) => {}
                        None => {
                            // Channel closed — all senders dropped.
                            info!("[worker] channel closed, shutting down");
                            break;
                        }
                    }
                }

                _ = interval.tick() => {
                    for zone in self.zones.values_mut() {
                        self.handler.tick(zone, &self.event_tx);
                    }
                }
            }
        }
    }

    /// Run the worker loop with adaptive tick timing.
    ///
    /// Instead of a fixed-interval tick, the worker asks the handler
    /// when the next tick should fire via [`CommandHandler::next_tick_at`].
    /// If the handler returns `None` (no scheduled work), the worker
    /// sleeps until the next command arrives — zero idle wakeups.
    ///
    /// After processing each batch of commands (drain via `try_recv`),
    /// [`CommandHandler::post_command`] is called to flush side-effects
    /// (e.g. route events) immediately.
    pub async fn run_adaptive(mut self) {
        loop {
            // Ask the handler when the next periodic tick should fire.
            let next_tick = self.handler.next_tick_at();

            tokio::select! {
                biased;

                cmd = self.rx.recv() => {
                    match cmd {
                        Some(first_cmd) => {
                            // Process the first command that woke us.
                            self.dispatch_command(first_cmd);

                            // Drain any additional commands already in the
                            // channel (batch processing).
                            while let Ok(cmd) = self.rx.try_recv() {
                                self.dispatch_command(cmd);
                            }

                            // Flush side-effects after the entire batch.
                            self.handler.post_command();
                        }
                        None => {
                            info!("[worker] channel closed, shutting down");
                            break;
                        }
                    }
                }

                _ = sleep_until_opt(next_tick) => {
                    for zone in self.zones.values_mut() {
                        self.handler.tick(zone, &self.event_tx);
                    }
                }
            }
        }
    }

    /// Dispatch a single worker command to the appropriate handler method.
    fn dispatch_command(&mut self, cmd: WorkerCommand<E, H::Command>) {
        match cmd {
            WorkerCommand::MapCommand(map_id, map_cmd) => {
                self.ensure_zone(map_id);
                if let Some(zone) = self.zones.get_mut(&map_id) {
                    self.handler.handle(zone, map_cmd, &self.event_tx);
                }
            }
            WorkerCommand::GlobalCommand(glob_cmd) => {
                for zone in self.zones.values_mut() {
                    self.handler.handle(zone, glob_cmd, &self.event_tx);
                    break;
                }
            }
            WorkerCommand::CrossZone(op) => {
                self.handle_cross_zone(op);
            }
            WorkerCommand::_Marker(_) => {}
        }
    }

    // ── Cross-zone operations ────────────────────────────────────────

    fn handle_cross_zone(&mut self, op: CrossZoneOp<E>) {
        match op {
            CrossZoneOp::TransferEntity {
                from_map, to_map, serial,
                new_x, new_y, new_z, new_direction, reply,
            } => {
                let result = self.transfer_entity(
                    from_map, to_map, serial,
                    new_x, new_y, new_z, new_direction,
                );
                let _ = reply.send(result);
            }
        }
    }

    /// Atomically transfer an entity (+ containers + item props) from
    /// one zone to another.
    ///
    /// 1. Remove entity from source zone → get `E`
    /// 2. Collect related containers (backpack hierarchy)
    /// 3. Collect related item properties
    /// 4. Update entity position
    /// 5. Spawn entity in destination zone
    /// 6. Insert containers + item props into destination
    /// 7. Emit `EntityRemoved` / `EntitySpawned` events
    fn transfer_entity(
        &mut self,
        from_map: u8,
        to_map: u8,
        serial: u32,
        new_x: u16,
        new_y: u16,
        new_z: i8,
        new_direction: Option<u8>,
    ) -> Result<TransferResult<E>, TransferError> {
        // Ensure both zones exist.
        if !self.ensure_zone(from_map) {
            return Err(TransferError::SourceZoneMissing);
        }
        if !self.ensure_zone(to_map) {
            return Err(TransferError::DestinationZoneMissing);
        }

        // ── Phase 0: Detach controller state ────────────────────────
        //
        // Call the handler hook *before* touching the zones.
        // The handler can detach an AI controller, cancel timers, etc.
        let controller_state = self.handler.on_entity_leaving_zone(serial);

        // ── Phase 1: Extract from source zone ───────────────────────

        let (mut entity, last_pos, containers, prop_serials, prop_values) = {
            let src = self.zones.get_mut(&from_map)
                .ok_or(TransferError::SourceZoneMissing)?;

            // Remove entity from zone (store + spatial + collision).
            let entity = src.remove(serial)
                .ok_or(TransferError::EntityNotFound)?;

            let last_pos = entity.pos();

            // Collect equipment item serials (for item props transfer).
            let equip_serials = entity.equipment_serials();

            // Collect backpack serial and recursively gather all
            // containers belonging to this entity.
            let backpack_serial = entity.backpack_serial();
            let containers = collect_and_remove_containers(
                &mut src.containers,
                backpack_serial,
            );

            // Gather all item serials that need property transfer:
            // the entity's OWN serial (name, pet/vendor tags, controller
            // persist key, logout_return, …) + equipment + all items inside
            // collected containers.
            //
            // Previously the entity's own item_props were silently dropped on
            // transfer.  Adding `serial` here ensures they follow the entity
            // to the destination zone — a clean remove→insert by the same key.
            let mut related_serials: Vec<u32> = equip_serials;
            related_serials.push(serial);
            for (cs, info) in &containers {
                related_serials.push(*cs);
                for s in info.item_serials() {
                    related_serials.push(s);
                }
            }

            // Extract item properties for all related serials.
            let mut prop_serials_out = Vec::new();
            let mut prop_values_out = Vec::new();
            for &s in &related_serials {
                if let Some(val) = src.item_props.remove(s) {
                    prop_serials_out.push(s);
                    prop_values_out.push(val);
                }
            }

            (entity, last_pos, containers, prop_serials_out, prop_values_out)
        };

        // ── Phase 2: Update entity position ─────────────────────────

        entity.set_pos(Pos3D::new(new_x, new_y, new_z));
        if let Some(dir) = new_direction {
            entity.set_direction(dir);
        }

        // ── Phase 3: Emit EntityRemoved on source zone ──────────────

        let _ = self.event_tx.send(WorldEvent::EntityRemoved {
            map_id: from_map,
            serial,
            last_pos,
        });

        // ── Phase 4: Insert into destination zone ───────────────────

        let snapshot = entity.snapshot();
        let new_pos = entity.pos();

        {
            let dst = self.zones.get_mut(&to_map)
                .ok_or(TransferError::DestinationZoneMissing)?;

            dst.spawn(serial, entity.clone());

            // Restore containers.
            for (cs, info) in containers {
                dst.containers.insert(cs, info);
            }

            // Restore item properties.
            for (s, val) in prop_serials.into_iter().zip(prop_values) {
                dst.item_props.insert(s, val);
            }
        }

        // ── Phase 5: Re-attach controller state ─────────────────────

        self.handler.on_entity_entering_zone(serial, controller_state, to_map);

        // ── Phase 6: Emit EntitySpawned on destination zone ─────────

        let _ = self.event_tx.send(WorldEvent::EntitySpawned {
            map_id: to_map,
            serial,
            pos: new_pos,
            entity: snapshot,
        });

        info!(
            "[worker] transferred entity {:#010X} from map {} to map {} at ({},{},{})",
            serial, from_map, to_map, new_x, new_y, new_z,
        );

        Ok(TransferResult {
            entity,
            from_map,
            to_map,
        })
    }
}

// ── Adaptive sleep helper ─────────────────────────────────────────────────

/// Sleep until the given deadline, or wait forever if `None`.
///
/// Used by [`Worker::run_adaptive`] in the `select!` tick arm.
/// When the handler has no scheduled work (`None`), the future never
/// completes so the tick arm is effectively disabled — the worker only
/// wakes for incoming commands.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

// ── Container collection helper ──────────────────────────────────────────

/// Recursively collect all containers belonging to an entity, starting
/// from `backpack_serial`, and remove them from the source container store.
///
/// Walks the container hierarchy depth-first:
/// 1. Start with the backpack (if any)
/// 2. For each container, check if any of its items are themselves
///    containers (i.e. exist as keys in the store)
/// 3. Recurse into sub-containers
///
/// Returns `Vec<(serial, ContainerInfo)>` of all collected containers.
fn collect_and_remove_containers<C: ZoneContainers>(
    store: &mut C,
    backpack_serial: Option<u32>,
) -> Vec<(u32, ContainerInfo)> {
    let Some(bp_serial) = backpack_serial else {
        return Vec::new();
    };

    let mut collected = Vec::new();
    let mut queue = vec![bp_serial];

    // Depth limit to prevent infinite loops on malformed data.
    const MAX_DEPTH: usize = 64;
    let mut iterations = 0;

    while let Some(cs) = queue.pop() {
        iterations += 1;
        if iterations > MAX_DEPTH {
            warn!(
                "[transfer] container hierarchy depth limit ({}) reached, \
                 stopping collection",
                MAX_DEPTH,
            );
            break;
        }

        if let Some(info) = store.remove_entry(cs) {
            // Check if any items inside this container are themselves containers.
            for item_serial in info.item_serials() {
                // If this item serial is itself a container in the store,
                // queue it for collection.
                if store.get(item_serial).is_some() {
                    queue.push(item_serial);
                }
            }
            collected.push((cs, info));
        }
    }

    collected
}
