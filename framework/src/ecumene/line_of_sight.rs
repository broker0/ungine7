//! [`LosRay`] and [`LosValidator`] — 3D line-of-sight primitives.
//!
//! [`LosRay`] is a zero-allocation iterator that walks a 3D Bresenham
//! ray between two world points, yielding `(x, y, z)` for each tile
//! along the path.  It stores only a handful of integers (~56 bytes on
//! the stack) and does no heap work.
//!
//! [`LosValidator`] is a higher-level consumer that pairs a `LosRay`
//! with a [`TileProvider`] to answer the question "is there a clear
//! line of sight between two points?"
//!
//! Both types are intentionally stateless and allocation-light — they
//! can be created cheaply on every call, just like
//! [`MovementValidator`](super::movement::MovementValidator).
//!
//! # Blocking rules
//!
//! A [`TileShape`](crate::vessel::tile_shape::TileShape) blocks LOS when [`TileShape::blocks_los_with`](crate::vessel::tile_shape::TileShape::blocks_los_with) returns
//! `true` **and** the ray's interpolated Z at that tile intersects the
//! shape's vertical extent `[z_base, z_top]`.
//!
//! # Usage
//!
//! ```ignore
//! use framework::ecumene::{LosValidator, LosRay};
//!
//! // High-level: does anything block the ray?
//! let los = LosValidator::new(&provider);
//! let can_see = los.has_los(
//!     mob_a.x, mob_a.y, mob_a.z as i16 + 14,
//!     mob_b.x, mob_b.y, mob_b.z as i16 + 14,
//! );
//!
//! // Low-level: iterate tiles along the ray
//! for (x, y, z) in LosRay::new(0, 0, 10, 20, 15, 10) {
//!     println!("ray passes through ({x}, {y}) at z={z}");
//! }
//!
//! // Include start/end tiles:
//! let ray = LosRay::new(0, 0, 10, 5, 0, 10).with_endpoints(true);
//! let all_tiles: Vec<_> = ray.collect();
//! ```

use u_core::Heading;

use super::tile_provider::TileProvider;

// ── LosTrace ──────────────────────────────────────────────────────────────

/// Result of a full LOS ray trace with per-tile blocking information.
///
/// Unlike [`LosValidator::has_los`] which returns a simple `bool`, this
/// struct preserves the entire ray path and records which tiles block
/// the ray.
///
/// # Example
///
/// ```ignore
/// let trace = LosValidator::new(&provider).trace(x1, y1, z1, x2, y2, z2);
/// if trace.has_los {
///     println!("Clear LOS, {} intermediate tiles", trace.tiles.len());
/// } else {
///     let first = trace.blockers[0];
///     let (bx, by, bz) = trace.tiles[first];
///     println!("First blocked at tile ({bx}, {by}, {bz}), {} blockers total", trace.blockers.len());
/// }
/// ```
#[derive(Debug, Clone)]
pub struct LosTrace {
    /// All intermediate tiles along the ray: `(x, y, z)`.
    ///
    /// Start and end tiles are **not** included (standard UO semantics).
    pub tiles: Vec<(u16, u16, i16)>,
    /// Indices into [`tiles`](Self::tiles) of **all** tiles that block LOS,
    /// in ray-traversal order.
    ///
    /// Empty when the ray is unobstructed.
    pub blockers: Vec<usize>,
    /// Convenience flag: `true` when `blockers` is empty.
    pub has_los: bool,
}

impl LosTrace {
    /// Index of the first blocking tile, if any.
    #[inline]
    pub fn first_blocker(&self) -> Option<usize> {
        self.blockers.first().copied()
    }
}

// ── LosRay ────────────────────────────────────────────────────────────────

