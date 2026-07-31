//! [`EntityRegistry`] — per-session cache of world objects.
//!
//! This is an inline "shadow" that remembers objects the client has seen
//! during the session, optionally persisting them beyond the visible view
//! rectangle.  Which entity kinds are cached is controlled by [`CacheMode`].
//!
//! Multi-object geometry (collision shapes) is resolved through cached
//! [`MultiDef`] structures and a per-world spatial index, so that
//! movement-validation queries can reference houses and boats even when
//! they have left the client's view rectangle.
//!
//! The registry stores objects across all visited worlds, indexed by
//! `world_id`.  When the player changes world (`set_world`), no data is
//! moved — only `current_world` is updated and staleness tracking is
//! armed.
//!
//! The registry is **generic over the entity type** `E: Entity`.  This
//! allows both the client-side diorama (`EntityRegistry<WorldEntity, …>`)
//! and the server-side continuum zone (`EntityRegistry<Entity, …>`) to
//! share the same implementation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::debug;
use files::multi::MultiPart;

use crate::diorama::staleness::{StalenessTracker, DEFAULT_STALENESS_THRESHOLD};

use super::multi_def::MultiDef;
use super::multi_spatial::SpatialIndex;
use crate::vessel::objects::Entity;
use super::shape_provider::ShapeProvider;
use super::tile_rect::TileRect;
use crate::vessel::tile_shape::TileShape;
use crate::vessel::traits::StaticDataProvider;

// ── Configuration ─────────────────────────────────────────────────────────

/// Which entity kinds the registry should cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Cache disabled — the registry stays empty.
    None,
    /// Cache multi-objects only (houses, boats).
    /// This matches the legacy `MultiRegistry` behaviour.
    MultisOnly,
    /// Cache items and multi-objects (no mobiles).
    ItemsAndMultis,
    /// Cache everything including mobiles.
    All,
}

impl Default for CacheMode {
    fn default() -> Self { CacheMode::MultisOnly }
}

/// Per-kind staleness sweep configuration.
#[derive(Debug, Clone)]
pub struct StalenessConfig {
    /// Sweep stale items.
    pub items: bool,
    /// Sweep stale multi-objects.
    pub multis: bool,
    /// Sweep stale mobiles.
    pub mobiles: bool,
    /// Quiet-period before a sweep is triggered.
    pub threshold: Duration,
}

impl Default for StalenessConfig {
    fn default() -> Self {
        Self {
            items: false,
            multis: true,
            mobiles: false,
            threshold: DEFAULT_STALENESS_THRESHOLD,
        }
    }
}

// ── Internal: per-world store ─────────────────────────────────────────────

/// Per-world bucket: entities + spatial index for multi/item shapes.
#[derive(Clone)]
struct WorldStore<E: Entity, S: SpatialIndex + Default> {
    /// All cached entities in this world, keyed by serial.
    entities: HashMap<u32, E>,
    /// Spatial index for entities that contribute collision shapes
    /// (multis, and items when `CacheMode` includes them).
    spatial: S,
    /// Bounding boxes for multi-objects (computed from [`MultiDef`]).
    multi_bbox: HashMap<u32, TileRect>,
    /// Number of shape-contributing entities (for `shapes_empty`).
    shape_count: usize,
}

impl<E: Entity, S: SpatialIndex + Default> WorldStore<E, S> {
    fn new() -> Self {
        Self {
            entities: HashMap::new(),
            spatial: S::default(),
            multi_bbox: HashMap::new(),
            shape_count: 0,
        }
    }

    fn clear(&mut self) {
        self.entities.clear();
        self.spatial.clear();
        self.multi_bbox.clear();
        self.shape_count = 0;
    }

    fn len(&self) -> usize {
        self.entities.len()
    }

    fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

// ── EntityRegistry ────────────────────────────────────────────────────────

/// Per-session entity cache with spatial indexing and staleness tracking.
///
/// Generic over:
/// - `E` — the entity type (must implement [`Entity`]).
/// - `S` — the spatial index implementation.
#[derive(Clone)]
pub struct EntityRegistry<E: Entity, S: SpatialIndex + Default> {
    current_world: u8,
    cache_mode: CacheMode,
    staleness_config: StalenessConfig,
    static_data: Option<Arc<dyn StaticDataProvider>>,

