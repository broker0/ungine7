//! [`MovementValidator`] — pure UO movement algorithm parameterised by
//! a [`TileProvider`].
//!
//! This is the core movement logic extracted from the former `path_server`.
//! It does not know where tile shapes come from — static map files, a
//! dynamic collision snapshot, or both — and delegates tile queries to the
//! [`TileProvider`] trait.
//!
//! Passability is determined at query time using the raw tiledata flags
//! stored in each [`TileShape`].  Two optional overrides can be set via
//! the builder:
//!
//! - `passable_mask` — if a tile's flags match, the tile is considered
//!   passable even if `IMPASSABLE` is set (e.g. `TileFlags::WET` for sea
//!   creatures).
//! - `can_fly` — flying creatures (gargoyles) snap to tiles that carry the
//!   `HOVER_OVER` flag within a Z tolerance.  Non-flying creatures ignore
//!   those tiles entirely.
//!
//! # Free functions
//!
//! The core algorithms are also available as free functions
//! ([`compute_source_range`], [`compute_dest_position`]) so that callers
//! who already have a `&[TileShape]` (e.g. a block-level cache) can use
//! the same logic without going through a [`TileProvider`].
//!
//! # Usage
//!
//! ```ignore
//! // Ground creature (default):
//! let mv = MovementValidator::new(&provider);
//! let new_z = mv.test_step(x, y, z, Heading::North);
//!
//! // Sea creature (water tiles are passable):
//! let mv = MovementValidator::new(&provider)
//!     .with_passable_mask(TileFlags::WET);
//! let new_z = mv.test_step(x, y, z, Heading::North);
//!
//! // Flying creature (gargoyle):
//! let mv = MovementValidator::new(&provider)
//!     .with_flying(true);
//! let new_z = mv.test_step(x, y, z, Heading::North);
//! ```

use files::tiledata::TileFlags;
use u_core::Heading;

use super::tile_provider::TileProvider;
use crate::vessel::tile_shape::TileShape;

// ── Constants (public for reuse by pathfinding caches) ────────────────────

/// Height a character occupies (in Z units).
pub const CHARACTER_HEIGHT: i16 = 16;

/// Maximum climb per step (in Z units).
pub const CLIMB_HEIGHT: i8 = 2;

/// Snap tolerance for `HOVER_OVER` tiles (gargoyle flight).
///
/// If a flying creature is within this many Z units of a hover-over tile,
/// it snaps to that tile's Z when moving onto the destination cell.
pub const HOVER_SNAP_TOLERANCE: i16 = 25;

// ── Free functions ────────────────────────────────────────────────────────
//
// These operate on pre-fetched `&[TileShape]` slices, making them usable
// from both `MovementValidator` (which fetches via `TileProvider`) and
// block-level caches (which already have the data in memory).

/// Check whether a tile shape is passable given `passable_mask` and
/// `can_fly` overrides.
///
/// - `HOVER_OVER` tiles are passable only for flying creatures.
/// - If `passable_mask` is non-zero and the shape's flags match, the tile
///   is considered passable even if `IMPASSABLE` is set.
/// - Otherwise, the tile is passable when `IMPASSABLE` is *not* set.
#[inline]
pub fn is_shape_passable(shape: &TileShape, passable_mask: u64, can_fly: bool) -> bool {
    match *shape {
        TileShape::Slope { flags, .. } | TileShape::Surface { flags, .. } => {
            if flags & TileFlags::HOVER_OVER != 0 {
                return can_fly;
            }
            if passable_mask != 0 && flags & passable_mask != 0 {
                return true;
            }
            flags & TileFlags::IMPASSABLE == 0
        }
        TileShape::Background { .. } => false,
    }
}

/// Compute the reachable Z range from a source tile stack.
///
/// Returns `(z_low, z_high)` — the lowest point the character can fall to
/// and the highest point they can climb to (including [`CLIMB_HEIGHT`]).
///
/// `HOVER_OVER` tiles do not contribute to this range regardless of
/// `can_fly` — they are only snap targets at the destination.
///
/// `shapes` is the tile stack at the *source* tile (without cap).
pub fn compute_source_range(shapes: &[TileShape], z: i8) -> (i8, i8) {
    let mut z_low_fall = i8::MIN;
    let mut z_high = z;

    for shape in shapes {
        let (z_base, z_stand, z_top, is_slope) = match *shape {
            TileShape::Surface { z_base, z_stand, flags } => {
                if flags & TileFlags::HOVER_OVER != 0 {
                    continue;
                }
                (z_base, z_stand, z_stand, false)
            }
            TileShape::Slope { z_base, z_stand, z_top, .. } => {
                (z_base, z_stand, z_top, true)
            }
            TileShape::Background { .. } => continue,
        };

        if z_stand <= z && z_stand > z_low_fall {
            z_low_fall = z_stand;
        }

        if is_slope && z_stand == z {
            z_low_fall = z_low_fall.min(z_base);
            z_high = z_high.max(z_top);
        }
    }

    (z_low_fall, z_high.saturating_add(CLIMB_HEIGHT))
}