/// 3D Bresenham ray iterator over tile coordinates.
///
/// Yields `(x: u16, y: u16, z: i16)` for each tile along the ray from
/// start to end.  By default, start and end tiles are **skipped**
/// (standard UO LOS semantics); call [`.with_endpoints(true)`](Self::with_endpoints)
/// to include them.
///
/// The iterator is allocation-free and stores only integer state on the
/// stack (~56 bytes).
///
/// # Example
///
/// ```ignore
/// // Count intermediate tiles between two points:
/// let count = LosRay::new(0, 0, 10, 8, 5, 10).count();
///
/// // Find first blocking tile:
/// let blocker = LosRay::new(x1, y1, z1, x2, y2, z2)
///     .find(|&(x, y, z)| is_blocked(x, y, z));
///
/// // Collect all tiles including endpoints:
/// let tiles: Vec<_> = LosRay::new(x1, y1, z1, x2, y2, z2)
///     .with_endpoints(true)
///     .collect();
/// ```
pub struct LosRay {
    // ── Current position ─────────────────────────────────────────────
    cur_x: i32,
    cur_y: i32,

    // ── End point ────────────────────────────────────────────────────
    end_x: i32,
    end_y: i32,

    // ── Z interpolation ──────────────────────────────────────────────
    z1: i32,
    dz: i32,

    // ── Bresenham state ──────────────────────────────────────────────
    error: i32,
    /// Total number of steps (`max(|dx|, |dy|)`).
    steps: u32,
    /// Current step index (0 = start tile, `steps` = end tile).
    step: u32,

    // ── Direction increments ─────────────────────────────────────────
    sx: i32,
    sy: i32,
    abs_major: u32,
    abs_minor: u32,
    x_dominant: bool,

    // ── Configuration ────────────────────────────────────────────────
    /// When `false` (default), the start tile (step 0) and end tile
    /// (step == steps) are skipped.
    emit_endpoints: bool,
    /// `true` after the iterator is exhausted.
    done: bool,
}

impl LosRay {
    /// Create a ray from `(x1, y1, z1)` to `(x2, y2, z2)`.
    ///
    /// By default, the start and end tiles are **not** yielded.
    /// Call [`.with_endpoints(true)`](Self::with_endpoints) to include them.
    pub fn new(
        x1: u16, y1: u16, z1: i16,
        x2: u16, y2: u16, z2: i16,
    ) -> Self {
        let dx = (x2 as i32) - (x1 as i32);
        let dy = (y2 as i32) - (y1 as i32);
        let dz = (z2 as i32) - (z1 as i32);

        let abs_dx = dx.unsigned_abs();
        let abs_dy = dy.unsigned_abs();
        let steps = abs_dx.max(abs_dy);

        let x_dominant = abs_dx >= abs_dy;
        let (abs_major, abs_minor) = if x_dominant {
            (abs_dx, abs_dy)
        } else {
            (abs_dy, abs_dx)
        };

        Self {
            cur_x: x1 as i32,
            cur_y: y1 as i32,
            end_x: x2 as i32,
            end_y: y2 as i32,
            z1: z1 as i32,
            dz,
            error: abs_major as i32 / 2,
            steps,
            // step counts how many Bresenham advances have been done.
            // 0 = at start tile, `steps` = at end tile.
            step: 0,
            sx: dx.signum(),
            sy: dy.signum(),
            abs_major,
            abs_minor,
            x_dominant,
            emit_endpoints: false,
            done: false,
        }
    }

    /// Configure whether start and end tiles are yielded.
    ///
    /// - `true` — yield **all** tiles including start `(x1, y1, z1)` and
    ///   end `(x2, y2, z2)`.
    /// - `false` (default) — skip start and end tiles, yield only
    ///   intermediate tiles.
    #[inline]
    pub fn with_endpoints(mut self, emit: bool) -> Self {
        self.emit_endpoints = emit;
        self
    }

    /// Interpolated Z at the given step index.
    #[inline]
    fn z_at_step(&self, step: u32) -> i16 {
        if self.steps == 0 {
            return self.z1 as i16;
        }
        (self.z1 + self.dz * step as i32 / self.steps as i32) as i16
    }

