//! [`StaticTileProvider`] — tile data from static map files only.
//!
//! When a [`DiffOverlay`] is provided,
//! overridden blocks are read from the overlay instead of the base
//! [`StaticDataProvider`].  A [`DiffAwareDataProvider`] is created
//! internally for this purpose.

use u_core::{BlockKey, Heading};
use log::trace;

use super::diff_overlay::DiffOverlay;
use super::diff_provider::DiffAwareDataProvider;
use super::tile_block::TileBlock;
use super::tile_provider::TileProvider;
use crate::vessel::tile_shape::TileShape;
use crate::vessel::traits::StaticDataProvider;

pub struct StaticTileProvider<'a> {
    pub static_data: Option<&'a dyn StaticDataProvider>,
    pub world: u8,
    pub diff_overlay: Option<&'a DiffOverlay>,
}

impl<'a> StaticTileProvider<'a> {
    pub fn new(
        static_data: Option<&'a dyn StaticDataProvider>,
        world: u8,
    ) -> Self {
        Self { static_data, world, diff_overlay: None }
    }

    /// Create a provider with diff overlay support.
    pub fn with_diff(
        static_data: Option<&'a dyn StaticDataProvider>,
        world: u8,
        diff_overlay: Option<&'a DiffOverlay>,
    ) -> Self {
        Self { static_data, world, diff_overlay }
    }

    /// Build a [`DiffAwareDataProvider`] if we have both base data and
    /// a non-empty diff overlay; otherwise return `None`.
    fn diff_provider(&self) -> Option<DiffAwareDataProvider<'a>> {
        let sd = self.static_data?;
        let overlay = self.diff_overlay?;
        if overlay.map_diff(self.world).is_none()
            && overlay.static_diff(self.world).is_none()
        {
            return None;
        }
        Some(DiffAwareDataProvider::new(sd, overlay, self.world))
    }
}

impl TileProvider for StaticTileProvider<'_> {
    fn query_tile_stack(&self, x: u16, y: u16, direction: Heading) -> Vec<TileShape> {
        let mut shapes = Vec::with_capacity(16);

        let Some(sd) = self.static_data else { return shapes; };

        // Try diff-aware path if overlay is active.
        if let Some(dap) = self.diff_provider() {
            trace!(
                "[tile] query_tile_stack({x}, {y}): using diff-aware path for world {}",
                self.world,
            );
            // 1. Land tile (diff-aware vertex Z).
            if let Some((z_base, z_stand, z_top)) = dap.land_z_stand(x, y, direction) {
                if let Some(tile) = dap.land_tile_at(x, y) {
                    let def = sd.land_tile_def(tile.tile_id);
                    shapes.push(TileShape::from_land(
                        z_base, z_stand, z_top, tile.tile_id, def,
                    ));
                }
            }

            // 2. Statics (diff-aware).
            if let Some(statics) = dap.statics_at(x, y) {
                for st in statics {
                    if let Some(def) = sd.static_tile_def(st.tile_id) {
                        shapes.push(TileShape::from_static(st.z, def));
                    }
                }
            }

            shapes.sort_by(|a, b| a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top())));
            return shapes;
        }

        // ── Non-diff path (original logic) ───────────────────────────

        // 1. Land tile
        if let Some((z_base, z_stand, z_top)) =
            sd.land_tile_z_stand(self.world, x, y, direction)
        {
            if let Some(tile) = sd.land_tile_at(self.world, x, y) {
                let def = sd.land_tile_def(tile.tile_id);
                shapes.push(TileShape::from_land(
                    z_base, z_stand, z_top, tile.tile_id, def,
                ));
            }
        }

        // 2. Statics
        if let Some(statics) = sd.statics_at(self.world, x, y) {
            for st in statics {
                if let Some(def) = sd.static_tile_def(st.tile_id) {
                    shapes.push(TileShape::from_static(st.z, def));
                }
            }
        }

        shapes.sort_by(|a, b| a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top())));
        shapes
    }

    fn query_block(&self, block: BlockKey) -> TileBlock {
        let mut tb = TileBlock::empty(block);

        let Some(sd) = self.static_data else { return tb; };

        let origin = block.origin();

        // Try diff-aware path if overlay is active.
        if let Some(dap) = self.diff_provider() {
            trace!(
                "[tile] query_block({}, {}): using diff-aware path for world {}",
                block.bx, block.by, self.world,
            );
            // 1. Land tiles — diff-aware block lookup.
            if let Some(map_block) = dap.land_block_at(block) {
                for ox in 0..8u8 {
                    for oy in 0..8u8 {
                        let tile = &map_block.cells[ox as usize][oy as usize];
                        let abs_x = origin.x + ox as u16;
                        let abs_y = origin.y + oy as u16;

                        if let Some((z_base, z_stand, z_top)) =
                            dap.land_z_range(abs_x, abs_y)
                        {
                            let def = sd.land_tile_def(tile.tile_id);
                            tb.tile_stack_mut(ox, oy).push(
                                TileShape::from_land(z_base, z_stand, z_top, tile.tile_id, def),
                            );
                        }
                    }
                }
            }

            // 2. Statics — diff-aware block lookup.
            if let Some(statics) = dap.statics_block_at(block) {
                for st in statics {
                    if let Some(def) = sd.static_tile_def(st.tile_id) {
                        tb.tile_stack_mut(st.x, st.y).push(TileShape::from_static(st.z, def));
                    }
                }
            }

            // Sort each non-empty stack.
            for (_, _, stack) in tb.iter_mut() {
                if stack.len() > 1 {
                    stack.sort_by(|a, b| a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top())));
                }
            }

            return tb;
        }

        // ── Non-diff path (original logic) ───────────────────────────

        // 1. Land tiles — one block lookup, then iterate 8×8 cells.
        //    MapBlock cells are stored column-major: cells[ox][oy].
        if let Some(map_block) = sd.land_block_at(self.world, block) {
            for ox in 0..8u8 {
                for oy in 0..8u8 {
                    let tile = &map_block.cells[ox as usize][oy as usize];
                    let abs_x = origin.x + ox as u16;
                    let abs_y = origin.y + oy as u16;

                    // Direction-agnostic Z range.
                    if let Some((z_base, z_stand, z_top)) =
                        sd.land_tile_z_range(self.world, abs_x, abs_y)
                    {
                        let def = sd.land_tile_def(tile.tile_id);
                        tb.tile_stack_mut(ox, oy).push(
                            TileShape::from_land(z_base, z_stand, z_top, tile.tile_id, def),
                        );
                    }
                }
            }
        }

        // 2. Statics — one block lookup, iterate all tiles in the block.
        //    Static tiles carry their (x, y) offset within the block.
        if let Some(statics) = sd.statics_block_at(self.world, block) {
            for st in statics {
                if let Some(def) = sd.static_tile_def(st.tile_id) {
                    tb.tile_stack_mut(st.x, st.y).push(TileShape::from_static(st.z, def));
                }
            }
        }

        // Sort each non-empty stack.
        for (_, _, stack) in tb.iter_mut() {
            if stack.len() > 1 {
                stack.sort_by(|a, b| a.z_base().cmp(&b.z_base()).then(a.z_top().cmp(&b.z_top())));
            }
        }

        tb
    }
}