/// Find the best destination Z within the reachable range.
///
/// Scans a destination tile stack for passable surfaces that:
/// - have enough headroom ([`CHARACTER_HEIGHT`] gap above),
/// - are reachable from the source range `[z_low, z_high]`,
/// and picks the one closest to the current `z`.
///
/// For flying creatures, `HOVER_OVER` tiles within [`HOVER_SNAP_TOLERANCE`]
/// Z units of the current `z` trigger an early snap return.
///
/// **Important:** `shapes_with_cap` must include a [`TileShape::cap()`]
/// sentinel as the last element (the caller is responsible for appending
/// it before calling this function).
pub fn compute_dest_position(
    shapes_with_cap: &[TileShape],
    z: i8,
    z_low: i8,
    z_high: i8,
    passable_mask: u64,
    can_fly: bool,
) -> Option<i8> {
    let objects = shapes_with_cap;

    let z = if z < z_low { z_low } else { z };
    let z_high = z_high as i16;
    let mut z_low_cursor: i16 = z_low as i16;
    let mut current_z: i16 = i8::MIN as i16;
    let mut result: Option<i8> = None;

    for (i, upper_obj) in objects.iter().enumerate() {
        let (upper_z_base, upper_z_stand) = match *upper_obj {
            TileShape::Slope { z_base, z_stand, .. } => {
                (z_base as i16, z_stand as i16)
            }
            TileShape::Surface { z_base, z_stand, flags } => {
                if flags & TileFlags::HOVER_OVER != 0 {
                    if can_fly
                        && (z_base as i16 - z as i16).abs() <= HOVER_SNAP_TOLERANCE
                    {
                        return Some(z_base);
                    }
                    continue;
                }
                (z_base as i16, z_stand as i16)
            }
            TileShape::Background { .. } => continue,
        };

        if upper_z_base - z_low_cursor >= CHARACTER_HEIGHT {
            for bottom_obj in objects[..i].iter().rev() {
                let (bottom_z_stand, passable) = match *bottom_obj {
                    TileShape::Slope { z_stand, .. } => {
                        (z_stand as i16, is_shape_passable(bottom_obj, passable_mask, can_fly))
                    }
                    TileShape::Surface { z_stand, flags, .. } => {
                        if flags & TileFlags::HOVER_OVER != 0 {
                            continue;
                        }
                        (z_stand as i16, is_shape_passable(bottom_obj, passable_mask, can_fly))
                    }
                    TileShape::Background { .. } => continue,
                };

                if passable
                    && bottom_z_stand >= current_z
                    && (upper_z_base - bottom_z_stand) >= CHARACTER_HEIGHT
                {
                    let reachable = match *bottom_obj {
                        TileShape::Slope { z_base, .. } => (z_base as i16) <= z_high,
                        TileShape::Surface { z_stand, .. } => (z_stand as i16) <= z_high,
                        TileShape::Background { .. } => unreachable!(),
                    };

                    if !reachable {
                        continue;
                    }

                    match result {
                        Some(best_z) => {
                            let curr_delta = (z as i16 - bottom_z_stand).abs();
                            let prev_delta = (z as i16 - best_z as i16).abs();
                            if curr_delta < prev_delta {
                                result = Some(bottom_z_stand as i8);
                            }
                        }
                        None => result = Some(bottom_z_stand as i8),
                    }
                }
            }
        }

        z_low_cursor = z_low_cursor.max(upper_z_stand);
        current_z = current_z.max(upper_z_stand);
    }

    result
}

// ── MovementValidator ─────────────────────────────────────────────────────

/// Pure movement validator parameterised by a [`TileProvider`].
///
/// Contains no mutable state and can be created cheaply on every call.
pub struct MovementValidator<'a, T: TileProvider> {
    provider: &'a T,
    passable_mask: u64,
    can_fly: bool,
}

impl<'a, T: TileProvider> MovementValidator<'a, T> {
    /// Create a validator backed by the given tile provider.
    ///
    /// By default the validator uses standard ground-creature passability
    /// rules (tiles with `IMPASSABLE` flag block movement, `HOVER_OVER`
    /// tiles are ignored).
    #[inline]
    pub fn new(provider: &'a T) -> Self {
        Self { provider, passable_mask: 0, can_fly: false }
    }

    /// Set an extra passable-override mask.
    ///
    /// If a tile's flags match `mask`, it is considered passable even if
    /// `IMPASSABLE` is set.  For example, pass `TileFlags::WET` to allow
    /// sea creatures to move through water tiles.
    #[inline]
    pub fn with_passable_mask(mut self, mask: u64) -> Self {
        self.passable_mask = mask;
        self
    }

    /// Enable or disable flying mode.
    ///
    /// When `true`, tiles with [`TileFlags::HOVER_OVER`] become valid
    /// landing targets: if the creature is within [`HOVER_SNAP_TOLERANCE`]
    /// Z units of such a tile at the destination, it snaps to that Z.
    /// Non-flying creatures ignore `HOVER_OVER` tiles entirely.
    #[inline]
    pub fn with_flying(mut self, flying: bool) -> Self {
        self.can_fly = flying;
        self
    }

