//! [`DiffAwareDataProvider`] — queries that consult a [`DiffOverlay`]
//! before falling back to a base [`StaticDataProvider`].
//!
//! This is a short-lived helper struct, created on-the-fly when building
//! tile stacks.  It is **not** a [`StaticDataProvider`] implementor
//! (that trait requires `Send + Sync + 'static` which conflicts with
//! borrowed overlay references).  Instead, it exposes the same query
//! methods and is used directly by tile providers.
//!
//! # Usage
//!
//! ```ignore
//! let dap = DiffAwareDataProvider::new(&*static_data, &diff_overlay, world);
//! let tile = dap.land_tile_at(x, y);
//! let (z_base, z_stand, z_top) = dap.land_z_stand(x, y, direction)?;
//! let statics = dap.statics_at(x, y);
//! ```

use u_core::{BlockKey, Heading};
use files::map::{MapBlock, MapTile};
use files::statics::StaticTile;
use log::trace;

use super::diff_overlay::DiffOverlay;
use super::land_z::{compute_land_z_range, compute_land_z_stand};
use crate::vessel::traits::StaticDataProvider;

/// Short-lived data accessor that layers [`DiffOverlay`] on top of a
/// base [`StaticDataProvider`].
///
/// For every block-level or tile-level query the overlay is checked first;
/// if the block is not overridden, the base provider is consulted.
pub struct DiffAwareDataProvider<'a> {
    base: &'a dyn StaticDataProvider,
    overlay: &'a DiffOverlay,
    world: u8,
}

impl<'a> DiffAwareDataProvider<'a> {
    /// Create a new diff-aware provider.
    ///
    /// - `base` — immutable shared world data.
    /// - `overlay` — per-session mutable diff storage.
    /// - `world` — current map index (0 = Felucca, …).
    pub fn new(
        base: &'a dyn StaticDataProvider,
        overlay: &'a DiffOverlay,
        world: u8,
    ) -> Self {
        Self { base, overlay, world }
    }

    /// Whether the overlay has any diffs for the current world.
    pub fn has_diffs(&self) -> bool {
        self.overlay.map_diff(self.world).is_some()
            || self.overlay.static_diff(self.world).is_some()
    }

    // ── Land tiles ────────────────────────────────────────────────────

    /// Get the land tile at absolute tile coordinates.
    ///
    /// If the block containing `(x, y)` is overridden in the diff,
    /// returns the tile from the diff block.  Otherwise falls back to
    /// the base provider.
    pub fn land_tile_at(&self, x: u16, y: u16) -> Option<&MapTile> {
        if let Some(map_diff) = self.overlay.map_diff(self.world) {
            let bx = (x / 8) as usize;
            let by = (y / 8) as usize;
            let y_blocks = self.y_blocks();
            let block_index = bx * y_blocks + by;
            if let Some(block) = map_diff.get(block_index) {
                let tx = (x % 8) as usize;
                let ty = (y % 8) as usize;
                trace!(
                    "[diff] land_tile_at({x}, {y}): using diff block {block_index} \
                     (bx={bx}, by={by}), tile_id={:#06X}, z={}",
                    block.cells[tx][ty].tile_id,
                    block.cells[tx][ty].z,
                );
                return Some(&block.cells[tx][ty]);
            }
        }
        self.base.land_tile_at(self.world, x, y)
    }

    /// Get the raw vertex Z at a tile coordinate.
    ///
    /// Consults the diff overlay first, then the base provider.
    fn vertex_z(&self, x: u16, y: u16) -> Option<i8> {
        self.land_tile_at(x, y).map(|t| t.z)
    }

