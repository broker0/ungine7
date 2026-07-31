//! [`TileBlock`] — tile stacks for all 64 tiles in an 8×8 block.
//!
//! Used by [`TileProvider::query_block`](super::tile_provider::TileProvider::query_block)
//! to return direction-agnostic tile data for an entire map block at once.
//! This is useful for pathfinding, area queries, pre-loading, and caching
//! scenarios where per-tile queries would be too granular.
//!
//! Land-tile slopes use averaged vertex Z (direction-independent) rather
//! than the direction-dependent exit Z used by
//! [`TileProvider::query_tile_stack`](super::tile_provider::TileProvider::query_tile_stack).
//! For precise single-step movement validation, use `query_tile_stack`
//! with a specific [`Heading`](u_core::Heading).

use std::array;

use u_core::BlockKey;

use crate::vessel::tile_shape::TileShape;

/// Tile stacks for all 64 tiles in an 8×8 block.
///
/// Indexed by local offset `(ox, oy)` where `ox, oy ∈ [0..8)`.
/// Linear index = `oy * 8 + ox`.
///
/// Tile stacks are sorted by `z_base` ascending, then `z_top` ascending,
/// matching the contract of [`TileProvider::query_tile_stack`](super::tile_provider::TileProvider::query_tile_stack).
#[derive(Clone)]
pub struct TileBlock {
    /// Which block this data belongs to.
    pub block_key: BlockKey,
    /// 64 tile stacks, one per cell in the 8×8 block.
    stacks: [Vec<TileShape>; 64],
}

impl TileBlock {
    /// Create an empty block (all 64 stacks are empty `Vec`s).
    #[inline]
    pub fn empty(block_key: BlockKey) -> Self {
        Self {
            block_key,
            stacks: array::from_fn(|_| Vec::new()),
        }
    }

    /// Linear index for `(ox, oy)`.
    #[inline]
    fn idx(ox: u8, oy: u8) -> usize {
        debug_assert!(ox < 8 && oy < 8, "tile offset out of range: ({ox}, {oy})");
        (oy as usize) * 8 + (ox as usize)
    }

    /// Get the tile stack at local offset `(ox, oy)`.
    #[inline]
    pub fn tile_stack(&self, ox: u8, oy: u8) -> &[TileShape] {
        &self.stacks[Self::idx(ox, oy)]
    }

    /// Get a mutable reference to the tile stack at local offset `(ox, oy)`.
    #[inline]
    pub fn tile_stack_mut(&mut self, ox: u8, oy: u8) -> &mut Vec<TileShape> {
        &mut self.stacks[Self::idx(ox, oy)]
    }

    /// Iterate over all cells, yielding `(ox, oy, &[TileShape])`.
    ///
    /// Iteration order: `oy` outer (0..8), `ox` inner (0..8).
    pub fn iter(&self) -> impl Iterator<Item = (u8, u8, &[TileShape])> {
        self.stacks.iter().enumerate().map(|(i, stack)| {
            let ox = (i % 8) as u8;
            let oy = (i / 8) as u8;
            (ox, oy, stack.as_slice())
        })
    }

    /// Mutable iterate over all cells, yielding `(ox, oy, &mut Vec<TileShape>)`.
    ///
    /// Iteration order: `oy` outer (0..8), `ox` inner (0..8).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (u8, u8, &mut Vec<TileShape>)> {
        self.stacks.iter_mut().enumerate().map(|(i, stack)| {
            let ox = (i % 8) as u8;
            let oy = (i / 8) as u8;
            (ox, oy, stack)
        })
    }

    /// Total number of [`TileShape`] entries across all 64 stacks.
    pub fn total_shapes(&self) -> usize {
        self.stacks.iter().map(|s| s.len()).sum()
    }
}
