//! Spatial event routing: unified observer registry with block-based
//! spatial indexing.
//!
//! Routes [`WorldEvent`]s to two kinds of subscribers:
//!
//! - **Sessions** (player clients) — receive events via async `mpsc` channel.
//! - **Controllers** (entity AI) — receive events into a synchronous buffer,
//!   drained on the next tick by [`ControllerHost`](crate::anima::ControllerHost).
//!
//! Both subscriber types use the same spatial index for O(1) "who can see
//! position (x, y)?" lookups via 8×8 block buckets.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use log::{debug, warn};
use tokio::sync::mpsc;
use u_core::BlockKey;

use super::world_event::WorldEvent;
use crate::ecumene::tile_rect::TileRect;

/// Observer identifier — a `u32` serial (player serial for sessions,
/// entity serial for controllers).
pub type ObserverId = u32;

// ── Spatial index (internal) ─────────────────────────────────────────────

/// Block-based spatial index for observer subscriptions.
///
/// Maps each 8×8 block to the set of observers whose watch rectangle
/// overlaps that block.
#[derive(Clone, Default)]
struct SpatialIndex {
    blocks: HashMap<BlockKey, Vec<ObserverId>>,
    reverse: HashMap<ObserverId, Vec<BlockKey>>,
}

impl SpatialIndex {
    fn new() -> Self { Self::default() }

    fn insert(&mut self, id: ObserverId, rect: &TileRect) {
        let bx_min = rect.x_min / BlockKey::BLOCK_SIZE;
        let bx_max = rect.x_max / BlockKey::BLOCK_SIZE;
        let by_min = rect.y_min / BlockKey::BLOCK_SIZE;
        let by_max = rect.y_max / BlockKey::BLOCK_SIZE;

        let mut keys = Vec::with_capacity(
            ((bx_max - bx_min + 1) * (by_max - by_min + 1)) as usize,
        );
        for bx in bx_min..=bx_max {
            for by in by_min..=by_max {
                let key = BlockKey::new(bx, by);
                self.blocks.entry(key).or_default().push(id);
                keys.push(key);
            }
        }
        self.reverse.insert(id, keys);
    }

    fn remove(&mut self, id: ObserverId) {
        if let Some(keys) = self.reverse.remove(&id) {
            for key in keys {
                if let Some(ids) = self.blocks.get_mut(&key) {
                    ids.retain(|&i| i != id);
                    if ids.is_empty() {
                        self.blocks.remove(&key);
                    }
                }
            }
        }
    }

    fn query_point(&self, x: u16, y: u16) -> Vec<ObserverId> {
        let key = BlockKey::from_tile(x, y);
        match self.blocks.get(&key) {
            Some(ids) => ids.clone(),
            None => Vec::new(),
        }
    }

    #[allow(dead_code)]
    fn query_rect(&self, rect: &TileRect) -> Vec<ObserverId> {
        let bx_min = rect.x_min / BlockKey::BLOCK_SIZE;
        let bx_max = rect.x_max / BlockKey::BLOCK_SIZE;
        let by_min = rect.y_min / BlockKey::BLOCK_SIZE;
        let by_max = rect.y_max / BlockKey::BLOCK_SIZE;

        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for bx in bx_min..=bx_max {
            for by in by_min..=by_max {
                let key = BlockKey::new(bx, by);
                if let Some(ids) = self.blocks.get(&key) {
                    for &id in ids {
                        if seen.insert(id) {
                            result.push(id);
                        }
                    }
                }
            }
        }
        result
    }
}

// ── Event target abstraction ─────────────────────────────────────────────

/// Maximum number of buffered world events for a controller subscription.
const CONTROLLER_BUFFER_CAP: usize = 256;

/// How an observer receives events — async channel or synchronous buffer.
enum EventTarget {
    /// Session observer — events sent via bounded async mpsc channel.
    Channel(mpsc::Sender<Arc<WorldEvent>>),
    /// Controller observer — events buffered for synchronous drain.
    Buffer(VecDeque<Arc<WorldEvent>>),
}

/// Per-observer state.
struct ObserverEntry {
    target: EventTarget,
    view_rect: TileRect,
    map_id: u8,
}