    // ── Public API ───────────────────────────────────────────────────

    /// Test whether a step from `(x, y, z)` in `direction` is possible.
    ///
    /// Returns `Some(new_z)` with the standing Z at the destination tile,
    /// or `None` if the step is blocked.
    ///
    /// For diagonal directions, the two adjacent cardinal tiles are also
    /// checked (UO requires them to be passable for a diagonal move).
    pub fn test_step(&self, x: u16, y: u16, z: i8, direction: Heading) -> Option<i8> {
        let dest_z = self.test_step_single(x, y, z, direction)?;

        if !direction.is_diagonal() {
            return Some(dest_z);
        }

        // Check adjacent cardinal tiles for diagonal movement.
        self.test_step_single(x, y, z, direction.turn(1))?;
        self.test_step_single(x, y, z, direction.turn(-1))?;

        Some(dest_z)
    }

    /// Find the best standing Z at `(x, y)` closest to `z_hint`.
    ///
    /// Scans the tile stack for passable surfaces with enough headroom
    /// (CHARACTER_HEIGHT) and returns the one whose `z_stand` is nearest
    /// to `z_hint`.  Returns `None` if no valid standing position exists.
    pub fn resolve_standing_z(
        &self,
        x: u16,
        y: u16,
        z_hint: i8,
        direction: Heading,
    ) -> Option<i8> {
        let mut shapes = self.provider.query_tile_stack(x, y, direction);
        shapes.push(TileShape::cap());

        let mut best: Option<i8> = None;
        let mut current_z: i16 = i8::MIN as i16;

        for (i, upper) in shapes.iter().enumerate() {
            let upper_z_base = match *upper {
                TileShape::Slope { z_base, .. } => z_base as i16,
                TileShape::Surface { z_base, .. } => z_base as i16,
                TileShape::Background { .. } => continue,
            };

            if upper_z_base - current_z >= CHARACTER_HEIGHT {
                for bottom in shapes[..i].iter().rev() {
                    let bottom_z_stand = match *bottom {
                        TileShape::Slope { z_stand, .. }
                            if self.is_passable(bottom) =>
                            z_stand as i16,
                        TileShape::Surface { z_stand, flags, .. }
                            if self.is_surface_passable(flags, z_stand as i16, z_hint as i16) =>
                            z_stand as i16,
                        _ => continue,
                    };

                    if bottom_z_stand >= current_z
                        && (upper_z_base - bottom_z_stand) >= CHARACTER_HEIGHT
                    {
                        match best {
                            Some(prev_best) => {
                                if (z_hint as i16 - bottom_z_stand).abs()
                                    < (z_hint as i16 - prev_best as i16).abs()
                                {
                                    best = Some(bottom_z_stand as i8);
                                }
                            }
                            None => {
                                best = Some(bottom_z_stand as i8);
                            }
                        }
                    }
                }
            }

            current_z = current_z.max(match *upper {
                TileShape::Slope { z_stand, .. }
                | TileShape::Surface { z_stand, .. } => z_stand as i16,
                TileShape::Background { .. } => current_z,
            });
        }

        best
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Check whether a tile shape is passable given the current mask.
    #[inline]
    fn is_passable(&self, shape: &TileShape) -> bool {
        shape.is_passable_with(self.passable_mask)
    }

    /// Check whether a `Surface` tile with the given `flags` and `z_stand` is
    /// passable, accounting for `HOVER_OVER` and `can_fly`.
    ///
    /// - `HOVER_OVER` tiles are only reachable by flying creatures and only
    ///   within [`HOVER_SNAP_TOLERANCE`] Z units.
    /// - All other tiles follow normal passability rules.
    #[inline]
    fn is_surface_passable(&self, flags: u64, z_stand: i16, z_ref: i16) -> bool {
        if flags & TileFlags::HOVER_OVER != 0 {
            return self.can_fly && (z_stand - z_ref).abs() <= HOVER_SNAP_TOLERANCE;
        }
        if self.passable_mask != 0 && flags & self.passable_mask != 0 {
            return true;
        }
        flags & TileFlags::IMPASSABLE == 0
    }

    /// Test a single step (no diagonal adjacency check).
    fn test_step_single(
        &self,
        x: u16,
        y: u16,
        z: i8,
        direction: Heading,
    ) -> Option<i8> {
        let (dx, dy) = direction.delta();
        let to_x = (x as i32 + dx).clamp(0, u16::MAX as i32) as u16;
        let to_y = (y as i32 + dy).clamp(0, u16::MAX as i32) as u16;

        let source_shapes = self.provider.query_tile_stack(x, y, direction);
        let (z_low, z_high) = compute_source_range(&source_shapes, z);

        let mut dest_shapes = self.provider.query_tile_stack(to_x, to_y, Heading::North);
        dest_shapes.push(TileShape::cap());
        compute_dest_position(&dest_shapes, z, z_low, z_high, self.passable_mask, self.can_fly)
    }
}