    /// Per-world stores.
    stores: HashMap<u8, WorldStore<E, S>>,

    /// Global serial → world mapping for O(1) removal routing.
    serial_index: HashMap<u32, u8>,

    /// Shared cache of parsed standard `MultiDef` by graphic id.
    multi_defs: HashMap<u16, Arc<MultiDef>>,

    /// Per-serial custom `MultiDef` (from `0xD8 SendCustomHouse`).
    ///
    /// When present for a serial, overrides the standard `multi_defs`
    /// entry for shape queries.
    custom_defs: HashMap<u32, Arc<MultiDef>>,

    /// Staleness tracker (armed on world change / view shift).
    staleness: StalenessTracker,
}

// ── Constructors & configuration ──────────────────────────────────────────

impl<E: Entity, S: SpatialIndex + Default> EntityRegistry<E, S> {
    /// Create a new registry for the given world with the specified cache mode.
    pub fn new(
        static_data: Option<Arc<dyn StaticDataProvider>>,
        world: u8,
        cache_mode: CacheMode,
    ) -> Self {
        Self {
            current_world: world,
            cache_mode,
            staleness_config: StalenessConfig::default(),
            static_data,
            stores: HashMap::new(),
            serial_index: HashMap::new(),
            multi_defs: HashMap::new(),
            custom_defs: HashMap::new(),
            staleness: StalenessTracker::new(DEFAULT_STALENESS_THRESHOLD),
        }
    }

    /// Current cache mode.
    pub fn cache_mode(&self) -> CacheMode {
        self.cache_mode
    }

    /// Set the cache mode.
    pub fn set_cache_mode(&mut self, mode: CacheMode) {
        self.cache_mode = mode;
    }

    /// Set the staleness sweep configuration.
    pub fn set_staleness_config(&mut self, config: StalenessConfig) {
        self.staleness = StalenessTracker::new(config.threshold);
        self.staleness_config = config;
    }

    /// Access the static data provider (if available).
    pub fn static_data(&self) -> Option<&Arc<dyn StaticDataProvider>> {
        self.static_data.as_ref()
    }

    /// Set or replace the static data provider.
    ///
    /// Clears cached `MultiDef` entries and all spatial data so that
    /// shapes are recomputed on the next insert.
    pub fn set_static_data(&mut self, sd: Option<Arc<dyn StaticDataProvider>>) {
        self.static_data = sd;
        self.multi_defs.clear();
        self.custom_defs.clear();
        // Clear spatial + bbox but keep entities (they'll be re-indexed on rebuild)
        for store in self.stores.values_mut() {
            store.spatial.clear();
            store.multi_bbox.clear();
            store.shape_count = 0;
        }
    }

    /// Current world id.
    pub fn current_world(&self) -> u8 {
        self.current_world
    }

    // ── World management ─────────────────────────────────────────────

    /// Switch to a different world.
    ///
    /// No data is moved — only `current_world` is updated and the
    /// staleness tracker is armed so that objects promoted from the
    /// cache can be validated by the server.
    pub fn set_world(&mut self, world: u8) {
        if world == self.current_world {
            return;
        }
        self.current_world = world;
        self.staleness.arm();
    }

    // ── Mutations ────────────────────────────────────────────────────

