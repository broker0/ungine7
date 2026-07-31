//! [`ShapeProvider`] trait — abstraction over dynamic tile-collision sources.
//!
 //! [`CompositeTileProvider`](crate::diorama::composite_tiles::CompositeTileProvider) layers
//! dynamic shapes from a `ShapeProvider` on top of static map data for
//! movement validation.  The concrete backing store is typically an
//! [`EntityRegistry`](super::entity_registry::EntityRegistry).

use crate::vessel::tile_shape::TileShape;

/// Source of dynamic tile-collision shapes (multi-objects, cached items, etc.).
///
/// Implementations provide collision shapes for tiles that are not part of
/// the static map data — e.g. multi-object parts (houses, boats) or cached
/// items that persist beyond the client's view rectangle.
pub trait ShapeProvider {
    /// Collision shapes at tile `(x, y)` for the current world context.
    ///
    /// The returned shapes are **not** required to be sorted — the caller
    /// is responsible for merging and sorting with other tile data.
    fn get_shapes_at(&self, x: u16, y: u16) -> Vec<TileShape>;

    /// `true` when the provider contains no shapes at all.
    ///
    /// Used as an optimisation gate to skip expensive per-tile loops when
    /// there is nothing to contribute.
    fn shapes_empty(&self) -> bool;
}
