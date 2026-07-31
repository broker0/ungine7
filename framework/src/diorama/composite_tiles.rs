//! [`CompositeTileProvider`] — tile data from the observer's perspective.
//!
//! Combines static map data ([`StaticTileProvider`]), visible items from
//! a [`VisibleWorld`], and dynamic shapes from a [`ShapeProvider`] into a
//! single [`TileProvider`] suitable for client-side movement validation.
//!
//! When a [`DiffOverlay`] is present, map/statics diffs are applied
//! transparently through [`StaticTileProvider`]'s diff-aware path.
//!
//! # Usage
//!
//! ```ignore
 //! let provider = CompositeTileProvider::new(
//!     &static_data,
//!     map_id,
//!     &visible_world,
//!     &entity_registry,   // implements ShapeProvider
//! );
//! let mv = MovementValidator::new(&provider);
//! let new_z = mv.test_step(x, y, z, Heading::North);
//! ```

use u_core::{BlockKey, Heading};

use crate::rythmos::ZResolver;
use crate::ecumene::DiffOverlay;
use crate::ecumene::MovementValidator;
use crate::ecumene::ShapeProvider;
use crate::ecumene::StaticTileProvider;
use crate::ecumene::TileBlock;
use crate::ecumene::TileProvider;
use crate::vessel::TileShape;
use crate::vessel::StaticDataProvider;
use super::visible_world::VisibleWorld;

/// Tile provider that layers visible-world items and dynamic shapes
/// on top of the static map data.
///
/// Query path:
/// 1. [`StaticTileProvider`] — land + statics from map files (with optional
///    diff overlay for server-patched blocks).
/// 2. [`VisibleWorld`] — items the client currently sees (non-mobile,
///    non-multi objects contribute collision shapes).
/// 3. [`ShapeProvider`] — multi-object parts and cached items covering
///    the tile.
/// 4. Merge and sort bottom-to-top.
pub struct CompositeTileProvider<'a, P: ShapeProvider> {
    base: StaticTileProvider<'a>,
    visible: &'a VisibleWorld,
    shapes: &'a P,
    static_data: &'a dyn StaticDataProvider,
}

impl<'a, P: ShapeProvider> CompositeTileProvider<'a, P> {
    /// Create a new diorama tile provider.
    ///
    /// - `static_data` — shared immutable world data (tiledata, map, statics).
    /// - `world` — current map index (0 = Felucca, …).
    /// - `visible` — the session's visible world.
    /// - `shapes` — dynamic shape source (e.g. [`EntityRegistry`](crate::ecumene::EntityRegistry)).
    pub fn new(
        static_data: &'a dyn StaticDataProvider,
        world: u8,
        visible: &'a VisibleWorld,
        shapes: &'a P,
    ) -> Self {
        Self {
            base: StaticTileProvider::new(Some(static_data), world),
            visible,
            shapes,
            static_data,
        }
    }

    /// Create a diorama tile provider with diff overlay support.
    ///
    /// When `diff_overlay` is `Some` and contains diffs for the current
    /// world, overridden map/statics blocks are read from the overlay
    /// instead of the base static data.
    pub fn with_diff(
        static_data: &'a dyn StaticDataProvider,
        world: u8,
        visible: &'a VisibleWorld,
        shapes: &'a P,
        diff_overlay: Option<&'a DiffOverlay>,
    ) -> Self {
        Self {
            base: StaticTileProvider::with_diff(Some(static_data), world, diff_overlay),
            visible,
            shapes,
            static_data,
        }
    }
}

impl<P: ShapeProvider> TileProvider for CompositeTileProvider<'_, P> {
    fn query_tile_stack(&self, x: u16, y: u16, direction: Heading) -> Vec<TileShape> {
        // 1. Static map data (land + statics), diff-aware if overlay present
        let mut shapes = self.base.query_tile_stack(x, y, direction);
        let base_len = shapes.len();

        // 2. Visible items (non-mobile, non-multi)
        for item in self.visible.items_at(x, y) {
            if item.is_mobile() || item.is_multi() {
                continue;
            }
            if let Some(def) = self.static_data.static_tile_def(item.graphic()) {
                shapes.push(TileShape::from_static(item.z(), def));
            }
        }

        // 3. Dynamic shapes (multi-object parts, cached items, etc.)
        let dynamic_shapes = self.shapes.get_shapes_at(x, y);
        if !dynamic_shapes.is_empty() {
            shapes.extend(dynamic_shapes);
        }

        // Re-sort if we added anything beyond the base
        if shapes.len() > base_len {
            shapes.sort_by(|a, b| {
                a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top()))
            });
        }

        shapes
    }

    fn query_block(&self, block: BlockKey) -> TileBlock {
        // 1. Static map data — optimised block-level query (diff-aware).
        let mut tb = self.base.query_block(block);

        let origin = block.origin();
        let mut needs_sort = [false; 64];

        // 2. Visible items (non-mobile, non-multi) — single pass over
        //    items in the block rather than 64 individual items_at calls.
        for (ox, oy, item) in self.visible.items_in_block(block) {
            if item.is_mobile() || item.is_multi() {
                continue;
            }
            if let Some(def) = self.static_data.static_tile_def(item.graphic()) {
                tb.tile_stack_mut(ox, oy).push(TileShape::from_static(item.z(), def));
                needs_sort[(oy * 8 + ox) as usize] = true;
            }
        }

        // 3. Dynamic shapes — per-tile (spatial index is point-based).
        if !self.shapes.shapes_empty() {
            for oy in 0..8u8 {
                for ox in 0..8u8 {
                    let x = origin.x + ox as u16;
                    let y = origin.y + oy as u16;
                    let dynamic_shapes = self.shapes.get_shapes_at(x, y);
                    if !dynamic_shapes.is_empty() {
                        let stack = tb.tile_stack_mut(ox, oy);
                        stack.extend(dynamic_shapes);
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

impl<P: ShapeProvider> ZResolver for CompositeTileProvider<'_, P> {
    fn resolve_standing_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8> {
        MovementValidator::new(self).resolve_standing_z(x, y, z_hint, direction)
    }
}