    /// Insert or update an entity in the cache.
    ///
    /// The entity is placed in `stores[world]`.  For multi-objects, the
    /// [`MultiDef`] is resolved from `static_data` (and cached), a
    /// bounding box is computed, and the spatial index is updated.
    /// For items (when `CacheMode` includes them), a single-tile bbox
    /// is inserted into the spatial index.
    ///
    /// Confirms the serial in the staleness tracker.
    pub fn insert(&mut self, entity: &E, world: u8) {
        if self.cache_mode == CacheMode::None {
            return;
        }

        let serial = entity.serial();

        // Remove from previous location (may be in a different world)
        self.remove(serial);

        let store = self.stores.entry(world).or_insert_with(WorldStore::new);

        if entity.is_multi() {
            // Resolve MultiDef and compute bbox
            if let Some(sd) = &self.static_data {
                let graphic = entity.graphic();
                let def = self.multi_defs
                    .entry(graphic)
                    .or_insert_with(|| {
                        let parts = sd.multi_parts(graphic);
                        let def = MultiDef::from_parts(parts);
                        debug!(
                            "[entity_registry] cached MultiDef graphic={:#06X}: \
                             {} tiles, {} parts, extent x=[{}..{}] y=[{}..{}]",
                            graphic, def.tile_count(), def.part_count(),
                            def.extent.x_min, def.extent.x_max,
                            def.extent.y_min, def.extent.y_max,
                        );
                        Arc::new(def)
                    })
                    .clone();

                let pos = entity.pos();
                let bbox = compute_bbox(pos.x, pos.y, &def);
                store.spatial.insert(serial, bbox);
                store.multi_bbox.insert(serial, bbox);
                store.shape_count += 1;
            }
        } else if !entity.is_mobile() {
            // Items occupy a single tile — insert a 1x1 bbox
            let pos = entity.pos();
            let bbox = TileRect {
                x_min: pos.x,
                y_min: pos.y,
                x_max: pos.x,
                y_max: pos.y,
            };
            store.spatial.insert(serial, bbox);
            store.shape_count += 1;
        }
        // Mobiles don't contribute collision shapes — no spatial insert.

        store.entities.insert(serial, entity.clone());
        self.serial_index.insert(serial, world);
        self.staleness.confirm(serial);
    }

    /// Add a custom house overlay for an existing multi.
    ///
    /// The entity must already exist in the registry (inserted via
    /// [`insert`](Self::insert) when the `0x1A`/`0xF3` packet arrived).
    /// The custom `parts` (decoded from `0xD8 SendCustomHouse`) are
    /// stored **alongside** the standard [`MultiDef`] (from `multi.mul`),
    /// not as a replacement.  The spatial-index bounding box is expanded
    /// to cover both the foundation and the custom overlay.
    ///
    /// If the entity is not found, this is a no-op.
    pub fn add_custom(
        &mut self,
        serial: u32,
        parts: &[MultiPart],
        world: u8,
    ) {
        let Some(store) = self.stores.get_mut(&world) else { return };
        let Some(entity) = store.entities.get(&serial) else { return };

        let custom_def = Arc::new(MultiDef::from_parts(parts));
        let pos = entity.pos();

        // Compute merged bbox: union of standard foundation + custom overlay.
        let standard_def = self.multi_defs.get(&entity.graphic());
        let bbox = if let Some(std_def) = standard_def {
            let std_bbox = compute_bbox(pos.x, pos.y, std_def);
            let cst_bbox = compute_bbox(pos.x, pos.y, &custom_def);
            TileRect {
                x_min: std_bbox.x_min.min(cst_bbox.x_min),
                y_min: std_bbox.y_min.min(cst_bbox.y_min),
                x_max: std_bbox.x_max.max(cst_bbox.x_max),
                y_max: std_bbox.y_max.max(cst_bbox.y_max),
            }
        } else {
            compute_bbox(pos.x, pos.y, &custom_def)
        };

        debug!(
            "[entity_registry] custom multi serial={:#010X}: {} tiles, {} parts, \
             extent x=[{}..{}] y=[{}..{}], merged bbox ({},{})..({},{})",
            serial, custom_def.tile_count(), custom_def.part_count(),
            custom_def.extent.x_min, custom_def.extent.x_max,
            custom_def.extent.y_min, custom_def.extent.y_max,
            bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max,
        );

        // Update spatial index with merged bbox
        store.spatial.remove(serial);
        store.spatial.insert(serial, bbox);
        store.multi_bbox.insert(serial, bbox);

        // Store custom def (queried alongside standard def for shape lookups)
        self.custom_defs.insert(serial, custom_def);
    }