// ── ObserverRegistry ─────────────────────────────────────────────────────

/// Unified registry for session and controller observers with spatial
/// event routing.
///
/// Stores per-map spatial indices and routes [`WorldEvent`]s to only those
/// observers whose watch rectangle covers the event's position.
pub struct ObserverRegistry {
    /// Per-map spatial indices.
    spatial: HashMap<u8, SpatialIndex>,
    /// All observers keyed by id.
    observers: HashMap<ObserverId, ObserverEntry>,
    /// Cumulative count of events dropped due to full channels.
    events_dropped: u64,
    /// Value of `events_dropped` at the last warning log.
    last_drop_warn: u64,
}

impl ObserverRegistry {
    pub fn new() -> Self {
        Self {
            spatial: HashMap::new(),
            observers: HashMap::new(),
            events_dropped: 0,
            last_drop_warn: 0,
        }
    }

    // ── Session registration (async channel) ──────────────────────────

    /// Register a session observer (player client).
    ///
    /// Events within `view_rect` on `map_id` are sent via the `tx` channel.
    pub fn register(
        &mut self,
        session_id: ObserverId,
        map_id: u8,
        view_rect: TileRect,
        tx: mpsc::Sender<Arc<WorldEvent>>,
    ) {
        // Remove any existing registration first.
        self.unregister(session_id);

        let spatial = self.spatial.entry(map_id).or_insert_with(SpatialIndex::new);
        spatial.insert(session_id, &view_rect);

        self.observers.insert(session_id, ObserverEntry {
            target: EventTarget::Channel(tx),
            view_rect,
            map_id,
        });

        debug!(
            "[observer] registered session {:#010X} on map {} (view: ({},{})-({},{}))",
            session_id, map_id,
            view_rect.x_min, view_rect.y_min,
            view_rect.x_max, view_rect.y_max,
        );
    }

    /// Unregister any observer (session or controller).
    pub fn unregister(&mut self, id: ObserverId) {
        if let Some(entry) = self.observers.remove(&id) {
            if let Some(spatial) = self.spatial.get_mut(&entry.map_id) {
                spatial.remove(id);
            }
            debug!("[observer] unregistered observer {:#010X}", id);
        }
    }

    /// Update a session's view rectangle.
    ///
    /// Returns `(strips_added, strips_removed)` for edge-update processing.
    pub fn update_view(
        &mut self,
        session_id: ObserverId,
        new_view_rect: TileRect,
    ) -> Option<(Vec<TileRect>, Vec<TileRect>)> {
        let entry = self.observers.get_mut(&session_id)?;
        let old_rect = entry.view_rect;

        if old_rect == new_view_rect {
            return Some((Vec::new(), Vec::new()));
        }

        let map_id = entry.map_id;

        let strips_added = new_view_rect.difference(&old_rect);
        let strips_removed = old_rect.difference(&new_view_rect);

        if let Some(spatial) = self.spatial.get_mut(&map_id) {
            spatial.remove(session_id);
            spatial.insert(session_id, &new_view_rect);
        }

        entry.view_rect = new_view_rect;

        Some((strips_added, strips_removed))
    }

    // ── Controller subscription (synchronous buffer) ──────────────────

    /// Register a controller observer for an entity.
    ///
    /// World events within `watch_rect` on `map_id` are buffered and can
    /// be drained via [`drain_controller_events`](Self::drain_controller_events).
    pub fn subscribe_controller(
        &mut self,
        entity_serial: ObserverId,
        map_id: u8,
        watch_rect: TileRect,
    ) {
        // Remove previous subscription if any.
        self.unsubscribe_controller(entity_serial);

        let spatial = self.spatial.entry(map_id).or_insert_with(SpatialIndex::new);
        spatial.insert(entity_serial, &watch_rect);

        self.observers.insert(entity_serial, ObserverEntry {
            target: EventTarget::Buffer(VecDeque::new()),
            view_rect: watch_rect,
            map_id,
        });

        debug!(
            "[observer] subscribed controller {:#010X} on map {} (watch: ({},{})-({},{}))",
            entity_serial, map_id,
            watch_rect.x_min, watch_rect.y_min,
            watch_rect.x_max, watch_rect.y_max,
        );
    }

