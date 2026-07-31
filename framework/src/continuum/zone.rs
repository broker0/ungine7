use std::sync::Arc;

use u_core::{BlockKey, Heading};

use crate::ecumene::entity_registry::EntityRegistry;
use crate::ecumene::entity_registry::CacheMode;
use crate::ecumene::multi_spatial::{BlockSpatialIndex, SpatialIndex};
use crate::vessel::objects::Entity;
use crate::ecumene::shape_provider::ShapeProvider;
use crate::ecumene::snapshot::CollisionSnapshot;
use crate::ecumene::static_provider::StaticTileProvider;
use crate::ecumene::tile_block::TileBlock;
use crate::ecumene::tile_provider::TileProvider;
use crate::ecumene::tile_rect::TileRect;
use crate::vessel::tile_shape::TileShape;
use crate::vessel::traits::StaticDataProvider;
use crate::ecumene::movement::MovementValidator;
use crate::ecumene::line_of_sight::LosValidator;

use super::container::{ZoneContainers, NoContainers};
use super::item_props::{ZoneItemProps, NoItemProps};
use super::traits::EntityStore;

pub struct Zone<E: Entity, C: ZoneContainers = NoContainers, P: ZoneItemProps = NoItemProps> {
    pub map_id: u8,
    static_data: Option<Arc<dyn StaticDataProvider>>,
    pub store: Box<dyn EntityStore<E>>,
    pub snapshot: CollisionSnapshot,
    pub registry: EntityRegistry<E, BlockSpatialIndex>,
    /// Spatial index over **all** entities (mobiles, items, multis) for
    /// fast area queries.  Keyed by entity serial, indexed by tile position.
    pub entity_spatial: BlockSpatialIndex,
    /// Container inventory: container serial → contents.
    ///
    /// Stored separately from the entity store because contained items
    /// have gump-relative coordinates, not world coordinates, and don't
    /// participate in collision detection.
    pub containers: C,
    /// Per-item property storage (name, tooltip, custom metadata).
    ///
    /// Stored separately from the entity store because the same item
    /// serial can exist in the entity store, as an equipped item on a
    /// mobile, or inside a container — properties are shared across all
    /// three locations.
    pub item_props: P,
}

impl<E: Entity, C: ZoneContainers, P: ZoneItemProps> Zone<E, C, P> {
    pub fn new(
        map_id: u8,
        static_data: Option<Arc<dyn StaticDataProvider>>,
        store: Box<dyn EntityStore<E>>,
        _width_blocks: u16,
        _height_blocks: u16,
    ) -> Self {
        Self {
            map_id,
            registry: EntityRegistry::new(static_data.clone(), map_id, CacheMode::MultisOnly),
            static_data,
            store,
            snapshot: CollisionSnapshot::new(),
            entity_spatial: BlockSpatialIndex::new(),
            containers: C::default(),
            item_props: P::default(),
        }
    }

    pub fn spawn(&mut self, id: u32, data: E) {
        // Update spatial index for all entity types.
        let pos = data.pos();
        self.entity_spatial.insert(id, TileRect::point(pos.x, pos.y));

        if data.is_multi() {
            self.registry.insert(&data, self.map_id);
        } else if !data.is_mobile() {
            let shapes = self.extract_entity_shapes(&data);
            let tag: u64 = id as u64;
            for (x, y, shape) in shapes {
                self.snapshot.add_shape(x, y, tag, shape);
            }
        }
        self.store.insert(id, data);
    }

    pub fn remove(&mut self, id: u32) -> Option<E> {
        if let Some(data) = self.store.remove(id) {
            self.entity_spatial.remove(id);

            if data.is_multi() {
                self.registry.remove(id);
            } else if !data.is_mobile() {
                let tag: u64 = id as u64;
                let shapes = self.extract_entity_shapes(&data);
                for (x, y, _) in shapes {
                    self.snapshot.remove_entity_shapes(x, y, tag);
                }
            }
            Some(data)
        } else { None }
    }

    pub fn update(&mut self, id: u32, data: E) {
        // Remove old spatial entry (if any).
        self.entity_spatial.remove(id);

        if let Some(old) = self.store.remove(id) {
            if old.is_multi() {
                self.registry.remove(id);
            } else if !old.is_mobile() {
                let tag: u64 = id as u64;
                let old_shapes = self.extract_entity_shapes(&old);
                for (x, y, _) in old_shapes {
                    self.snapshot.remove_entity_shapes(x, y, tag);
                }
            }
        }
        if data.is_multi() {
            self.registry.insert(&data, self.map_id);
        } else if !data.is_mobile() {
            let new_shapes = self.extract_entity_shapes(&data);
            let tag: u64 = id as u64;
            for (x, y, shape) in new_shapes {
                self.snapshot.add_shape(x, y, tag, shape);
            }
        }
        // Insert new spatial entry.
        let pos = data.pos();
        self.entity_spatial.insert(id, TileRect::point(pos.x, pos.y));
        self.store.insert(id, data);
    }