    /// Remove an entity from the cache (any world).
    pub fn remove(&mut self, serial: u32) {
        if let Some(world) = self.serial_index.remove(&serial) {
            if let Some(store) = self.stores.get_mut(&world) {
                if let Some(entity) = store.entities.remove(&serial) {
                    if !entity.is_mobile() {
                        store.spatial.remove(serial);
                        store.shape_count = store.shape_count.saturating_sub(1);
                    }
                    store.multi_bbox.remove(&serial);
                }
                if store.is_empty() {
                    self.stores.remove(&world);
                }
            }
        }
        self.custom_defs.remove(&serial);
    }

    /// Mark a mobile as hidden (e.g. after `0x1D DeleteObject`).
    ///
    /// The entity remains in the store for future queries / sweeps,
    /// but can be identified as hidden via the returned flag on
    /// `get` queries (currently a no-op placeholder — the entity stays
    /// as-is until swept or the session ends).
    pub fn mark_hidden(&mut self, _serial: u32) {
        // Currently a no-op: the entity remains in the store.
        // A future extension may add a `hidden: HashSet<u32>` to
        // track hidden mobiles and filter them from certain queries.
    }

    /// Clear all entities for a specific world.
    pub fn clear_world(&mut self, world: u8) {
        if let Some(store) = self.stores.get_mut(&world) {
            // Remove serial_index + custom_defs entries for this world
            for serial in store.entities.keys() {
                self.serial_index.remove(serial);
                self.custom_defs.remove(serial);
            }
            store.clear();
        }
        if world == self.current_world {
            self.staleness.disarm();
        }
    }

    /// Clear all worlds, preserving cached `MultiDef` entries.
    pub fn clear_all(&mut self) {
        self.stores.clear();
        self.serial_index.clear();
        self.custom_defs.clear();
        self.staleness.disarm();
    }

    /// Full reset: clear everything including cached `MultiDef` entries.
    pub fn clear_all_with_defs(&mut self) {
        self.clear_all();
        self.multi_defs.clear();
    }

    // ── Staleness ────────────────────────────────────────────────────

    /// Arm the staleness tracker (e.g. when new view strips appear).
    pub fn arm_staleness(&mut self) {
        self.staleness.arm();
    }

    /// Confirm that an entity is still alive (server sent fresh data).
    pub fn confirm(&mut self, serial: u32) {
        self.staleness.confirm(serial);
    }

    /// Whether a staleness sweep is due.
    pub fn should_sweep(&self) -> bool {
        self.staleness.should_sweep()
    }

    /// Sweep stale entities within `view_rect` (current world only).
    ///
    /// Queries the spatial index for candidates overlapping `view_rect`,
    /// then removes those that were **not** confirmed by the staleness
    /// tracker.  Respects `staleness_config` — only sweeps entity kinds
    /// that are enabled.
    ///
    /// Returns the number of removed entities.
    pub fn sweep_stale(&mut self, view_rect: &TileRect) -> usize {
        if !self.staleness.should_sweep() {
            return 0;
        }

        let Some(store) = self.stores.get(&self.current_world) else {
            self.staleness.disarm();
            return 0;
        };

        let candidates: Vec<u32> = store.spatial.query_rect(view_rect);

        // Filter candidates by staleness_config — only sweep enabled kinds
        let filtered: Vec<u32> = candidates.into_iter().filter(|serial| {
            if let Some(entity) = store.entities.get(serial) {
                if entity.is_mobile() {
                    self.staleness_config.mobiles
                } else if entity.is_multi() {
                    self.staleness_config.multis
                } else {
                    self.staleness_config.items
                }
            } else {
                true // orphan in spatial index — sweep it
            }
        }).collect();

        let stale = self.staleness.sweep(&filtered);
        let count = stale.len();

        for serial in &stale {
            debug!(
                "[entity_registry] sweep: removing stale entity serial={:#010X} (world={})",
                serial, self.current_world,
            );
            self.remove(*serial);
        }

        count
    }

    // ── Queries (current world) ──────────────────────────────────────

    /// Get a cached entity by serial (current world only).
    pub fn get(&self, serial: u32) -> Option<&E> {
        self.stores.get(&self.current_world)
            .and_then(|store| store.entities.get(&serial))
    }

