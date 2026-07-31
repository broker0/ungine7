//! [`SessionView`] — per-session state: current world index, visible world,
//! entity registry, and feature flags.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use log::{debug, info, trace, warn};

use packets::character::UpdateMobile;
use packets::interaction::{DeleteObject, EquipItem};
use packets::system::{ClientViewRange, GeneralInfo};
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::{
    DrawMobile, DrawMobileExtended, ObjectDataType, ObjectInfo, ObjectInfoSA,
    PacketList,
};

use crate::ecumene::DiffOverlay;
use crate::ecumene::EntityRegistry;
use crate::ecumene::CacheMode;
use crate::ecumene::BBoxSpatialIndex;
use crate::ecumene::TileRect;
use crate::vessel::StaticDataProvider;
use super::visible_world::{VisibleWorld, WorldEntity};

// ── SessionView ───────────────────────────────────────────────────────────

/// Per-session mutable state.
#[derive(Clone)]
pub struct SessionView {
    /// Current UO map index for this session (0 = Felucca, 1 = Trammel, …).
    /// Updated when a `0xBF GeneralInfo::SetMap` packet is ingested.
    pub current_world: u8,

    /// Objects and containers the client of this session currently sees.
    pub visible: VisibleWorld,

    /// Per-session entity cache for movement validation and queries beyond
    /// the visible view rectangle.
    ///
    /// Replaces the legacy `MultiRegistry` — can cache multis, items, and
    /// mobiles depending on [`CacheMode`].
    pub registry: EntityRegistry<WorldEntity, BBoxSpatialIndex>,

    /// Per-session map/statics diff overlay.
    ///
    /// Updated when the server sends `EnableMapDiff` (0xBF sub 0x0018).
    /// Consulted by [`DiffAwareDataProvider`](crate::ecumene::DiffAwareDataProvider)
    /// to override base map/statics data.
    pub diff_overlay: DiffOverlay,

    /// Path to the UO client data directory (for loading diff files).
    ///
    /// `None` if data files are not available (e.g. no `--data-dir` flag).
    data_dir: Option<PathBuf>,

    /// Last S→C `0xB9 EnableFeatures` packet seen in the log.
    ///
    /// Stored as raw bytes so it can be re-sent to the client after a
    /// snapshot seek that crosses a world boundary (the server may send
    /// different feature flags per facet).
    pub last_enable_features: Option<Bytes>,

    /// Last S→C `0xBF` sub `0x0018` `EnableMapDiff` packet seen.
    ///
    /// Stored as raw bytes so it can be re-sent to Mirror clients during
    /// bootstrap — they need this packet to load the correct diff files
    /// from their local UO data directory.
    pub last_enable_map_diff: Option<Bytes>,
}

impl SessionView {
    /// Create a new session view centred at `(cx, cy)` with the given view range.
    pub fn new(cx: u16, cy: u16, view_range: u16) -> Self {
        Self {
            current_world: 0,
            visible: VisibleWorld::new(cx, cy, view_range),
            registry: EntityRegistry::new(None, 0, CacheMode::default()),
            diff_overlay: DiffOverlay::new(),
            data_dir: None,
            last_enable_features: None,
            last_enable_map_diff: None,
        }
    }

    /// Create a session view with static data for local movement validation.
    pub fn with_static_data(
        cx: u16,
        cy: u16,
        view_range: u16,
        static_data: Arc<dyn StaticDataProvider>,
    ) -> Self {
        Self {
            current_world: 0,
            visible: VisibleWorld::new(cx, cy, view_range),
            registry: EntityRegistry::new(Some(static_data), 0, CacheMode::default()),
            diff_overlay: DiffOverlay::new(),
            data_dir: None,
            last_enable_features: None,
            last_enable_map_diff: None,
        }
    }