    /// Unsubscribe a controller observer.
    pub fn unsubscribe_controller(&mut self, entity_serial: ObserverId) {
        // Only remove if it's a Buffer-type entry (don't accidentally
        // remove a session).
        if let Some(entry) = self.observers.get(&entity_serial) {
            if matches!(&entry.target, EventTarget::Buffer(_)) {
                self.unregister(entity_serial);
            }
        }
    }

    /// Whether the given entity has an active controller subscription.
    pub fn has_controller_subscription(&self, entity_serial: ObserverId) -> bool {
        self.observers.get(&entity_serial)
            .map_or(false, |e| matches!(&e.target, EventTarget::Buffer(_)))
    }

    /// Update a controller's watch rectangle (e.g. after entity movement).
    ///
    /// Called internally by the system when a subscribed entity moves.
    pub fn update_controller_watch(
        &mut self,
        entity_serial: ObserverId,
        new_watch_rect: TileRect,
    ) {
        let entry = match self.observers.get_mut(&entity_serial) {
            Some(e) if matches!(&e.target, EventTarget::Buffer(_)) => e,
            _ => return,
        };

        if entry.view_rect == new_watch_rect {
            return;
        }

        let map_id = entry.map_id;

        if let Some(spatial) = self.spatial.get_mut(&map_id) {
            spatial.remove(entity_serial);
            spatial.insert(entity_serial, &new_watch_rect);
        }

        entry.view_rect = new_watch_rect;
    }

    /// Drain all buffered world events for a controller.
    ///
    /// Returns the events accumulated since the last drain.
    pub fn drain_controller_events(
        &mut self,
        entity_serial: ObserverId,
    ) -> Vec<Arc<WorldEvent>> {
        let entry = match self.observers.get_mut(&entity_serial) {
            Some(e) => e,
            None => return Vec::new(),
        };

        match &mut entry.target {
            EventTarget::Buffer(buf) => buf.drain(..).collect(),
            EventTarget::Channel(_) => Vec::new(),
        }
    }

    /// Get the radius stored for a controller subscription, if any.
    ///
    /// Returns `None` if the entity has no controller subscription.
    pub fn get_controller_subscription_radius(
        &self,
        entity_serial: ObserverId,
    ) -> Option<u16> {
        let entry = self.observers.get(&entity_serial)?;
        if !matches!(&entry.target, EventTarget::Buffer(_)) {
            return None;
        }
        // Derive radius from the stored rect (half-width).
        let rx = (entry.view_rect.x_max - entry.view_rect.x_min) / 2;
        let ry = (entry.view_rect.y_max - entry.view_rect.y_min) / 2;
        Some(rx.min(ry))
    }

    /// Get serials of all controllers that have buffered events pending.
    pub fn controllers_with_pending_events(&self) -> Vec<ObserverId> {
        self.observers.iter()
            .filter_map(|(&id, entry)| {
                match &entry.target {
                    EventTarget::Buffer(buf) if !buf.is_empty() => Some(id),
                    _ => None,
                }
            })
            .collect()
    }

    // ── Event routing ─────────────────────────────────────────────────