    /// Advance the Bresenham state by one step along the dominant axis.
    fn advance(&mut self) {
        if self.x_dominant {
            self.cur_x += self.sx;
            self.error -= self.abs_minor as i32;
            if self.error < 0 {
                self.cur_y += self.sy;
                self.error += self.abs_major as i32;
            }
        } else {
            self.cur_y += self.sy;
            self.error -= self.abs_minor as i32;
            if self.error < 0 {
                self.cur_x += self.sx;
                self.error += self.abs_major as i32;
            }
        }
    }
}

impl Iterator for LosRay {
    type Item = (u16, u16, i16);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        // ── Step 0: start tile ───────────────────────────────────────
        //
        // `step` == 0 means we haven't advanced yet — `cur` is the
        // start tile.
        if self.step == 0 {
            // Same tile (steps == 0): emit once if endpoints, then done.
            if self.steps == 0 {
                self.done = true;
                if self.emit_endpoints {
                    return Some((self.cur_x as u16, self.cur_y as u16, self.z_at_step(0)));
                }
                return None;
            }

            if self.emit_endpoints {
                // Yield the start tile, then advance for next call.
                let z = self.z_at_step(0);
                let result = (self.cur_x as u16, self.cur_y as u16, z);
                self.advance();
                self.step = 1;
                return Some(result);
            }

            // Skip the start tile: advance to step 1.
            self.advance();
            self.step = 1;
            // Fall through to the intermediate/end logic below.
        }

        // ── Steps 1..steps: intermediate and end tiles ───────────────
        //
        // At this point `cur` is already at the tile for `self.step`.
        // `self.step` ranges from 1 to `steps` inclusive.
        loop {
            if self.step > self.steps {
                self.done = true;
                return None;
            }

            let is_end = self.step == self.steps;
            let step_idx = self.step;
            let x = self.cur_x;
            let y = self.cur_y;
            let z = self.z_at_step(step_idx);

            // Prepare for the next call: advance Bresenham.
            self.step += 1;
            if !is_end {
                self.advance();
            }

            // End tile: emit only if endpoints are enabled.
            if is_end {
                if self.emit_endpoints {
                    self.done = true;
                    return Some((self.end_x as u16, self.end_y as u16, z));
                }
                self.done = true;
                return None;
            }

            // Intermediate tile: skip if it coincides with the end tile
            // (possible on perfect diagonals where the last intermediate
            // step lands on (x2, y2) before the actual end step).
            if !self.emit_endpoints && x == self.end_x && y == self.end_y {
                continue;
            }

            return Some((x as u16, y as u16, z));
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.done {
            return (0, Some(0));
        }
        let remaining = self.steps.saturating_sub(self.step) as usize;
        (0, Some(remaining + if self.emit_endpoints { 2 } else { 0 }))
    }
}

/// 3D line-of-sight validator parameterised by a [`TileProvider`].
///
/// Traces a ray between two points checking tile stacks for
/// LOS-blocking geometry.  Contains no mutable state and can be
/// created cheaply on every call.
///
/// Blocking behaviour is fully configurable via three masks:
///
/// | Mask | Default | Purpose |
/// |------|---------|---------|
/// | `blocking_mask` | `NO_SHOOT \| IMPASSABLE \| WALL` | Any of these flags → blocks |
/// | `exempt_mask` | `WINDOW` | Any of these flags → never blocks |
/// | `transparent_mask` | `0` | Any of these flags → transparent (caller override) |
///
/// Evaluation order: transparent → exempt → blocking.
pub struct LosValidator<'a, T: TileProvider> {
    provider: &'a T,
    /// Flags that cause a tile to block LOS (any bit match → blocks).
    blocking_mask: u64,
    /// Flags that exempt a tile from blocking (any bit match → never blocks).
    exempt_mask: u64,
    /// Extra transparency override (e.g. `FOLIAGE` to see through trees).
    transparent_mask: u64,
}

impl<'a, T: TileProvider> LosValidator<'a, T> {
    /// Create a validator with default blocking rules.
    #[inline]
    pub fn new(provider: &'a T) -> Self {
        use crate::vessel::tile_shape::TileShape;
        Self {
            provider,
            blocking_mask: TileShape::LOS_BLOCKING_DEFAULT,
            exempt_mask: TileShape::LOS_EXEMPT_DEFAULT,
            transparent_mask: 0,
        }
    }