    /// Serials in the spatial index that overlap `rect` (current world).
    pub fn serials_in_rect(&self, rect: &TileRect) -> Vec<u32> {
        self.stores.get(&self.current_world)
            .map(|store| store.spatial.query_rect(rect))
            .unwrap_or_default()
    }

    /// Check whether a serial's bbox overlaps `rect` (current world).
    ///
    /// For multi-objects, checks the multi bounding box.
    /// For other entities, checks point-in-rect by coordinates.
    pub fn serial_in_rect(&self, serial: u32, rect: &TileRect) -> bool {
        let Some(store) = self.stores.get(&self.current_world) else { return false };
        if let Some(bbox) = store.multi_bbox.get(&serial) {
            return bbox.overlaps(rect);
        }
        if let Some(entity) = store.entities.get(&serial) {
            let pos = entity.pos();
            return pos.x >= rect.x_min && pos.x <= rect.x_max
                && pos.y >= rect.y_min && pos.y <= rect.y_max;
        }
        false
    }

    // ── Queries (any world) ──────────────────────────────────────────

    /// Get a cached entity by serial (any world).
    ///
    /// Returns `(world_id, &E)` if found.
    pub fn get_any(&self, serial: u32) -> Option<(u8, &E)> {
        let world = *self.serial_index.get(&serial)?;
        let entity = self.stores.get(&world)?.entities.get(&serial)?;
        Some((world, entity))
    }

    /// Get a cached entity by serial in a specific world.
    pub fn get_in_world(&self, serial: u32, world: u8) -> Option<&E> {
        self.stores.get(&world)?.entities.get(&serial)
    }

    /// Serials in the spatial index that overlap `rect` for a specific world.
    pub fn serials_in_rect_world(&self, world: u8, rect: &TileRect) -> Vec<u32> {
        self.stores.get(&world)
            .map(|store| store.spatial.query_rect(rect))
            .unwrap_or_default()
    }

    // ── Stats ────────────────────────────────────────────────────────

    /// Number of cached entities in the current world.
    pub fn len(&self) -> usize {
        self.stores.get(&self.current_world).map_or(0, |s| s.len())
    }

    /// Whether the current world store is empty.
    pub fn is_empty(&self) -> bool {
        self.stores.get(&self.current_world).map_or(true, |s| s.is_empty())
    }

    /// Total cached entities across all worlds.
    pub fn total_len(&self) -> usize {
        self.stores.values().map(|s| s.len()).sum()
    }

    /// Number of cached `MultiDef` entries.
    pub fn cached_defs(&self) -> usize {
        self.multi_defs.len()
    }

    /// Number of worlds with cached data.
    pub fn world_count(&self) -> usize {
        self.stores.len()
    }

    /// Return the foundation extent (from `multi.mul`) for a given
    /// multi serial, if the entity and its standard `MultiDef` are known.
    ///
    /// Returns `(x_min, y_min, x_max, y_max)` — relative part offsets.
    /// This is needed by [`SendCustomHouse::decode_all_tiles`](packets::house::SendCustomHouse::decode_all_tiles) for mode-2
    /// planes where X/Y are implicit and depend on the foundation bbox.
    pub fn foundation_extent(&self, serial: u32) -> Option<(i16, i16, i16, i16)> {
        let world = self.serial_index.get(&serial)?;
        let store = self.stores.get(world)?;
        let entity = store.entities.get(&serial)?;
        let def = self.multi_defs.get(&entity.graphic())?;
        Some((def.extent.x_min, def.extent.y_min, def.extent.x_max, def.extent.y_max))
    }