    /// Compute direction-dependent `(z_base, z_stand, z_top)` for the
    /// land tile at `(x, y)`, using diff-aware vertex lookups.
    pub fn land_z_stand(
        &self,
        x: u16,
        y: u16,
        direction: Heading,
    ) -> Option<(i8, i8, i8)> {
        let left   = self.vertex_z(x,     y    )?;
        let bottom = self.vertex_z(x + 1, y    )?;
        let right  = self.vertex_z(x + 1, y + 1)?;
        let top    = self.vertex_z(x,     y + 1)?;

        Some(compute_land_z_stand(left, bottom, right, top, direction))
    }

    /// Compute direction-agnostic `(z_base, z_stand, z_top)` for the
    /// land tile at `(x, y)`, using diff-aware vertex lookups.
    pub fn land_z_range(&self, x: u16, y: u16) -> Option<(i8, i8, i8)> {
        let left   = self.vertex_z(x,     y    )?;
        let bottom = self.vertex_z(x + 1, y    )?;
        let right  = self.vertex_z(x + 1, y + 1)?;
        let top    = self.vertex_z(x,     y + 1)?;

        Some(compute_land_z_range(left, bottom, right, top))
    }

    // ── Land blocks ───────────────────────────────────────────────────

    /// Get a land block by block coordinates.
    ///
    /// If the block is overridden in the diff, returns the diff block.
    /// Otherwise falls back to the base provider.
    pub fn land_block_at(&self, block: BlockKey) -> Option<&MapBlock> {
        if let Some(map_diff) = self.overlay.map_diff(self.world) {
            let y_blocks = self.y_blocks();
            let block_index = block.bx as usize * y_blocks + block.by as usize;
            if let Some(blk) = map_diff.get(block_index) {
                trace!(
                    "[diff] land_block_at({}, {}): using diff block {block_index}",
                    block.bx, block.by,
                );
                return Some(blk);
            }
        }
        self.base.land_block_at(self.world, block)
    }

    // ── Statics ───────────────────────────────────────────────────────

    /// Get static tiles at absolute tile coordinates.
    ///
    /// If the block containing `(x, y)` is overridden in the diff,
    /// returns tiles from the diff.  Otherwise falls back to the base.
    pub fn statics_at(&self, x: u16, y: u16) -> Option<&[StaticTile]> {
        if let Some(static_diff) = self.overlay.static_diff(self.world) {
            let bx = (x / 8) as usize;
            let by = (y / 8) as usize;
            let y_blocks = self.y_blocks();
            let block_index = bx * y_blocks + by;
            let ox = (x % 8) as u8;
            let oy = (y % 8) as u8;
            if let Some(tiles) = static_diff.get_tile(block_index, ox, oy) {
                trace!(
                    "[diff] statics_at({x}, {y}): using diff block {block_index}, \
                     {} tiles at ({ox}, {oy})",
                    tiles.len(),
                );
                return Some(tiles);
            }
        }
        self.base.statics_at(self.world, x, y)
    }

    /// Get all static tiles in a block.
    ///
    /// If the block is overridden in the diff, returns diff tiles.
    /// Otherwise falls back to the base provider.
    pub fn statics_block_at(&self, block: BlockKey) -> Option<&[StaticTile]> {
        if let Some(static_diff) = self.overlay.static_diff(self.world) {
            let y_blocks = self.y_blocks();
            let block_index = block.bx as usize * y_blocks + block.by as usize;
            if let Some(tiles) = static_diff.get(block_index) {
                trace!(
                    "[diff] statics_block_at({}, {}): using diff block {block_index}, \
                     {} tiles",
                    block.bx, block.by, tiles.len(),
                );
                return Some(tiles);
            }
        }
        self.base.statics_block_at(self.world, block)
    }

    // ── Helpers ───────────────────────────────────────────────────────

    /// Get the height of the map in blocks for the current world.
    ///
    /// Derived from map tile dimensions (divides tile height by 8).
    fn y_blocks(&self) -> usize {
        self.base
            .map_tile_dimensions(self.world)
            .map(|(_, h)| (h / 8) as usize)
            .unwrap_or(512) // fallback to Felucca/Trammel
    }
}