    pub fn get(&self, id: u32) -> Option<&E> {
        self.store.get(id)
    }

    /// Move an entity to a new position and/or change its direction.
    ///
    /// Updates the entity store **and** the spatial index atomically.
    /// If `direction` is `Some`, the entity's facing is also updated
    /// (only meaningful for mobiles).
    ///
    /// Returns `true` if the entity was found and updated.
    pub fn move_entity(
        &mut self,
        id: u32,
        new_x: u16,
        new_y: u16,
        new_z: i8,
        direction: Option<u8>,
    ) -> bool {
        let entity = match self.store.get_mut(id) {
            Some(e) => e,
            None => return false,
        };

        let old_pos = entity.pos();
        let pos_changed = old_pos.x != new_x || old_pos.y != new_y;

        entity.set_pos(u_core::Pos3D::new(new_x, new_y, new_z));
        if let Some(dir) = direction {
            entity.set_direction(dir);
        }

        if pos_changed {
            self.entity_spatial.remove(id);
            self.entity_spatial.insert(id, TileRect::point(new_x, new_y));
        }

        true
    }

    /// Returns all entities whose position falls within `area`.
    ///
    /// Uses the spatial index for a fast block-level lookup, then filters
    /// candidates by exact tile position.
    pub fn query_area(&self, area: &TileRect) -> Vec<E> {
        let candidates = self.entity_spatial.query_rect(area);
        candidates
            .into_iter()
            .filter_map(|serial| {
                let entity = self.store.get(serial)?;
                let pos = entity.pos();
                if area.contains_pos(&pos) {
                    Some(entity.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn extract_entity_shapes(&self, entity: &E) -> Vec<(u16, u16, TileShape)> {
        match &self.static_data {
            Some(sd) => entity.extract_shapes(&**sd),
            None => vec![],
        }
    }

    fn make_provider(&self) -> ZoneTileProvider<'_> {
        ZoneTileProvider {
            static_data: self.static_data.as_deref(),
            snapshot: &self.snapshot,
            shapes: &self.registry,
            map_id: self.map_id,
        }
    }

    pub fn test_step(&self, x: u16, y: u16, z: i8, direction: Heading) -> Option<i8> {
        if self.static_data.is_none()
            && self.snapshot.get_dynamic_shapes(x, y).is_none()
            && self.registry.is_empty()
        {
            return Some(z);
        }
        let provider = self.make_provider();
        MovementValidator::new(&provider).test_step(x, y, z, direction)
    }

    pub fn resolve_standing_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8> {
        if self.static_data.is_none()
            && self.snapshot.get_dynamic_shapes(x, y).is_none()
            && self.registry.is_empty()
        {
            return Some(z_hint);
        }
        let provider = self.make_provider();
        MovementValidator::new(&provider).resolve_standing_z(x, y, z_hint, direction)
    }

    /// Check line of sight between two 3D points.
    ///
    /// `z1` / `z2` are full Z coordinates including any eye-height offset
    /// (typically `standing_z + 14` for humanoid mobiles).  The caller is
    /// responsible for adding the offset before calling this method.
    ///
    /// Uses the zone's combined tile provider (static map + collision
    /// snapshot + entity registry) — the same data source as
    /// [`test_step`](Self::test_step).
    ///
    /// Returns `true` if nothing blocks the ray between the two points.
    pub fn has_los(
        &self,
        x1: u16, y1: u16, z1: i16,
        x2: u16, y2: u16, z2: i16,
    ) -> bool {
        if self.static_data.is_none()
            && self.snapshot.active_blocks.is_empty()
            && self.registry.is_empty()
        {
            return true;
        }
        let provider = self.make_provider();
        LosValidator::new(&provider).has_los(x1, y1, z1, x2, y2, z2)
    }

    pub fn clear_all(&mut self) {
        self.store.clear();
        self.snapshot = CollisionSnapshot::new();
        self.entity_spatial.clear();
        self.registry.clear_world(self.map_id);
        self.containers.clear();
        self.item_props.clear();
    }

    // -- Snapshot helpers -------------------------------------------------

    /// Collect all entities from the store as `(serial, entity)` pairs.
    ///
    /// Useful for save/restore: iterate every entity in the zone, clone it,
    /// and return as a `Vec`.
    pub fn collect_entities(&self) -> Vec<(u32, E)> {
        self.store.iter().map(|(&id, e)| (id, e.clone())).collect()
    }

    /// Access the static data provider attached to this zone.
    ///
    /// Returns `None` when the zone was created without static data
    /// (e.g. a headless test zone).
    pub fn static_data(&self) -> Option<&Arc<dyn StaticDataProvider>> {
        self.static_data.as_ref()
    }

    /// Build the full collision [`TileBlock`] for a single 8×8 map block.
    ///
    /// Merges three data sources in the same way [`Zone::test_step`] does:
    /// 1. Static map data (land tiles + statics from `.mul`/`.uop` files)
    /// 2. Dynamic collision snapshot (items placed via `spawn`)
    /// 3. Entity registry shapes (multi-object / house parts)
    ///
    /// This is intended for external pathfinders that run outside the zone
    /// thread: request the block(s) via a worker command, cache them, and
    /// run A* without holding any borrow on the zone.
    pub fn query_collision_block(&self, block: BlockKey) -> TileBlock {
        let provider = ZoneTileProvider {
            static_data: self.static_data.as_deref(),
            snapshot: &self.snapshot,
            shapes: &self.registry,
            map_id: self.map_id,
        };
        use crate::ecumene::TileProvider;
        provider.query_block(block)
    }

    /// Batch-fetch collision [`TileBlock`]s for every 8×8 block that
    /// overlaps the given tile rectangle (inclusive, tile coordinates).
    ///
    /// Blocks are returned in row-major order: `bx` is the outer loop,
    /// `by` is the inner loop.  Out-of-range blocks are still included as
    /// [`TileBlock::empty`] so the caller can index them by offset.
    pub fn query_collision_blocks(
        &self,
        tile_left: u16,
        tile_top: u16,
        tile_right: u16,
        tile_bottom: u16,
    ) -> Vec<TileBlock> {
        let bx_min = tile_left  / 8;
        let by_min = tile_top   / 8;
        let bx_max = (tile_right .saturating_add(7)) / 8;
        let by_max = (tile_bottom.saturating_add(7)) / 8;

        let provider = ZoneTileProvider {
            static_data: self.static_data.as_deref(),
            snapshot: &self.snapshot,
            shapes: &self.registry,
            map_id: self.map_id,
        };
        use crate::ecumene::TileProvider;

        let mut blocks = Vec::new();
        for bx in bx_min..=bx_max {
            for by in by_min..=by_max {
                blocks.push(provider.query_block(BlockKey::new(bx, by)));
            }
        }
        blocks
    }
}

/// Zone-specific tile provider that combines static map data, dynamic
/// collision snapshot, and entity registry shapes into a single tile stack.
struct ZoneTileProvider<'a> {
    static_data: Option<&'a dyn StaticDataProvider>,
    snapshot: &'a CollisionSnapshot,
    shapes: &'a dyn ShapeProvider,
    map_id: u8,
}

impl TileProvider for ZoneTileProvider<'_> {
    fn query_tile_stack(&self, x: u16, y: u16, direction: Heading) -> Vec<TileShape> {
        // 1. Static map data (land + statics)
        let base = StaticTileProvider::new(self.static_data, self.map_id);
        let mut shapes = base.query_tile_stack(x, y, direction);

        // 2. Dynamic obstacles from the collision snapshot
        let has_dynamic = if let Some(dynamic) = self.snapshot.get_dynamic_shapes(x, y) {
            shapes.extend(dynamic);
            true
        } else {
            false
        };

        // 3. Entity registry shapes (multi-object parts)
        let registry_shapes = self.shapes.get_shapes_at(x, y);
        let has_registry = !registry_shapes.is_empty();
        if has_registry {
            shapes.extend(registry_shapes);
        }

        // Re-sort if we added dynamic or registry shapes
        if has_dynamic || has_registry {
            shapes.sort_by(|a, b| a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top())));
        }

        shapes
    }