    /// Route a world event to all observers (sessions and controllers)
    /// that can see the event's position.
    pub fn route_event(&mut self, event: Arc<WorldEvent>) {
        match event.as_ref() {
            WorldEvent::EntityMoved {
                map_id, old_pos, new_pos, ..
            } => {
                let ids = if let Some(spatial) = self.spatial.get(map_id) {
                    let mut notified = HashSet::new();
                    let mut result = Vec::new();
                    for id in spatial.query_point(old_pos.x, old_pos.y) {
                        if notified.insert(id) {
                            result.push(id);
                        }
                    }
                    for id in spatial.query_point(new_pos.x, new_pos.y) {
                        if notified.insert(id) {
                            result.push(id);
                        }
                    }
                    result
                } else {
                    return;
                };
                for id in ids {
                    self.deliver(id, &event);
                }
            }

            WorldEvent::ShipMoved {
                map_id, ship_old_pos, ship_new_pos, passengers, cargo, ..
            } => {
                // Deliver atomically to every observer near the hull's old or
                // new origin, plus every passenger's and every cargo item's old
                // or new tile, so no one at the edge of view misses the hull
                // redraw, a passenger snap, or a carried item.
                let ids = if let Some(spatial) = self.spatial.get(map_id) {
                    let mut notified = HashSet::new();
                    let mut result = Vec::new();
                    let mut points: Vec<(u16, u16)> =
                        Vec::with_capacity(2 + (passengers.len() + cargo.len()) * 2);
                    points.push((ship_old_pos.x, ship_old_pos.y));
                    points.push((ship_new_pos.x, ship_new_pos.y));
                    for (_, old_pos, new_pos, _) in passengers {
                        points.push((old_pos.x, old_pos.y));
                        points.push((new_pos.x, new_pos.y));
                    }
                    for (_, old_pos, new_pos, _) in cargo {
                        points.push((old_pos.x, old_pos.y));
                        points.push((new_pos.x, new_pos.y));
                    }
                    for (px, py) in points {
                        for id in spatial.query_point(px, py) {
                            if notified.insert(id) {
                                result.push(id);
                            }
                        }
                    }
                    result
                } else {
                    return;
                };
                for id in ids {
                    self.deliver(id, &event);
                }
            }

            WorldEvent::EntitySpawned { map_id, pos, .. } => {
                self.broadcast_at_point(*map_id, pos.x, pos.y, u32::MAX, &event);
            }

            WorldEvent::EntityRemoved { map_id, last_pos, .. } => {
                self.broadcast_at_point(*map_id, last_pos.x, last_pos.y, u32::MAX, &event);
            }

            WorldEvent::EntityUpdated { map_id, pos, .. } => {
                self.broadcast_at_point(*map_id, pos.x, pos.y, u32::MAX, &event);
            }

            WorldEvent::SoundPlayed { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::EffectPlayed { map_id, x, y, target_x, target_y, .. } => {
                let ids = if let Some(spatial) = self.spatial.get(map_id) {
                    let mut notified = HashSet::new();
                    let mut result = Vec::new();
                    for id in spatial.query_point(*x, *y) {
                        if notified.insert(id) {
                            result.push(id);
                        }
                    }
                    for id in spatial.query_point(*target_x, *target_y) {
                        if notified.insert(id) {
                            result.push(id);
                        }
                    }
                    result
                } else {
                    return;
                };
                for id in ids {
                    self.deliver(id, &event);
                }
            }

            WorldEvent::AnimationPlayed { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::Speech { map_id, serial, x, y, .. } => {
                if *serial == 0xFFFF_FFFF {
                    self.broadcast_to_map(*map_id, &event);
                } else {
                    self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
                }
            }

            WorldEvent::GlobalLight { map_id, .. }
            | WorldEvent::Weather { map_id, .. }
            | WorldEvent::Season { map_id, .. }
            | WorldEvent::Music { map_id, .. } => {
                self.broadcast_to_map(*map_id, &event);
            }

            WorldEvent::MobileKilled { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::PlayerDied { map_id, x, y, .. }
            | WorldEvent::PlayerResurrected { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::GhostVisibilityChanged { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::DamageDealt { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::MobileHealed { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::ManaStaminaChanged { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::BaseStatChanged { map_id, x, y, .. } => {
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            WorldEvent::ContainerContentsUpdated { map_id, container_serial, x, y, changes, .. } => {
                let ids: Vec<ObserverId> = if let Some(spatial) = self.spatial.get(map_id) {
                    spatial.query_point(*x, *y)
                } else {
                    Vec::new()
                };
                debug!(
                    "[observer] routing ContainerContentsUpdated: container=0x{:08X}, \
                     pos=({},{}), map={}, {} change(s), {} recipient(s): [{}]",
                    container_serial, x, y, map_id, changes.len(), ids.len(),
                    ids.iter().map(|s| format!("0x{:08X}", s)).collect::<Vec<_>>().join(", "),
                );
                self.broadcast_at_point(*map_id, *x, *y, u32::MAX, &event);
            }

            // Targeted events — routed directly to one observer.
            WorldEvent::TargetedGump { target_player, .. }
            | WorldEvent::TargetedMessage { target_player, .. }
            | WorldEvent::TargetedCloseGump { target_player, .. }
            | WorldEvent::TargetedTargetCursor { target_player, .. }
            | WorldEvent::TargetedCrossWorldTeleport { target_player, .. } => {
                self.deliver(*target_player, &event);
            }

            // Internal events — not routed.
            WorldEvent::SnapshotRestored { .. } => {}
        }
    }

    /// Get a session's current view rectangle.
    #[allow(dead_code)]
    pub fn get_view_rect(&self, session_id: ObserverId) -> Option<TileRect> {
        self.observers.get(&session_id).map(|e| e.view_rect)
    }

    /// Send an event directly to a specific observer (for edge updates).
    pub fn send_to_session(&mut self, session_id: ObserverId, event: Arc<WorldEvent>) {
        self.deliver(session_id, &event);
    }

    // ── Internal delivery ─────────────────────────────────────────────

    /// Deliver event to a single observer (session or controller).
    fn deliver(&mut self, id: ObserverId, event: &Arc<WorldEvent>) -> bool {
        let entry = match self.observers.get_mut(&id) {
            Some(e) => e,
            None => return false,
        };

        match &mut entry.target {
            EventTarget::Channel(tx) => {
                match tx.try_send(Arc::clone(event)) {
                    Ok(()) => false,
                    Err(err) => {
                        use tokio::sync::mpsc::error::TrySendError;
                        match err {
                            TrySendError::Closed(_) => {
                                // Eagerly unregister disconnected session.
                                self.unregister(id);
                                debug!("[observer] eagerly unregistered closed session {:#010X}", id);
                                true
                            }
                            TrySendError::Full(_) => {
                                self.events_dropped += 1;
                                let is_container = matches!(event.as_ref(), WorldEvent::ContainerContentsUpdated { .. });
                                if is_container {
                                    if let Some(entry) = self.observers.get(&id) {
                                        if let EventTarget::Channel(tx) = &entry.target {
                                            warn!(
                                                "[observer] DROPPED ContainerContentsUpdated for session=0x{:08X} \
                                                 (channel capacity={}, total drops={})",
                                                id, tx.max_capacity(), self.events_dropped,
                                            );
                                        }
                                    }
                                }
                                if self.events_dropped - self.last_drop_warn >= 1000 {
                                    warn!(
                                        "[observer] {} events dropped total (channel full)",
                                        self.events_dropped
                                    );
                                    self.last_drop_warn = self.events_dropped;
                                }
                                false
                            }
                        }
                    }
                }
            }
            EventTarget::Buffer(buf) => {
                // Drop oldest if buffer is full.
                if buf.len() >= CONTROLLER_BUFFER_CAP {
                    buf.pop_front();
                }
                buf.push_back(Arc::clone(event));
                false
            }
        }
    }

    /// Broadcast event to all observers on `map_id` that can see `(x, y)`,
    /// skipping `exclude_id`.
    fn broadcast_at_point(
        &mut self,
        map_id: u8,
        x: u16,
        y: u16,
        exclude_id: ObserverId,
        event: &Arc<WorldEvent>,
    ) {
        let ids: Vec<ObserverId> = if let Some(spatial) = self.spatial.get(&map_id) {
            spatial.query_point(x, y)
        } else {
            return;
        };
        for id in ids {
            if id == exclude_id { continue; }
            self.deliver(id, event);
        }
    }

    /// Broadcast event to ALL observers on a given map.
    fn broadcast_to_map(&mut self, map_id: u8, event: &Arc<WorldEvent>) {
        let ids: Vec<ObserverId> = self.observers.iter()
            .filter(|(_, e)| e.map_id == map_id)
            .map(|(&id, _)| id)
            .collect();
        for id in ids {
            self.deliver(id, event);
        }
    }
}

impl Default for ObserverRegistry {
    fn default() -> Self {
        Self::new()
    }
}