    /// Resolve and cache the [`MultiDef`] for a given multi graphic,
    /// returning a clone of the `Arc`.
    ///
    /// If static data is not available, returns `None`.
    pub fn resolve_multi_def(&mut self, graphic: u16) -> Option<Arc<MultiDef>> {
        if let Some(def) = self.multi_defs.get(&graphic) {
            return Some(def.clone());
        }
        let sd = self.static_data.as_ref()?;
        let parts = sd.multi_parts(graphic);
        let def = MultiDef::from_parts(parts);
        debug!(
            "[entity_registry] cached MultiDef graphic={:#06X}: \
             {} tiles, {} parts, extent x=[{}..{}] y=[{}..{}]",
            graphic, def.tile_count(), def.part_count(),
            def.extent.x_min, def.extent.x_max,
            def.extent.y_min, def.extent.y_max,
        );
        let arc = Arc::new(def);
        self.multi_defs.insert(graphic, arc.clone());
        Some(arc)
    }

    // ── Internal: shape queries ──────────────────────────────────────

    /// Query collision shapes at `(x, y)` from a specific store.
    fn query_shapes_in_store(
        store: &WorldStore<E, S>,
        multi_defs: &HashMap<u16, Arc<MultiDef>>,
        custom_defs: &HashMap<u32, Arc<MultiDef>>,
        sd: &dyn StaticDataProvider,
        x: u16,
        y: u16,
    ) -> Vec<TileShape> {
        let candidates = store.spatial.query_point(x, y);
        if candidates.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::new();

        for serial in candidates {
            let Some(entity) = store.entities.get(&serial) else { continue };

            if entity.is_multi() {
                // For custom houses, the 0xD8 packet carries only the
                // player-designed parts (walls, floors, roofs).  The
                // foundation (stone steps, base floor) lives in
                // `multi.mul` and must be included alongside the custom
                // overlay.  We therefore collect shapes from **both**
                // the standard def and the custom def when both exist.
                let standard_def = multi_defs.get(&entity.graphic());
                let custom_def = custom_defs.get(&serial);

                if standard_def.is_none() && custom_def.is_none() {
                    continue;
                }

                let pos = entity.pos();
                let dx = x as i32 - pos.x as i32;
                let dy = y as i32 - pos.y as i32;

                // Helper closure: emit shapes from a single MultiDef.
                let mut emit_shapes = |def: &MultiDef| {
                    if !def.contains(dx as i16, dy as i16) { return; }
                    for part in def.parts_at(dx as i16, dy as i16) {
                        if let Some(tile_def) = sd.static_tile_def(part.tile_id) {
                            let pz = pos.z.saturating_add(
                                part.z.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                            );
                            result.push(TileShape::from_static(pz, tile_def));
                        }
                    }
                };

                if let Some(def) = standard_def {
                    emit_shapes(def);
                }
                if let Some(def) = custom_def {
                    emit_shapes(def);
                }
            } else if !entity.is_mobile() {
                // Item: single tile shape
                if let Some(tile_def) = sd.static_tile_def(entity.graphic()) {
                    result.push(TileShape::from_static(entity.pos().z, tile_def));
                }
            }
            // Mobiles: no collision shapes
        }

        result
    }
}

// ── ShapeProvider impl ────────────────────────────────────────────────────

impl<E: Entity, S: SpatialIndex + Default> ShapeProvider for EntityRegistry<E, S> {
    fn get_shapes_at(&self, x: u16, y: u16) -> Vec<TileShape> {
        let Some(sd) = &self.static_data else { return Vec::new() };
        let Some(store) = self.stores.get(&self.current_world) else { return Vec::new() };
        Self::query_shapes_in_store(store, &self.multi_defs, &self.custom_defs, sd.as_ref(), x, y)
    }

    fn shapes_empty(&self) -> bool {
        self.stores.get(&self.current_world)
            .map_or(true, |s| s.shape_count == 0)
    }
}

// ── Free functions ────────────────────────────────────────────────────────

fn compute_bbox(ox: u16, oy: u16, def: &MultiDef) -> TileRect {
    TileRect {
        x_min: (ox as i32 + def.extent.x_min as i32).clamp(0, u16::MAX as i32) as u16,
        y_min: (oy as i32 + def.extent.y_min as i32).clamp(0, u16::MAX as i32) as u16,
        x_max: (ox as i32 + def.extent.x_max as i32).clamp(0, u16::MAX as i32) as u16,
        y_max: (oy as i32 + def.extent.y_max as i32).clamp(0, u16::MAX as i32) as u16,
    }
}