    /// Create a session view with static data and a data directory for
    /// loading map/statics diff files.
    pub fn with_data_dir(
        cx: u16,
        cy: u16,
        view_range: u16,
        static_data: Arc<dyn StaticDataProvider>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            current_world: 0,
            visible: VisibleWorld::new(cx, cy, view_range),
            registry: EntityRegistry::new(Some(static_data), 0, CacheMode::default()),
            diff_overlay: DiffOverlay::new(),
            data_dir: Some(data_dir),
            last_enable_features: None,
            last_enable_map_diff: None,
        }
    }

    /// Set or replace the static data provider used by the entity registry.
    /// Existing registry entries are cleared and must be re-populated
    /// (e.g. via [`rebuild_registry`](Self::rebuild_registry)).
    pub fn set_static_data(&mut self, static_data: Option<Arc<dyn StaticDataProvider>>) {
        self.registry.set_static_data(static_data);
        self.rebuild_registry();
    }

    /// Set the UO client data directory (for loading diff files).
    pub fn set_data_dir(&mut self, data_dir: Option<PathBuf>) {
        self.data_dir = data_dir;
    }

    /// Get the data directory path, if set.
    pub fn data_dir(&self) -> Option<&PathBuf> {
        self.data_dir.as_ref()
    }

    /// Rebuild the entity registry from the current visible set.
    ///
    /// Call this after bulk operations (e.g. seek, world change) that
    /// replace the visible set wholesale.
    pub fn rebuild_registry(&mut self) {
        self.registry.clear_world(self.current_world);
        for entity in self.visible.iter() {
            if self.should_cache(entity) {
                self.registry.insert(entity, self.current_world);
            }
        }
        let count = self.registry.len();
        if count > 0 {
            trace!(
                "[session] rebuilt registry: {} entities cached",
                count,
            );
        }
    }

    /// Whether an entity should be cached based on the current `CacheMode`.
    fn should_cache(&self, entity: &super::visible_world::WorldEntity) -> bool {
        match self.registry.cache_mode() {
            CacheMode::None => false,
            CacheMode::MultisOnly => entity.is_multi(),
            CacheMode::ItemsAndMultis => !entity.is_mobile(),
            CacheMode::All => true,
        }
    }

    // ── View management ──────────────────────────────────────────────

    /// Update the view centre, evict out-of-view entities from visible,
    /// and return new strips to be populated.
    ///
    /// Entities evicted from `visible` remain in the `registry` cache
    /// (for passability and future queries).  Multi-objects are checked
    /// by bounding box, not just origin — a house whose origin is outside
    /// the view but whose bbox overlaps the view is kept visible.
    pub fn update_view(&mut self, cx: u16, cy: u16) -> Vec<TileRect> {
        // 1. Geometry — compute new rect + diff strips
        let (new_rect, new_strips) = self.visible.set_view_center(cx, cy);

        // 2. Evict entities outside the new view rect
        let outside = self.visible.serials_outside_rect(&new_rect);
        for serial in outside {
            // For multis, check bbox overlap (not just origin)
            if let Some(entity) = self.visible.get(serial) {
                if entity.is_multi() && self.registry.serial_in_rect(serial, &new_rect) {
                    continue; // multi bbox overlaps view — keep visible
                }
            }
            self.visible.remove(serial);
            // registry is NOT touched — entity stays in cache
        }

        // 3. Arm staleness if new strips appeared
        if !new_strips.is_empty() {
            self.registry.arm_staleness();
        }

        new_strips
    }

    /// Feed a raw S→C packet into the session view.
    ///
    /// Each packet is parsed **once** and the result is forwarded to both
    /// the visible world and the entity registry in a single pass.
    ///
    /// Handled packets:
    /// - `0xBF GeneralInfo::SetMap` — updates `current_world`, clears visible
    ///   world, switches registry world.
    /// - `0xC8 ClientViewRange` — updates the visible world's view range.
    /// - `0xB9 EnableFeatures` — caches the raw packet for re-send after seek.
    /// - `0x1A ObjectInfo` — upserts item/multi + syncs registry.
    /// - `0xF3 ObjectInfoSA` — upserts item/multi + syncs registry.
    /// - `0xF7 PacketList` — upserts sub-items + syncs registry.
    /// - `0x78 DrawMobile` — upserts mobile (with full equipment).
    /// - `0xD3 DrawMobileExtended` — upserts mobile (with full equipment).
    /// - `0x77 UpdateMobile` — updates existing mobile state.
    /// - `0x2E EquipItem` — updates equipment on a mobile.
    /// - `0x1D DeleteObject` — removes from visible world + registry.
    /// - `0x24 DrawContainer` — marks entity as container + caches contents.
    /// - `0x25 AddItemToContainer` / `0x3C ContainerContent` — caches contents.
    pub fn ingest_packet(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        match data[0] {
            // ── GeneralInfo (0xBF) ────────────────────────────────────
            0xBF => {
                if let Ok(gi) = GeneralInfo::from_bytes(data) {
                    match gi {
                        GeneralInfo::SetMap { world } => {
                            info!("[session] SetMap: world {world}");
                            self.current_world = world;
                            self.visible.clear();
                            self.registry.set_world(world);
                        }
                        GeneralInfo::EnableMapDiff { maps } => {
                            // Cache the raw packet for bootstrap re-send.
                            self.last_enable_map_diff = Some(Bytes::copy_from_slice(data));

                            let entries: Vec<(u32, u32)> = maps
                                .iter()
                                .map(|e| (e.map_patches, e.static_patches))
                                .collect();

                            for (i, entry) in maps.iter().enumerate() {
                                debug!(
                                    "[session] EnableMapDiff: world {i}: \
                                     map_patches={}, static_patches={}",
                                    entry.map_patches, entry.static_patches,
                                );
                            }

                            if let Some(dir) = &self.data_dir {
                                info!(
                                    "[session] EnableMapDiff: {} worlds, loading from {}",
                                    entries.len(),
                                    dir.display(),
                                );
                                self.diff_overlay.load_and_apply(dir, &entries);
                                debug!(
                                    "[session] EnableMapDiff: overlay applied, \
                                     is_empty={}",
                                    self.diff_overlay.is_empty(),
                                );
                            } else {
                                warn!(
                                    "[session] EnableMapDiff: {} worlds, \
                                     but no data_dir configured — diffs ignored",
                                    entries.len(),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }

            // ── ClientViewRange (0xC8) ──────────────────────────────
            id if id == ClientViewRange::ID => {
                if let Ok(cvr) = ClientViewRange::from_bytes(data) {
                    let old_range = self.visible.view_range();
                    self.visible.set_view_range(cvr.range as u16);
                    if cvr.range as u16 != old_range {
                        info!(
                            "[session] S→C ClientViewRange: range {} → {} (world={})",
                            old_range, cvr.range, self.current_world,
                        );
                    } else {
                        debug!(
                            "[session] S→C ClientViewRange: range={} (unchanged)",
                            cvr.range,
                        );
                    }
                }
            }

            // ── EnableFeatures (0xB9) ───────────────────────────────
            0xB9 => {
                debug!("[session] S→C 0xB9 EnableFeatures ({} bytes)", data.len());
                self.last_enable_features = Some(Bytes::copy_from_slice(data));
            }

            // ── ObjectInfo (0x1A) ───────────────────────────────────
            id if id == ObjectInfo::ID => {
                if let Ok(obj) = ObjectInfo::from_bytes(data) {
                    self.visible.upsert_object_info(&obj);
                    // Cache in registry
                    if let Some(entity) = self.visible.get(obj.object_id) {
                        if self.should_cache(entity) {
                            self.registry.insert(entity, self.current_world);
                        }
                    }
                    if obj.multi_id().is_some() {
                        debug!(
                            "[session] 0x1A ObjectInfo serial={:#010X} multi={:#06X} ({},{},{})",
                            obj.object_id, obj.multi_id().unwrap_or(0), obj.x, obj.y, obj.z,
                        );
                    } else {
                        debug!(
                            "[session] 0x1A ObjectInfo serial={:#010X} graphic={:#06X} ({},{},{})",
                            obj.object_id, obj.graphic, obj.x, obj.y, obj.z,
                        );
                    }
                }
            }

            // ── ObjectInfoSA (0xF3) ─────────────────────────────────
            id if id == ObjectInfoSA::ID => {
                if let Ok(obj) = ObjectInfoSA::from_bytes(data) {
                    self.visible.upsert_object_info_sa(&obj);
                    // Cache in registry
                    if let Some(entity) = self.visible.get(obj.serial) {
                        if self.should_cache(entity) {
                            self.registry.insert(entity, self.current_world);
                        }
                    }
                    if obj.data_type == ObjectDataType::Multi {
                        debug!(
                            "[session] 0xF3 ObjectInfoSA serial={:#010X} multi={:#06X} ({},{},{})",
                            obj.serial, obj.graphic, obj.x, obj.y, obj.z,
                        );
                    } else {
                        debug!(
                            "[session] 0xF3 ObjectInfoSA serial={:#010X} graphic={:#06X} ({},{},{})",
                            obj.serial, obj.graphic, obj.x, obj.y, obj.z,
                        );
                    }
                }
            }

            // ── PacketList (0xF7) ───────────────────────────────────
            id if id == PacketList::ID => {
                if let Ok(list) = PacketList::from_bytes(data) {
                    debug!(
                        "[session] 0xF7 PacketList: {} sub-items",
                        list.items.len(),
                    );
                    for obj in &list.items {
                        self.visible.upsert_object_info_sa(obj);
                        // Cache in registry
                        if let Some(entity) = self.visible.get(obj.serial) {
                            if self.should_cache(entity) {
                                self.registry.insert(entity, self.current_world);
                            }
                        }
                    }
                }
            }

            // ── DrawMobile (0x78) ───────────────────────────────────
            id if id == DrawMobile::ID => {
                if let Ok(mob) = DrawMobile::parse(data, false) {
                    debug!(
                        "[session] 0x78 DrawMobile serial={:#010X} graphic={:#06X} ({},{},{}) items={}",
                        mob.serial, mob.graphic, mob.x, mob.y, mob.z, mob.items.len(),
                    );
                    let serial = mob.serial;
                    self.visible.upsert_draw_mobile(&mob);
                    // Cache in registry
                    if let Some(entity) = self.visible.get(serial) {
                        if self.should_cache(entity) {
                            self.registry.insert(entity, self.current_world);
                        }
                    }
                }
            }

            // ── DrawMobileExtended (0xD3) ───────────────────────────
            id if id == DrawMobileExtended::ID => {
                if let Ok(mob) = DrawMobileExtended::parse(data, false) {
                    debug!(
                        "[session] 0xD3 DrawMobileExtended serial={:#010X} graphic={:#06X} ({},{},{}) items={}",
                        mob.serial, mob.graphic, mob.x, mob.y, mob.z, mob.items.len(),
                    );
                    let serial = mob.serial;
                    self.visible.upsert_draw_mobile_ext(&mob);
                    // Cache in registry
                    if let Some(entity) = self.visible.get(serial) {
                        if self.should_cache(entity) {
                            self.registry.insert(entity, self.current_world);
                        }
                    }
                }
            }

            // ── UpdateMobile (0x77) ─────────────────────────────────
            id if id == UpdateMobile::ID => {
                if let Ok(upd) = UpdateMobile::from_bytes(data) {
                    debug!(
                        "[session] 0x77 UpdateMobile serial={:#010X} ({},{},{})",
                        upd.serial, upd.x, upd.y, upd.z,
                    );
                    self.visible.apply_update_mobile(&upd);
                    // Update position in registry if cached
                    if let Some(entity) = self.visible.get(upd.serial) {
                        if self.should_cache(entity) {
                            self.registry.insert(entity, self.current_world);
                        }
                    }
                }
            }

            // ── EquipItem (0x2E) ────────────────────────────────────
            id if id == EquipItem::ID => {
                if let Ok(equip) = EquipItem::from_bytes(data) {
                    debug!(
                        "[session] 0x2E EquipItem item={:#010X} on mobile={:#010X} layer={:?}",
                        equip.item_serial, equip.player_serial, equip.layer,
                    );
                    self.visible.apply_equip_item(&equip);
                    // Equipment doesn't affect passability — no registry update
                }
            }

            // ── DeleteObject (0x1D) ─────────────────────────────────
            id if id == DeleteObject::ID => {
                if let Ok(d) = DeleteObject::from_bytes(data) {
                    debug!(
                        "[session] 0x1D DeleteObject serial={:#010X}",
                        d.serial,
                    );
                    // Check if mobile before removing from visible (need kind info)
                    let was_mobile = self.visible.get(d.serial)
                        .map(|e| e.is_mobile())
                        .unwrap_or(false);
                    self.visible.remove(d.serial);
                    if was_mobile {
                        self.registry.mark_hidden(d.serial);
                    } else {
                        self.registry.remove(d.serial);
                    }
                }
            }

            // ── Container packets (0x24, 0x25, 0x3C) ────────────────
            // Delegated to VisibleWorld which handles both entity
            // marking and container cache ingestion.
            0x24 => {
                if data.len() >= 5 {
                    let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                    let gump = if data.len() >= 7 {
                        u16::from_be_bytes([data[5], data[6]])
                    } else {
                        0
                    };
                    debug!(
                        "[session] 0x24 DrawContainer serial={:#010X} gump={:#06X}",
                        serial, gump,
                    );
                    self.visible.mark_container(serial, gump);
                }
            }
            0x25 => {
                use packets::interaction::AddItemToContainer;
                use packets::traits::ManualPacket;
                if let Ok(add) = AddItemToContainer::from_bytes(data) {
                    let cs = add.container_serial();
                    let item = super::container_item_from_add(&add);
                    self.visible.containers_mut().ingest_item_upsert(cs, item);
                    let item_count = self.visible.containers()
                        .get(cs)
                        .map(|c| c.item_count())
                        .unwrap_or(0);
                    debug!(
                        "[session] 0x25 AddItemToContainer container={:#010X} ({} items now)",
                        cs, item_count,
                    );
                }
            }
            0x3C => {
                use packets::interaction::ContainerContent;
                use packets::traits::ManualPacket;
                if let Ok(cc) = ContainerContent::from_bytes(data) {
                    if let Some(cs) = cc.container_serial() {
                        let items = super::container_items_from_content(&cc);
                        self.visible.containers_mut().ingest_content(cs, items);
                        let item_count = self.visible.containers()
                            .get(cs)
                            .map(|c| c.item_count())
                            .unwrap_or(0);
                        debug!(
                            "[session] 0x3C ContainerContent container={:#010X} ({} items)",
                            cs, item_count,
                        );
                    }
                }
            }

            // ── SendCustomHouse (0xD8) ──────────────────────────────
            0xD8 => {
                use packets::house::SendCustomHouse;
                use files::multi::MultiPart;

                if let Ok(house) = SendCustomHouse::from_bytes(data) {
                    let serial = house.house_serial;

                    info!(
                        "[session] 0xD8 SendCustomHouse received: serial={:#010X} \
                         rev={} planes={}",
                        serial, house.revision, house.planes.len(),
                    );

                    // Look up the entity to get position and graphic.
                    // Copy the data we need so we don't borrow the registry.
                    let entity_info = self.registry.get_any(serial).map(|(w, e)| {
                        let pos = <WorldEntity as crate::vessel::objects::Entity>::pos(e);
                        let graphic = <WorldEntity as crate::vessel::objects::Entity>::graphic(e);
                        (w, pos.x, pos.y, graphic)
                    });

                    if let Some((world, _multi_x, _multi_y, graphic)) = entity_info {
                        // Resolve the standard MultiDef to get foundation extent.
                        // This may mutate the cache (inserts a new def on first access).
                        let extent = self.registry.resolve_multi_def(graphic)
                            .map(|def| (def.extent.x_min, def.extent.y_min,
                                        def.extent.x_max, def.extent.y_max));

                        let (fx_min, fy_min, fx_max, fy_max) = extent
                            .unwrap_or((0, 0, 0, 0));

                        match house.decode_all_tiles(fx_min, fy_min, fx_max, fy_max) {
                            Ok(tiles) => {
                                let parts: Vec<MultiPart> = tiles.iter().map(|t| MultiPart {
                                    tile_id: t.tile_id,
                                    x: t.x,
                                    y: t.y,
                                    z: t.z,
                                    flags: 1,
                                }).collect();

                                info!(
                                    "[session] 0xD8 SendCustomHouse serial={:#010X} \
                                     rev={} planes={} tiles={} → registry.add_custom()",
                                    serial, house.revision, house.planes.len(), parts.len(),
                                );

                                self.registry.add_custom(serial, &parts, world);
                            }
                            Err(e) => {
                                warn!(
                                    "[session] 0xD8 SendCustomHouse serial={:#010X}: \
                                     tile decode failed: {}",
                                    serial, e,
                                );
                            }
                        }
                    } else {
                        warn!(
                            "[session] 0xD8 SendCustomHouse serial={:#010X}: \
                             entity NOT in registry (no collision data)",
                            serial,
                        );
                    }
                }
            }

            _ => {}
        }

        // Check if a staleness sweep is due after processing this packet.
        self.sweep_stale();
    }

    /// Current view rectangle.
    pub fn view_rect(&self) -> &TileRect {
        self.visible.view_rect()
    }

    /// Current view range (Chebyshev radius).
    pub fn view_range(&self) -> u16 {
        self.visible.view_range()
    }

    // ── Staleness sweep ──────────────────────────────────────────────

    /// Perform a staleness sweep if conditions are met (tracker armed +
    /// quiet period elapsed).  Returns the number of stale entities
    /// removed, or 0 if no sweep was needed.
    ///
    /// Called automatically at the end of [`ingest_packet`](Self::ingest_packet),
    /// and can also be called explicitly (e.g. after a player step).
    pub fn sweep_stale(&mut self) -> usize {
        if !self.registry.should_sweep() {
            return 0;
        }
        let rect = *self.visible.view_rect();
        let swept = self.registry.sweep_stale(&rect);
        if swept > 0 {
            debug!("[session] swept {} stale entities in view rect", swept);
        }
        swept
    }

    // ── Backward compatibility ───────────────────────────────────────

    /// Backward-compatible alias for [`sweep_stale`](Self::sweep_stale).
    #[deprecated(note = "use sweep_stale() instead")]
    pub fn sweep_stale_multis(&mut self) -> usize {
        self.sweep_stale()
    }

    /// Backward-compatible alias for [`rebuild_registry`](Self::rebuild_registry).
    #[deprecated(note = "use rebuild_registry() instead")]
    pub fn rebuild_multi_registry(&mut self) {
        self.rebuild_registry()
    }
}