    /// Override which flags cause blocking.
    ///
    /// Default: `NO_SHOOT | IMPASSABLE | WALL`.
    #[inline]
    pub fn with_blocking_mask(mut self, mask: u64) -> Self {
        self.blocking_mask = mask;
        self
    }

    /// Override which flags exempt a tile from blocking.
    ///
    /// Default: `WINDOW`.
    #[inline]
    pub fn with_exempt_mask(mut self, mask: u64) -> Self {
        self.exempt_mask = mask;
        self
    }

    /// Mark tiles whose flags match `mask` as transparent
    /// (non-blocking for LOS).
    ///
    /// For example, pass [`TileFlags::FOLIAGE`](files::tiledata::TileFlags::FOLIAGE)
    /// to allow line of sight through trees.
    #[inline]
    pub fn with_transparent_mask(mut self, mask: u64) -> Self {
        self.transparent_mask = mask;
        self
    }

    // ── Public API ───────────────────────────────────────────────────

    /// Check whether there is a clear line of sight between two 3D points.
    ///
    /// `z1` and `z2` are full Z coordinates (standing Z + eye offset).
    /// The caller is responsible for adding any eye-height offset before
    /// calling this method (typically `+14` for humanoid mobiles, `+0`
    /// for items on the ground).
    ///
    /// The start tile `(x1, y1)` and end tile `(x2, y2)` are **not**
    /// checked — only intermediate tiles along the ray.  This matches
    /// standard UO LOS semantics.
    ///
    /// Returns `true` if nothing blocks the ray (or if start == end).
    pub fn has_los(
        &self,
        x1: u16, y1: u16, z1: i16,
        x2: u16, y2: u16, z2: i16,
    ) -> bool {
        for (x, y, z) in LosRay::new(x1, y1, z1, x2, y2, z2) {
            if self.tile_blocks_ray(x, y, z) {
                return false;
            }
        }
        true
    }

