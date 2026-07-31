//! Shared base handler for UO server examples.
//!
//! [`BaseHandler`] provides the common worker infrastructure used by
//! every concrete server (demo-server, path-server, etc.):
//!
//! - [`EngineHandler`] — processes [`EngineCommand`]s
//! - [`ControllerHost`] — ticks entity AI controllers
//! - [`ObserverRegistry`] — spatial event routing to sessions
//! - Event coalescing — merges multiple `EntityMoved` per tick
//!
//! Concrete servers own a `BaseHandler` and delegate shared command
//! variants via [`BaseCommand`], handling only their server-specific
//! commands themselves.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::Instant;

use log::debug;

use framework::anima::{ControllerHost, EntityController};
use framework::continuum::{CommandHandler, HashContainerStore, Zone, WorldEvent, ObserverRegistry};
use framework::continuum::container::ZoneContainers;
use framework::continuum::item_props::ZoneItemProps;
use framework::ecumene::{Entity as EngineEntity, TileRect};
use u_core::{MobilePos, Pos3D};

use super::controller::{GameCommand, DemoControllerDef, EntityEvent, GumpTextEntry};
use super::entity::DemoEntity;
use super::handler::{EngineCommand, EngineHandler, UseObjectResult};
use super::serial_alloc::SerialAllocator;

// ── BaseCommand ───────────────────────────────────────────────────────────

/// Command variants shared across all server examples.
///
/// Concrete servers wrap this in their own command enum:
///
/// ```ignore
/// enum MyServerCommand {
///     Base(BaseCommand),
///     MySpecificCommand { ... },
/// }
/// ```
pub enum BaseCommand {
    /// Delegate to the underlying [`EngineHandler`].
    Engine(EngineCommand),

    /// Attach an AI controller to an entity.
    AttachController {
        serial: u32,
        controller: Box<dyn EntityController<DemoControllerDef>>,
    },

    /// Send a command to an entity's AI controller.
    ControllerCommand {
        serial: u32,
        cmd: GameCommand,
    },

    /// Register a spatial observer for a game session.
    ///
    /// On registration, the handler sends `EntitySpawned` events for all
    /// entities currently within `view_rect`, then signals `reply`.
    /// The caller should await the reply before sending LoginComplete.
    RegisterObserver {
        session_id: u32,
        map_id: u8,
        view_rect: TileRect,
        tx: tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
        reply: tokio::sync::oneshot::Sender<()>,
    },

    /// Unregister a spatial observer.
    UnregisterObserver {
        session_id: u32,
    },

    /// Update a session's view rectangle (e.g. after player movement).
    UpdateObserverView {
        session_id: u32,
        new_view_rect: TileRect,
    },

    // ── Object interaction (per-object scripted controllers) ─────────

    /// Check whether an entity has an attached controller, and if so,
    /// deliver a [`UsedBy`](EntityEvent::UsedBy) event to it.
    ///
    /// The reply tells the session whether to skip standard handling.
    UseObject {
        serial: u32,
        player_serial: u32,
        reply: tokio::sync::oneshot::Sender<UseObjectResult>,
    },

    /// Forward a gump response from a player to the object's controller.
    ///
    /// Fire-and-forget — the controller processes the response on its
    /// next tick.
    ObjectGumpResponse {
        item_serial: u32,
        player_serial: u32,
        gump_id: u32,
        button_id: u32,
        switches: Vec<u32>,
        text_entries: Vec<GumpTextEntry>,
    },
}

// ── BaseHandler ───────────────────────────────────────────────────────────

/// Shared handler infrastructure for UO server examples.
///
/// Owns the engine, AI controller host, observer registry, and event
/// drain/coalescing logic.  Concrete servers compose this into their
/// own `CommandHandler` implementation.
pub struct BaseHandler {
    pub engine: EngineHandler,
    pub host: ControllerHost<DemoControllerDef>,
    pub observer_registry: ObserverRegistry,
    /// Receiver side of the internal mpsc channel, drained in `tick()`.
    event_rx: tokio::sync::mpsc::UnboundedReceiver<WorldEvent>,
    /// Optional forward sender for Lua scripting.  When set,
    /// `drain_and_route_events` clones each event into this broadcast
    /// channel so Lua scripts can observe world events with a one-tick
    /// delay — decoupled from the critical path.
    lua_forward_tx: Option<tokio::sync::broadcast::Sender<WorldEvent>>,
    /// Pending `EntityEvent`s extracted from `WorldEvent`s during drain.
    ///
    /// `DamageDealt` → `DamageReceived` conversion: when damage is dealt
    /// to an entity with an attached controller, we queue a
    /// `DamageReceived` event here and deliver it on the next tick.
    pending_entity_events: Vec<(u32, EntityEvent)>,
    /// Serials currently carrying an *active* criminal flag, tracked so the
    /// expiry sweep can emit a single `EntityUpdated` when the flag lapses
    /// (re-colouring the mobile from gray back to blue for observers).
    flagged_criminals: std::collections::HashSet<u32>,
    /// Wall-clock of the last criminal-flag expiry sweep (epoch ms).
    last_noto_sweep_ms: u64,
}

