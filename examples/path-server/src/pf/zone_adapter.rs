//! [`ZoneTileAdapter`] — wraps a [`Zone`] as a [`TileProvider`].
//!
//! The adapter combines:
//! 1. Static map data (land tiles + statics) via [`StaticTileProvider`]
//! 2. Dynamic collision shapes from the zone's [`framework::ecumene::CollisionSnapshot`]
//! 3. Entity registry shapes (multi-object parts) from the zone's registry
//!
//! This is the same data that [`Zone::test_step`] uses internally, exposed
//! as a [`TileProvider`] so the A* surveyor sees all obstacles — not just
//! the static map.

use u_core::{BlockKey, Heading};

use framework::continuum::container::ZoneContainers;
use framework::continuum::Zone;
use framework::ecumene::{StaticTileProvider, TileBlock, TileProvider};
use framework::vessel::objects::Entity as VesselEntity;
use framework::vessel::tile_shape::TileShape;

// ── ZoneTileAdapter ───────────────────────────────────────────────────────

/// Borrow-based adapter that exposes a `&Zone` as a [`TileProvider`].
///
/// Lifetime `'z` is tied to the zone borrow.  Create this adapter at the
/// start of a pathfinding call, use it for the duration of the search,
/// then drop it before the next zone mutation.
///
/// Full-zone `TileProvider` for the A* surveyor; the implementation is
/// complete but not yet wired into the active pathfinding pipeline (the
/// surveyor currently builds its provider differently).
#[allow(dead_code)]
pub struct ZoneTileAdapter<'z, E, C>
where
    E: VesselEntity + Clone,
    C: ZoneContainers,
{
    zone: &'z Zone<E, C>,
}

impl<'z, E, C> ZoneTileAdapter<'z, E, C>
where
    E: VesselEntity + Clone,
    C: ZoneContainers,
{
    /// Create an adapter over the given zone.
    #[allow(dead_code)]
    pub fn new(zone: &'z Zone<E, C>) -> Self {
        Self { zone }
    }
}

impl<E, C> TileProvider for ZoneTileAdapter<'_, E, C>
where
    E: VesselEntity + Clone,
    C: ZoneContainers,
{
    fn query_tile_stack(&self, x: u16, y: u16, direction: Heading) -> Vec<TileShape> {
        use framework::ecumene::ShapeProvider;

        // 1. Static map data
        let static_provider = StaticTileProvider::new(
            self.zone.static_data().map(|sd| sd.as_ref()
                as &dyn framework::vessel::traits::StaticDataProvider),
            self.zone.map_id,
        );
        let mut shapes = static_provider.query_tile_stack(x, y, direction);

        // 2. Dynamic collision snapshot (items placed on the map)
        if let Some(dynamic) = self.zone.snapshot.get_dynamic_shapes(x, y) {
            shapes.extend(dynamic);
        }

        // 3. Entity registry shapes (multi-object parts)
        let registry_shapes = self.zone.registry.get_shapes_at(x, y);
        if !registry_shapes.is_empty() {
            shapes.extend(registry_shapes);
        }

        // Re-sort if we added anything beyond the base
        if self.zone.snapshot.active_blocks.is_empty()
            && self.zone.registry.shapes_empty()
        {
            // Nothing dynamic — already sorted
        } else {
            shapes.sort_by(|a, b| {
                a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top()))
            });
        }

        shapes
    }

    fn query_block(&self, block: BlockKey) -> TileBlock {
        use framework::ecumene::ShapeProvider;

        // 1. Static map data — optimised block-level query
        let static_provider = StaticTileProvider::new(
            self.zone.static_data().map(|sd| sd.as_ref()
                as &dyn framework::vessel::traits::StaticDataProvider),
            self.zone.map_id,
        );
        let mut tb = static_provider.query_block(block);

        let origin = block.origin();
        let mut needs_sort = [false; 64];

        // 2. Dynamic collision snapshot
        if let Some(pb) = self.zone.snapshot.get_path_block(block) {
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

        // 3. Entity registry shapes (multi-object parts)
        if !self.zone.registry.shapes_empty() {
            for oy in 0..8u8 {
                for ox in 0..8u8 {
                    let x = origin.x + ox as u16;
                    let y = origin.y + oy as u16;
                    let registry_shapes = self.zone.registry.get_shapes_at(x, y);
                    if !registry_shapes.is_empty() {
                        let stack = tb.tile_stack_mut(ox, oy);
                        stack.extend(registry_shapes);
                        needs_sort[(oy * 8 + ox) as usize] = true;
                    }
                }
            }
        }

        // Re-sort dirty stacks
        for (i, &dirty) in needs_sort.iter().enumerate() {
            if dirty {
                let ox = (i % 8) as u8;
                let oy = (i / 8) as u8;
                let stack = tb.tile_stack_mut(ox, oy);
                stack.sort_by(|a, b| {
                    a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top()))
                });
            }
        }

        tb
    }
}