    /// Trace the full ray between two 3D points, returning all
    /// intermediate tiles and the indices of every blocking tile.
    ///
    /// This is the "diagnostic" counterpart of [`has_los`](Self::has_los):
    /// instead of short-circuiting on the first blocker it collects the
    /// entire ray so callers can visualise which tiles are clear, which
    /// ones block, and which tiles lie beyond the obstruction.
    ///
    /// Z conventions are the same as `has_los` — the caller must add any
    /// eye-height offset.
    pub fn trace(
        &self,
        x1: u16, y1: u16, z1: i16,
        x2: u16, y2: u16, z2: i16,
    ) -> LosTrace {
        let ray = LosRay::new(x1, y1, z1, x2, y2, z2);
        let mut tiles: Vec<(u16, u16, i16)> = Vec::with_capacity(ray.size_hint().1.unwrap_or(0));
        let mut blockers: Vec<usize> = Vec::new();

        for (x, y, z) in ray {
            let idx = tiles.len();
            tiles.push((x, y, z));
            if self.tile_blocks_ray(x, y, z) {
                blockers.push(idx);
            }
        }

        LosTrace {
            has_los: blockers.is_empty(),
            tiles,
            blockers,
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Check whether any shape in the tile stack at `(x, y)` blocks the
    /// ray at the given interpolated Z.
    ///
    /// A shape blocks the ray when:
    /// 1. `blocks_los_masked(blocking, exempt, transparent)` returns true.
    /// 2. The ray's Z intersects the shape's vertical extent `[z_base, z_top]`.
    #[inline]
    fn tile_blocks_ray(&self, x: u16, y: u16, ray_z: i16) -> bool {
        // Direction is irrelevant for LOS — we use North as a neutral default
        // (same convention as query_block).
        let shapes = self.provider.query_tile_stack(x, y, Heading::North);

        for shape in &shapes {
            if !shape.blocks_los_masked(
                self.blocking_mask,
                self.exempt_mask,
                self.transparent_mask,
            ) {
                continue;
            }

            let z_base = shape.z_base() as i16;
            let z_top = shape.z_top() as i16;

            // The ray passes through this shape's vertical extent.
            if ray_z >= z_base && ray_z <= z_top {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vessel::tile_shape::TileShape;
    use files::tiledata::TileFlags;

    /// Trivial in-memory tile provider for unit tests.
    struct TestProvider {
        /// (x, y) → tile stack.
        tiles: std::collections::HashMap<(u16, u16), Vec<TileShape>>,
    }

    impl TestProvider {
        fn new() -> Self {
            Self { tiles: std::collections::HashMap::new() }
        }

        fn set(&mut self, x: u16, y: u16, shapes: Vec<TileShape>) {
            self.tiles.insert((x, y), shapes);
        }
    }

    impl TileProvider for TestProvider {
        fn query_tile_stack(&self, x: u16, y: u16, _direction: Heading) -> Vec<TileShape> {
            self.tiles.get(&(x, y)).cloned().unwrap_or_default()
        }
    }

    // ── LosRay tests ─────────────────────────────────────────────────

    #[test]
    fn ray_same_tile_yields_nothing() {
        let tiles: Vec<_> = LosRay::new(5, 5, 10, 5, 5, 10).collect();
        assert!(tiles.is_empty());
    }

    #[test]
    fn ray_same_tile_with_endpoints() {
        let tiles: Vec<_> = LosRay::new(5, 5, 10, 5, 5, 10)
            .with_endpoints(true)
            .collect();
        // Same tile: steps == 0, only the start/end point (they're the same).
        assert_eq!(tiles, vec![(5, 5, 10)]);
    }

    #[test]
    fn ray_adjacent_tiles_no_intermediate() {
        // Adjacent tiles: only 1 step, no intermediate tiles.
        let tiles: Vec<_> = LosRay::new(3, 3, 0, 4, 3, 0).collect();
        assert!(tiles.is_empty());
    }

    #[test]
    fn ray_adjacent_tiles_with_endpoints() {
        let tiles: Vec<_> = LosRay::new(3, 3, 0, 4, 3, 0)
            .with_endpoints(true)
            .collect();
        assert_eq!(tiles, vec![(3, 3, 0), (4, 3, 0)]);
    }

    #[test]
    fn ray_horizontal_skips_endpoints() {
        // Horizontal ray from (0,0) to (5,0) — 5 steps.
        // Without endpoints: tiles (1,0), (2,0), (3,0), (4,0).
        let tiles: Vec<_> = LosRay::new(0, 0, 10, 5, 0, 10).collect();
        let xs: Vec<u16> = tiles.iter().map(|t| t.0).collect();
        assert_eq!(xs, vec![1, 2, 3, 4]);
        // All at y=0, z=10.
        assert!(tiles.iter().all(|t| t.1 == 0 && t.2 == 10));
    }

    #[test]
    fn ray_horizontal_with_endpoints() {
        let tiles: Vec<_> = LosRay::new(0, 0, 10, 5, 0, 10)
            .with_endpoints(true)
            .collect();
        let xs: Vec<u16> = tiles.iter().map(|t| t.0).collect();
        assert_eq!(xs, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn ray_vertical_path() {
        let tiles: Vec<_> = LosRay::new(3, 0, 0, 3, 4, 0).collect();
        let ys: Vec<u16> = tiles.iter().map(|t| t.1).collect();
        assert_eq!(ys, vec![1, 2, 3]);
        assert!(tiles.iter().all(|t| t.0 == 3));
    }

    #[test]
    fn ray_diagonal_path() {
        let tiles: Vec<_> = LosRay::new(0, 0, 0, 4, 4, 0).collect();
        // Perfect diagonal: steps == 4, intermediate = (1,1), (2,2), (3,3).
        assert_eq!(tiles, vec![(1, 1, 0), (2, 2, 0), (3, 3, 0)]);
    }

    #[test]
    fn ray_diagonal_with_endpoints() {
        let tiles: Vec<_> = LosRay::new(0, 0, 0, 4, 4, 0)
            .with_endpoints(true)
            .collect();
        assert_eq!(
            tiles,
            vec![(0, 0, 0), (1, 1, 0), (2, 2, 0), (3, 3, 0), (4, 4, 0)]
        );
    }

    #[test]
    fn ray_z_interpolation() {
        // Ray from z=0 to z=100 across 10 tiles.
        let tiles: Vec<_> = LosRay::new(0, 0, 0, 10, 0, 100).collect();
        // Intermediate tiles: x=1..9, z = 0 + 100*step/10.
        assert_eq!(tiles.len(), 9);
        for (i, &(x, y, z)) in tiles.iter().enumerate() {
            let step = i as i32 + 1;
            assert_eq!(x, step as u16);
            assert_eq!(y, 0);
            assert_eq!(z, (100 * step / 10) as i16);
        }
    }

    #[test]
    fn ray_negative_direction() {
        // Ray going backwards: (5,5) → (0,5).
        let tiles: Vec<_> = LosRay::new(5, 5, 0, 0, 5, 0).collect();
        let xs: Vec<u16> = tiles.iter().map(|t| t.0).collect();
        assert_eq!(xs, vec![4, 3, 2, 1]);
    }

    #[test]
    fn ray_count_matches_distance() {
        // Horizontal 10 tiles: 9 intermediate tiles.
        assert_eq!(LosRay::new(0, 0, 0, 10, 0, 0).count(), 9);
        // With endpoints: 11 tiles.
        assert_eq!(
            LosRay::new(0, 0, 0, 10, 0, 0).with_endpoints(true).count(),
            11,
        );
        // Same tile: 0 intermediate, 1 with endpoints.
        assert_eq!(LosRay::new(5, 5, 0, 5, 5, 0).count(), 0);
        assert_eq!(
            LosRay::new(5, 5, 0, 5, 5, 0).with_endpoints(true).count(),
            1,
        );
    }

    #[test]
    fn ray_non_axis_aligned() {
        // Shallow angle: dx=6, dy=2 → X-dominant, 6 steps.
        let tiles: Vec<_> = LosRay::new(0, 0, 0, 6, 2, 0)
            .with_endpoints(true)
            .collect();
        // Should hit 7 tiles (steps 0..6).
        assert_eq!(tiles.len(), 7);
        // First and last tiles.
        assert_eq!(tiles[0], (0, 0, 0));
        assert_eq!(tiles[6], (6, 2, 0));
        // All x values monotonically increase.
        for i in 1..tiles.len() {
            assert!(tiles[i].0 > tiles[i - 1].0);
        }
    }

    // ── LosValidator tests (existing, via LosRay now) ────────────────

    #[test]
    fn same_tile_always_visible() {
        let provider = TestProvider::new();
        let los = LosValidator::new(&provider);
        assert!(los.has_los(10, 10, 0, 10, 10, 0));
    }

    #[test]
    fn clear_path_no_obstacles() {
        let provider = TestProvider::new();
        let los = LosValidator::new(&provider);
        // Horizontal line, no tiles in the way.
        assert!(los.has_los(0, 0, 0, 10, 0, 0));
        // Diagonal.
        assert!(los.has_los(0, 0, 0, 5, 5, 0));
    }

    #[test]
    fn wall_blocks_los() {
        let mut provider = TestProvider::new();

        // Place a NO_SHOOT wall at (5, 0) spanning z 0..20.
        provider.set(5, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 20,
                flags: TileFlags::NO_SHOOT | TileFlags::IMPASSABLE,
            },
        ]);

        let los = LosValidator::new(&provider);
        // Ray at z=10 through (5,0) — blocked.
        assert!(!los.has_los(0, 0, 10, 10, 0, 10));
    }

    #[test]
    fn ray_over_wall_not_blocked() {
        let mut provider = TestProvider::new();

        // Wall at (5, 0) from z=0 to z=10.
        provider.set(5, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 10,
                flags: TileFlags::NO_SHOOT | TileFlags::IMPASSABLE,
            },
        ]);

        let los = LosValidator::new(&provider);
        // Ray at z=20 — above the wall, not blocked.
        assert!(los.has_los(0, 0, 20, 10, 0, 20));
    }

    #[test]
    fn wall_with_window_does_not_block() {
        let mut provider = TestProvider::new();

        // WALL + IMPASSABLE + WINDOW — should not block LOS.
        provider.set(5, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 20,
                flags: TileFlags::WALL | TileFlags::IMPASSABLE | TileFlags::WINDOW,
            },
        ]);

        let los = LosValidator::new(&provider);
        assert!(los.has_los(0, 0, 10, 10, 0, 10));
    }

    #[test]
    fn foliage_blocks_unless_transparent() {
        let mut provider = TestProvider::new();

        // Tree with NO_SHOOT + FOLIAGE.
        provider.set(3, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 15,
                flags: TileFlags::NO_SHOOT | TileFlags::FOLIAGE,
            },
        ]);

