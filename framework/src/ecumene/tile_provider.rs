//! [`TileProvider`] trait — abstraction over tile data sources.
//!
//! The movement continuum does not care *where* tile shapes come from — static
//! map data, a collision snapshot of dynamic objects, or a combination of
//! both.  This trait provides the methods that
//! [`MovementValidator`](super::movement::MovementValidator) and other
//! consumers need.
//!
//! Two levels of granularity are available:
//!
//! - [`query_tile_stack`](TileProvider::query_tile_stack) — single tile,
//!   direction-aware (used by movement validation).
//! - [`query_block`](TileProvider::query_block) — entire 8×8 block,
//!   direction-agnostic (used by pathfinding, area queries, caching).

use u_core::{BlockKey, Heading};

use super::tile_block::TileBlock;
use crate::vessel::tile_shape::TileShape;

/// Source of [`TileShape`] data for a given tile coordinate.
///
/// Implementations may read from static map files, a dynamic collision
/// snapshot, or combine multiple sources.  The returned vector must be
/// sorted bottom-to-top by `z_base` / `z_top`.
pub trait TileProvider {
    /// Collect all [`TileShape`] entries for the tile at `(x, y)`.
    ///
    /// `direction` is needed for land-tile vertex interpolation (the
    /// standing Z of a sloped land tile depends on which edge the
    /// character approaches from).
    ///
    /// The result **must** be sorted by `z_base` ascending, then `z_top`
    /// ascending.
    fn query_tile_stack(&self, x: u16, y: u16, direction: Heading) -> Vec<TileShape>;

    /// Collect tile stacks for all 64 tiles in an 8×8 block.
    ///
    /// This is the **direction-agnostic** bulk query.  Land-tile slopes
    /// use averaged vertex Z rather than the direction-dependent exit Z
    /// computed by [`query_tile_stack`](Self::query_tile_stack).  For
    /// precise per-step movement validation, use `query_tile_stack` with
    /// a specific [`Heading`].
    ///
    /// The default implementation calls `query_tile_stack` 64 times with
    /// [`Heading::North`] as the direction.  Providers that store data at
    /// block granularity (e.g. map files, collision snapshots) should
    /// override this for better performance.
    ///
    /// Each tile stack in the returned [`TileBlock`] is sorted by
    /// `z_base` ascending, then `z_top` ascending.
    fn query_block(&self, block: BlockKey) -> TileBlock {
        let origin = block.origin();
        let mut tb = TileBlock::empty(block);
        for oy in 0..8u8 {
            for ox in 0..8u8 {
                let x = origin.x + ox as u16;
                let y = origin.y + oy as u16;
                *tb.tile_stack_mut(ox, oy) = self.query_tile_stack(x, y, Heading::North);
            }
        }
        tb
    }
}