impl BaseHandler {
    pub fn new(
        event_rx: tokio::sync::mpsc::UnboundedReceiver<WorldEvent>,
        serial_alloc: Arc<SerialAllocator>,
    ) -> Self {
        Self {
            engine: EngineHandler { serial_alloc, equipment_index: std::collections::HashMap::new() },
            host: ControllerHost::new(),
            observer_registry: ObserverRegistry::new(),
            event_rx,
            lua_forward_tx: None,
            pending_entity_events: Vec::new(),
            flagged_criminals: std::collections::HashSet::new(),
            last_noto_sweep_ms: 0,
        }
    }

    /// Set the Lua forward broadcast sender.
    ///
    /// When set, every event drained from the mpsc channel is also
    /// cloned into this broadcast channel for Lua scripts to consume.
    /// The broadcast is independent — if Lua lags, it only affects Lua.
    pub fn set_lua_forward(&mut self, tx: tokio::sync::broadcast::Sender<WorldEvent>) {
        self.lua_forward_tx = Some(tx);
    }

    /// Populate the equipment reverse index from all mobiles in a zone.
    ///
    /// Call this after directly populating a zone via `zone.spawn()` to
    /// keep the index in sync.  Not needed when entities are spawned
    /// through `EngineCommand::SpawnEntity` (the handler updates the
    /// index automatically).
    pub fn index_zone_equipment<C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &Zone<DemoEntity, C, P>,
    ) {
        for (&serial, entity) in zone.store.iter() {
            if let DemoEntity::Mobile(m) = entity {
                for eq in &m.items {
                    self.engine.equipment_index.insert(eq.serial, serial);
                }
            }
        }
    }

    /// Process a [`BaseCommand`].
    ///
    /// Handles the 6 shared command variants (Engine, AttachController,
    /// ControllerCommand, RegisterObserver, UnregisterObserver,
    /// UpdateObserverView).
    ///
    /// The `engine_intercept` closure is called for every `EngineCommand`
    /// **before** it reaches `EngineHandler`.  Return `Some(cmd)` to let
    /// the engine process it, or `None` to consume it (e.g. for
    /// `GetItemProps` / `SetItemProps` interception in demo-server).
    pub fn handle_base_command<P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<DemoEntity, HashContainerStore, P>,
        cmd: BaseCommand,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
        engine_intercept: &mut dyn FnMut(EngineCommand, &mut Zone<DemoEntity, HashContainerStore, P>) -> Option<EngineCommand>,
    ) where
        P::Value: 'static,
    {
        match cmd {
            BaseCommand::Engine(engine_cmd) => {
                // Detach any controller before the entity is removed from
                // the zone — otherwise the controller outlives its entity
                // and may be triggered by a new entity reusing the serial.
                if let EngineCommand::RemoveEntity { entity_id } = &engine_cmd {
                    self.host.detach(*entity_id);
                }

                // Remember if this command moves a mobile — we need to
                // deliver `SteppedOnBy` to item controllers at the
                // destination tile after the engine processes the step.
                let step_serial = match &engine_cmd {
                    EngineCommand::MobileStep { serial, .. }
                    | EngineCommand::TeleportEntity { serial, .. } => Some(*serial),
                    _ => None,
                };

                if let Some(cmd) = engine_intercept(engine_cmd, zone) {
                    self.engine.handle(zone, cmd, event_tx);
                }

                // ── Step-on triggers ────────────────────────────────
                // If a mobile just moved, scan the destination tile for
                // items with controllers and deliver SteppedOnBy events.
                if let Some(serial) = step_serial {
                    self.process_step_on_triggers(serial, zone, event_tx);
                }
            }

            BaseCommand::AttachController { serial, controller } => {
                self.host.attach(serial, controller, zone.map_id);
            }

            BaseCommand::ControllerCommand { serial, cmd } => {
                self.host.send_command_with_events(zone, serial, cmd, event_tx);
            }

            BaseCommand::RegisterObserver {
                session_id, map_id, view_rect, tx, reply,
            } => {
                self.observer_registry.register(session_id, map_id, view_rect, tx);

                // Stream initial entities in view.
                let entities = zone.query_area(&view_rect);
                for entity in entities {
                    let serial = entity.serial();
                    let pos = entity.pos();
                    let snapshot = entity.snapshot();
                    let event = Arc::new(WorldEvent::EntitySpawned {
                        map_id: zone.map_id,
                        serial,
                        pos,
                        entity: snapshot,
                    });
                    self.observer_registry.send_to_session(session_id, event);
                }

                let _ = reply.send(());
            }

            BaseCommand::UnregisterObserver { session_id } => {
                self.observer_registry.unregister(session_id);
            }

            BaseCommand::UpdateObserverView { session_id, new_view_rect } => {
                if let Some((strips_added, strips_removed)) =
                    self.observer_registry.update_view(session_id, new_view_rect)
                {
                    // Send EntitySpawned for newly visible strips.
                    for strip in &strips_added {
                        let entities = zone.query_area(strip);
                        for entity in entities {
                            let serial = entity.serial();
                            if serial == session_id { continue; }
                            let pos = entity.pos();
                            let snapshot = entity.snapshot();
                            let event = Arc::new(WorldEvent::EntitySpawned {
                                map_id: zone.map_id,
                                serial,
                                pos,
                                entity: snapshot,
                            });
                            self.observer_registry.send_to_session(session_id, event);
                        }
                    }

                    // Send EntityRemoved for no-longer-visible strips.
                    for strip in &strips_removed {
                        let entities = zone.query_area(strip);
                        for entity in entities {
                            let serial = entity.serial();
                            if serial == session_id { continue; }
                            let pos = entity.pos();
                            let event = Arc::new(WorldEvent::EntityRemoved {
                                map_id: zone.map_id,
                                serial,
                                last_pos: pos,
                            });
                            self.observer_registry.send_to_session(session_id, event);
                        }
                    }
                }
            }

            // ── Object interaction ───────────────────────────────────────

            BaseCommand::UseObject { serial, player_serial, reply } => {
                if self.host.has_controller(serial) {
                    self.host.send_event_with_events(
                        zone, serial,
                        EntityEvent::UsedBy { player_serial },
                        event_tx,
                    );
                    let _ = reply.send(UseObjectResult::HandledByController);
                } else {
                    let _ = reply.send(UseObjectResult::NotScripted);
                }
            }

            BaseCommand::ObjectGumpResponse {
                item_serial, player_serial, gump_id, button_id,
                switches, text_entries,
            } => {
                self.host.send_event_with_events(
                    zone, item_serial,
                    EntityEvent::GumpResponse {
                        player_serial,
                        gump_id,
                        button_id,
                        switches,
                        text_entries,
                    },
                    event_tx,
                );
            }
        }
    }

    /// Tick AI controllers and drain/route events.
    ///
    /// Call this from your `CommandHandler::tick()` implementation.
    pub fn tick<P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<DemoEntity, HashContainerStore, P>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where
        P::Value: 'static,
    {
        self.host.tick_with_observer(zone, Instant::now(), event_tx, &mut self.observer_registry);
        self.drain_and_route_events();

        // Deliver pending EntityEvents (DamageDealt → DamageReceived, etc.)
        // that were collected during drain.  These are dispatched *after*
        // the world events have been routed so that controllers can react
        // to damage in the same tick.
        if !self.pending_entity_events.is_empty() {
            let events: Vec<(u32, EntityEvent)> =
                std::mem::take(&mut self.pending_entity_events);
            for (target_serial, entity_event) in events {
                self.host.send_event_with_events(
                    zone, target_serial, entity_event, event_tx,
                );
            }
        }

        // Sweep expiring criminal flags so observers re-colour mobiles back
        // to innocent (~once per second is plenty for a 2-minute flag).
        self.sweep_criminal_flags(zone, event_tx);

        // Apply pending poison ticks (damage / expiry) for all poisoned
        // mobiles in the zone — players and NPCs alike.
        self.sweep_poison(zone, event_tx);
    }

    /// Apply poison damage ticks and expire finished poisons for every
    /// poisoned mobile in the zone.
    ///
    /// For each mobile with `poison_level > 0`:
    /// - If the poison has expired, clear it (flag + `EntityUpdated`).
    /// - Otherwise, while a tick is due, deal `poison_damage_per_tick`
    ///   damage (reusing the standard damage/kill path) and schedule the
    ///   next tick.
    ///
    /// Runs every worker tick; cheap because it only acts on mobiles that
    /// are actually poisoned and whose next-tick time has been reached.
    fn sweep_poison<P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<DemoEntity, HashContainerStore, P>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where
        P::Value: 'static,
    {
        let now = crate::uo_engine::entity::MobileData::now_epoch_ms();

        // Collect serials needing work first (avoid holding a borrow over the
        // mutating damage/kill path).  `(serial, expired, due_ticks, dmg, src)`.
        struct PoisonWork {
            serial: u32,
            expired: bool,
            ticks_due: u32,
            damage_per_tick: u16,
            source: u32,
        }
        let mut work: Vec<PoisonWork> = Vec::new();

        for (&serial, entity) in zone.store.iter() {
            let DemoEntity::Mobile(m) = entity else { continue };
            if m.poison_level == 0 {
                continue;
            }
            if now >= m.poison_until_ms {
                work.push(PoisonWork {
                    serial, expired: true, ticks_due: 0,
                    damage_per_tick: 0, source: m.poison_source,
                });
                continue;
            }
            if m.poison_tick_interval_ms > 0 && now >= m.poison_next_tick_ms {
                // Number of ticks elapsed since the scheduled tick (usually 1).
                let elapsed = now.saturating_sub(m.poison_next_tick_ms);
                let ticks = 1 + (elapsed / m.poison_tick_interval_ms) as u32;
                work.push(PoisonWork {
                    serial, expired: false, ticks_due: ticks,
                    damage_per_tick: m.poison_damage_per_tick,
                    source: m.poison_source,
                });
            }
        }

        for w in work {
            if w.expired {
                // Clear poison state and re-broadcast.
                if let Some(m) = zone.store.get_mut(w.serial).and_then(|e| e.mobile_mut()) {
                    m.poison_level = 0;
                    m.poison_until_ms = 0;
                    m.poison_next_tick_ms = 0;
                    m.poison_damage_per_tick = 0;
                    m.poison_tick_interval_ms = 0;
                    m.poison_source = 0;
                    m.status = m.status.with_poisoned(false);
                    let pos = EngineEntity::pos(zone.store.get(w.serial).unwrap());
                    let snap = zone.get(w.serial).and_then(|e| e.snapshot());
                    let _ = event_tx.send(WorldEvent::EntityUpdated {
                        map_id: zone.map_id,
                        serial: w.serial,
                        pos,
                        entity: snap,
                    });
                }
                continue;
            }

            // Advance the next-tick schedule first so a kill mid-loop does
            // not leave a stale timer.
            let mut still_alive = true;
            if let Some(m) = zone.store.get_mut(w.serial).and_then(|e| e.mobile_mut()) {
                m.poison_next_tick_ms = now + m.poison_tick_interval_ms;
            }

            // Deal one tick of damage per elapsed interval (capped to avoid a
            // burst after a long stall).
            let ticks = w.ticks_due.min(4);
            for _ in 0..ticks {
                if !still_alive {
                    break;
                }
                if let Some(result) = self.engine.deal_damage_with_kill(
                    zone, event_tx, w.serial, w.damage_per_tick, w.source,
                ) {
                    if result.killed {
                        still_alive = false;
                    }
                } else {
                    still_alive = false;
                }
            }
            // If a player-vs-player poison kill occurred, record the murder.
            if !still_alive && w.source != 0 {
                // Re-use the standard reputation path.
                debug!("[poison] 0x{:08X} died from poison (source 0x{:08X})", w.serial, w.source);
            }
        }
    }


    /// Re-broadcast mobiles whose criminal flag just expired so observers
    /// stop rendering them gray.  Throttled to ~1 Hz.
    fn sweep_criminal_flags<P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<DemoEntity, HashContainerStore, P>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where
        P::Value: 'static,
    {
        use crate::uo_engine::notoriety::NotorietyClass;
        let now = crate::uo_engine::entity::MobileData::now_epoch_ms();
        if now.saturating_sub(self.last_noto_sweep_ms) < 1000 {
            return;
        }
        self.last_noto_sweep_ms = now;

        // Collect the current set of effectively-criminal serials.
        let mut current: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for (&serial, entity) in zone.store.iter() {
            if let DemoEntity::Mobile(m) = entity {
                if m.effective_notoriety_class() == NotorietyClass::Criminal {
                    current.insert(serial);
                }
            }
        }

        // Anyone previously flagged but no longer criminal → re-broadcast.
        let expired: Vec<u32> = self
            .flagged_criminals
            .difference(&current)
            .copied()
            .collect();
        for serial in expired {
            if let Some(entity) = zone.store.get(serial) {
                let pos = framework::ecumene::Entity::pos(entity);
                let snap = entity.snapshot();
                let _ = event_tx.send(WorldEvent::EntityUpdated {
                    map_id: zone.map_id,
                    serial,
                    pos,
                    entity: snap,
                });
            }
        }

        self.flagged_criminals = current;
    }

    /// Flush pending events immediately (without ticking controllers).
    ///
    /// Unlike [`Self::drain_and_route_events`] this does **not** coalesce
    /// `EntityMoved` events — every move is delivered individually so
    /// clients see smooth step-by-step animation.  Intended for use in
    /// `CommandHandler::post_command()` where commands arrive one at a
    /// time and coalescing would merge distinct player steps into jumps.
    pub fn flush_events(&mut self) {
        self.drain_and_route_events_raw();
    }

    /// Return the earliest [`Instant`] at which the host needs a tick.
    ///
    /// Aggregates the minimum of the scheduler's next fire time and
    /// all attached controllers' requested wake times.  Returns `None`
    /// when nothing needs ticking.
    pub fn next_tick_at(&mut self) -> Option<Instant> {
        self.host.next_tick_at()
    }

    /// Detach controller for an entity being transferred out.
    ///
    /// Returns the controller boxed as `Any` so it can be passed through
    /// the framework's `on_entity_leaving_zone` hook.
    pub fn detach_controller_for_transfer(
        &mut self,
        serial: u32,
    ) -> Option<Box<dyn std::any::Any + Send>> {
        self.host.detach(serial)
            .map(|c| -> Box<dyn std::any::Any + Send> { Box::new(c) })
    }

    /// Re-attach a controller for an entity arriving via transfer.
    ///
    /// `state` should be the value returned by
    /// [`Self::detach_controller_for_transfer`] on the source side.
    /// `map_id` is the destination zone the entity is entering, so the
    /// controller (and any pending timers) are re-stamped to the new world.
    pub fn attach_controller_from_transfer(
        &mut self,
        serial: u32,
        state: Option<Box<dyn std::any::Any + Send>>,
        map_id: u8,
    ) {
        if let Some(boxed) = state {
            if let Ok(controller) = boxed.downcast::<Box<dyn framework::anima::EntityController<DemoControllerDef>>>() {
                self.host.attach(serial, *controller, map_id);
            }
        }
    }

    /// Drain all pending events and route each one individually,
    /// **without** coalescing `EntityMoved` events.
    ///
    /// Used by [`flush_events`] (post-command path) where every move
    /// must be delivered so clients see step-by-step animation.
    fn drain_and_route_events_raw(&mut self) {
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    if let Some(ref lua_tx) = self.lua_forward_tx {
                        let _ = lua_tx.send(event.clone());
                    }
                    // Convert DamageDealt → EntityEvent::DamageReceived
                    // for the target entity's controller.
                    if let WorldEvent::DamageDealt { serial, source_serial, amount, .. } = &event {
                        if self.host.has_controller(*serial) {
                            self.pending_entity_events.push((
                                *serial,
                                EntityEvent::DamageReceived {
                                    source_serial: *source_serial,
                                    amount: *amount,
                                },
                            ));
                        }
                    }
                    self.observer_registry.route_event(Arc::new(event));
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Drain all pending events from the mpsc channel, coalesce
    /// multiple `EntityMoved` events for the same serial (keeping only
    /// the last position), and route the result through the observer
    /// registry.
    ///
    /// Coalescing is the key optimisation for dense areas: if an entity
    /// moved multiple times within a single tick, observers only need
    /// to see the final position.  The `old_pos` of the coalesced event
    /// is taken from the *first* move so that enter/leave-view checks
    /// remain correct.
    pub fn drain_and_route_events(&mut self) {
        let mut coalesced_moves: HashMap<u32, WorldEvent> = HashMap::new();
        let mut move_first_old: HashMap<u32, MobilePos> = HashMap::new();
        let mut move_has_teleport: HashMap<u32, bool> = HashMap::new();
        let mut other_events: Vec<WorldEvent> = Vec::new();
        let mut event_order: Vec<CoalescedEntry> = Vec::new();
        let mut container_event_count: usize = 0;

        loop {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    // Forward to Lua broadcast if configured.
                    if let Some(ref lua_tx) = self.lua_forward_tx {
                        let _ = lua_tx.send(event.clone());
                    }
                    // Convert DamageDealt → EntityEvent::DamageReceived
                    // for the target entity's controller.
                    if let WorldEvent::DamageDealt { serial, source_serial, amount, .. } = &event {
                        if self.host.has_controller(*serial) {
                            self.pending_entity_events.push((
                                *serial,
                                EntityEvent::DamageReceived {
                                    source_serial: *source_serial,
                                    amount: *amount,
                                },
                            ));
                        }
                    }
                    // Track container events specifically
                    if matches!(&event, WorldEvent::ContainerContentsUpdated { .. }) {
                        container_event_count += 1;
                    }
                    if let WorldEvent::EntityMoved { serial, old_pos, is_teleport, .. } = &event {
                        let serial = *serial;
                        if coalesced_moves.contains_key(&serial) {
                            // Preserve teleport flag: once set, stays set.
                            if *is_teleport {
                                move_has_teleport.insert(serial, true);
                            }
                            coalesced_moves.insert(serial, event);
                        } else {
                            move_first_old.insert(serial, *old_pos);
                            move_has_teleport.insert(serial, *is_teleport);
                            coalesced_moves.insert(serial, event);
                            event_order.push(CoalescedEntry::Move(serial));
                        }
                    } else {
                        let idx = other_events.len();
                        other_events.push(event);
                        event_order.push(CoalescedEntry::Other(idx));
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }

        if container_event_count > 0 {
            debug!(
                "[drain_events] routing {} container event(s), {} other events, {} coalesced moves",
                container_event_count, other_events.len(), coalesced_moves.len(),
            );
        }

        for entry in event_order {
            match entry {
                CoalescedEntry::Move(serial) => {
                    if let Some(mut event) = coalesced_moves.remove(&serial) {
                        if let WorldEvent::EntityMoved { old_pos, is_teleport, .. } = &mut event {
                            if let Some(first_old) = move_first_old.get(&serial) {
                                *old_pos = *first_old;
                            }
                            // If any coalesced move was a teleport, mark the
                            // merged event as teleport so sessions send
                            // DrawGamePlayer instead of silently skipping it.
                            if let Some(true) = move_has_teleport.get(&serial) {
                                *is_teleport = true;
                            }
                        }
                        self.observer_registry.route_event(Arc::new(event));
                    }
                }
                CoalescedEntry::Other(idx) => {
                    let event = std::mem::replace(
                        &mut other_events[idx],
                        WorldEvent::EntityRemoved {
                            map_id: 0,
                            serial: 0,
                            last_pos: Pos3D { x: 0, y: 0, z: 0 },
                        },
                    );
                    self.observer_registry.route_event(Arc::new(event));
                }
            }
        }
    }

    // ── Step-on trigger scan ──────────────────────────────────────────

    /// After a mobile moves (step or teleport), scan the destination tile
    /// for items that have an attached controller and deliver
    /// [`EntityEvent::SteppedOnBy`] to each.
    ///
    /// This enables teleporters, traps, pressure plates, and similar
    /// triggered items — all driven by per-item Lua/Rust controllers.
    fn process_step_on_triggers<P: ZoneItemProps>(
        &mut self,
        mobile_serial: u32,
        zone: &mut Zone<DemoEntity, HashContainerStore, P>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where
        P::Value: 'static,
    {
        // Look up the mobile's current position.  If the entity doesn't
        // exist (e.g. removed between command queuing and execution), bail.
        let pos = match zone.get(mobile_serial) {
            Some(e) if e.is_mobile() => e.pos(),
            _ => return,
        };

        // Query all entities on the same tile.
        let tile = TileRect::point(pos.x, pos.y);
        let entities = zone.query_area(&tile);

        // Collect serials of items with controllers on this tile.
        // We collect first to avoid borrowing conflicts with
        // `host.send_event_with_events` which needs `&mut self`.
        let triggered: Vec<u32> = entities
            .iter()
            .filter(|e| !e.is_mobile() && !e.is_multi())
            .filter(|e| self.host.has_controller(e.serial()))
            .map(|e| e.serial())
            .collect();

        for item_serial in triggered {
            self.host.send_event_with_events(
                zone,
                item_serial,
                EntityEvent::SteppedOnBy { mobile_serial },
                event_tx,
            );
        }
    }
}

enum CoalescedEntry {
    Move(u32),
    Other(usize),
}