        // Without transparency — blocked.
        let los = LosValidator::new(&provider);
        assert!(!los.has_los(0, 0, 10, 6, 0, 10));

        // With FOLIAGE transparency — not blocked.
        let los = LosValidator::new(&provider)
            .with_transparent_mask(TileFlags::FOLIAGE);
        assert!(los.has_los(0, 0, 10, 6, 0, 10));
    }

    #[test]
    fn start_and_end_tiles_not_checked() {
        let mut provider = TestProvider::new();

        // Place blockers at both start and end tiles.
        let wall = vec![TileShape::Surface {
            z_base: 0,
            z_stand: 20,
            flags: TileFlags::NO_SHOOT | TileFlags::IMPASSABLE,
        }];
        provider.set(0, 0, wall.clone());
        provider.set(3, 0, wall);

        let los = LosValidator::new(&provider);
        // Only intermediate tiles (1,0) and (2,0) are checked — they're
        // empty, so LOS passes.
        assert!(los.has_los(0, 0, 10, 3, 0, 10));
    }

    #[test]
    fn background_does_not_block() {
        let mut provider = TestProvider::new();

        // Background shapes never block LOS regardless of Z.
        provider.set(5, 0, vec![
            TileShape::Background { z_base: 0, z_top: 100 },
        ]);

        let los = LosValidator::new(&provider);
        assert!(los.has_los(0, 0, 10, 10, 0, 10));
    }

    #[test]
    fn diagonal_ray_checks_intermediate() {
        let mut provider = TestProvider::new();

        // Block (3, 3) — on the diagonal from (0,0) to (6,6).
        provider.set(3, 3, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 20,
                flags: TileFlags::NO_SHOOT,
            },
        ]);

        let los = LosValidator::new(&provider);
        assert!(!los.has_los(0, 0, 10, 6, 6, 10));
    }

    #[test]
    fn z_interpolation_misses_low_wall() {
        let mut provider = TestProvider::new();

        // Low wall at (5, 0) from z=0 to z=5.
        provider.set(5, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 5,
                flags: TileFlags::NO_SHOOT,
            },
        ]);

        let los = LosValidator::new(&provider);
        // Ray from z=0 at start to z=20 at end — at tile (5,0) the
        // interpolated Z is approximately 10, which is above the wall.
        assert!(los.has_los(0, 0, 0, 10, 0, 20));
    }

    #[test]
    fn slope_blocks_within_z_range() {
        let mut provider = TestProvider::new();

        // Slope from z=5 to z=15 with NO_SHOOT.
        provider.set(5, 0, vec![
            TileShape::Slope {
                z_base: 5,
                z_stand: 10,
                z_top: 15,
                flags: TileFlags::NO_SHOOT,
            },
        ]);

        let los = LosValidator::new(&provider);
        // Ray at z=12 — inside [5, 15] → blocked.
        assert!(!los.has_los(0, 0, 12, 10, 0, 12));
        // Ray at z=20 — above → not blocked.
        assert!(los.has_los(0, 0, 20, 10, 0, 20));
        // Ray at z=2 — below → not blocked.
        assert!(los.has_los(0, 0, 2, 10, 0, 2));
    }

    // ── LosValidator::trace tests ────────────────────────────────────

    #[test]
    fn trace_clear_path() {
        let provider = TestProvider::new();
        let los = LosValidator::new(&provider);
        let trace = los.trace(0, 0, 10, 5, 0, 10);

        assert!(trace.has_los);
        assert!(trace.blockers.is_empty());
        // 5-step horizontal ray: 4 intermediate tiles.
        assert_eq!(trace.tiles.len(), 4);
        let xs: Vec<u16> = trace.tiles.iter().map(|t| t.0).collect();
        assert_eq!(xs, vec![1, 2, 3, 4]);
    }

    #[test]
    fn trace_blocked_records_index() {
        let mut provider = TestProvider::new();

        // Wall at (3, 0)
        provider.set(3, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 20,
                flags: TileFlags::NO_SHOOT | TileFlags::IMPASSABLE,
            },
        ]);

        let los = LosValidator::new(&provider);
        let trace = los.trace(0, 0, 10, 6, 0, 10);

        assert!(!trace.has_los);
        // Intermediate tiles: 1,2,3,4,5 → first blocker at index 2 (tile x=3)
        assert_eq!(trace.first_blocker(), Some(2));
        assert_eq!(trace.tiles[2], (3, 0, 10));
        // All 5 intermediate tiles are collected (ray continues past blocker).
        assert_eq!(trace.tiles.len(), 5);
    }

    #[test]
    fn trace_same_tile() {
        let provider = TestProvider::new();
        let los = LosValidator::new(&provider);
        let trace = los.trace(5, 5, 0, 5, 5, 0);

        assert!(trace.has_los);
        assert!(trace.tiles.is_empty());
        assert!(trace.blockers.is_empty());
    }

    #[test]
    fn trace_consistent_with_has_los() {
        let mut provider = TestProvider::new();

        // Place a wall at (5, 0)
        provider.set(5, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 20,
                flags: TileFlags::NO_SHOOT,
            },
        ]);

        let los = LosValidator::new(&provider);

        // Blocked ray
        assert_eq!(
            los.trace(0, 0, 10, 10, 0, 10).has_los,
            los.has_los(0, 0, 10, 10, 0, 10),
        );

        // Clear ray (above the wall)
        assert_eq!(
            los.trace(0, 0, 30, 10, 0, 30).has_los,
            los.has_los(0, 0, 30, 10, 0, 30),
        );
    }

    #[test]
    fn trace_tiles_beyond_blocker() {
        let mut provider = TestProvider::new();

        // Wall at (2, 0)
        provider.set(2, 0, vec![
            TileShape::Surface {
                z_base: 0,
                z_stand: 20,
                flags: TileFlags::NO_SHOOT,
            },
        ]);

        let los = LosValidator::new(&provider);
        let trace = los.trace(0, 0, 10, 8, 0, 10);

        // first blocker: tile (2, 0) is at index 1
        assert_eq!(trace.first_blocker(), Some(1));
        // Tiles beyond the blocker are still present.
        assert_eq!(trace.tiles.len(), 7); // tiles x=1..7
        // Tiles after index 1 are "beyond" tiles.
        assert!(trace.tiles.len() > trace.first_blocker().unwrap() + 1);
    }

    #[test]
    fn trace_multiple_blockers() {
        let mut provider = TestProvider::new();

        // Two walls: at (2, 0) and (5, 0)
        let wall = vec![TileShape::Surface {
            z_base: 0,
            z_stand: 20,
            flags: TileFlags::NO_SHOOT,
        }];
        provider.set(2, 0, wall.clone());
        provider.set(5, 0, wall);

        let los = LosValidator::new(&provider);
        let trace = los.trace(0, 0, 10, 8, 0, 10);

        assert!(!trace.has_los);
        // Both walls recorded as blockers.
        assert_eq!(trace.blockers.len(), 2);
        // First blocker at index 1 (tile x=2), second at index 4 (tile x=5).
        assert_eq!(trace.tiles[trace.blockers[0]], (2, 0, 10));
        assert_eq!(trace.tiles[trace.blockers[1]], (5, 0, 10));
    }
}