    fn query_block(&self, block: BlockKey) -> TileBlock {
        // 1. Static map data — optimised block-level query.
        let base = StaticTileProvider::new(self.static_data, self.map_id);
        let mut tb = base.query_block(block);

        let origin = block.origin();

        // 2. Dynamic obstacles — one HashMap lookup for the whole block.
        let path_block = self.snapshot.get_path_block(block);

        // 3. Entity registry shapes — per-tile (spatial index is point-based).
        //    We iterate all 64 cells and merge dynamic + registry shapes.
        let mut needs_sort = [false; 64];

        if let Some(pb) = path_block {
            for local_idx in 0..64u8 {
                let ox = local_idx % 8;
                let oy = local_idx / 8;
                let stack = tb.tile_stack_mut(ox, oy);
                let before = stack.len();
                pb.collect_shapes_at(local_idx, stack);
                if stack.len() > before {
                    needs_sort[local_idx as usize] = true;
                }
            }
        }

        if !self.shapes.shapes_empty() {
            for oy in 0..8u8 {
                for ox in 0..8u8 {
                    let x = origin.x + ox as u16;
                    let y = origin.y + oy as u16;
                    let registry_shapes = self.shapes.get_shapes_at(x, y);
                    if !registry_shapes.is_empty() {
                        let stack = tb.tile_stack_mut(ox, oy);
                        stack.extend(registry_shapes);
                        needs_sort[(oy * 8 + ox) as usize] = true;
                    }
                }
            }
        }

        // Re-sort only stacks that received extra shapes.
        for (i, dirty) in needs_sort.iter().enumerate() {
            if *dirty {
                let ox = (i % 8) as u8;
                let oy = (i / 8) as u8;
                let stack = tb.tile_stack_mut(ox, oy);
                stack.sort_by(|a, b| a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top())));
            }
        }

        tb
    }
}
