//! [`ZResolver`] — trait for resolving standing Z at a given tile.
//!
//! This trait decouples movement tracking from the concrete tile-data
//! implementation.  The [`rythmos`](super) module uses it to adjust the
//! player's Z coordinate after a step without depending on
 //! `CompositeTileProvider`, `SessionView`, or any other diorama-specific type.

use u_core::Heading;

/// Resolves the standing Z at a given (x, y) tile using available world
/// data (terrain, visible items, multi shapes, etc.).
///
/// Implementations live outside the `rythmos` module — e.g.
 /// `CompositeTileProvider` in the `diorama` module.
pub trait ZResolver {
    /// Find the best standing Z at `(x, y)` closest to `z_hint`, taking
    /// the movement `direction` into account for slope traversal.
    ///
    /// Returns `Some(z)` if a valid standing position exists, `None`
    /// otherwise.
    fn resolve_standing_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8>;
}
