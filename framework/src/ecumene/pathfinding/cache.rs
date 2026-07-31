//! Block-level tile cache for pathfinding.
//!
//! [`CachingProvider`] wraps any [`TileProvider`] and caches [`TileBlock`]s
//! by [`BlockKey`].  Within a single A* search the same 8×8 block is often
//! visited many times; loading it once via `query_block` and serving
//! subsequent per-tile queries from the in-memory cache avoids repeated file
//! lookups and per-tile heap allocations.
//!
//! # Why `&mut self` instead of `&self`
//!
//! [`TileProvider::query_tile_stack`] takes `&self`, which conflicts with
//! cache mutation.  Rather than reaching for `RefCell` or `UnsafeCell`,
//! `CachingProvider` exposes its own `tile_stack` method taking `&mut self`.
//! The surveyor calls this directly (it holds `&mut CachingProvider`) so no
//! dynamic dispatch or interior mutability is needed.
//!
//! # Movement validation
//!
//! Step validation (`test_step`, `test_step_single`) delegates to the free
//! functions in [`super::super::movement`] — the same functions used by
//! [`MovementValidator`](super::super::movement::MovementValidator) — so
//! there is no duplicated movement logic.
//!
//! # Statistics
//!
//! [`CachingProvider::stats`] returns a [`CacheStats`] snapshot useful for
//! tuning and benchmarking.

use std::collections::HashMap;

use u_core::{BlockKey, Heading};

use crate::ecumene::movement::{compute_source_range, compute_dest_position};
use crate::ecumene::tile_block::TileBlock;
use crate::ecumene::tile_provider::TileProvider;
use crate::vessel::tile_shape::TileShape;

// ── CacheStats ────────────────────────────────────────────────────────────

/// Diagnostic counters collected during a pathfinding search.
#[derive(Debug, Default, Clone, Copy)]
pub struct CacheStats {
    /// `query_tile_stack` calls served from the block cache (no I/O).
    pub hits: u64,
    /// `query_block` calls issued to the underlying provider.
    pub misses: u64,
    /// Distinct blocks currently held in the cache.
    pub blocks_loaded: u64,
}

impl CacheStats {
    /// Cache hit rate in [0.0, 1.0].
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 { 0.0 } else { self.hits as f64 / total as f64 }
    }
}

impl std::fmt::Display for CacheStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "blocks={} hits={} misses={} hit_rate={:.1}%",
            self.blocks_loaded,
            self.hits,
            self.misses,
            self.hit_rate() * 100.0,
        )
    }
}

// ── CachingProvider ───────────────────────────────────────────────────────

/// Per-search block cache wrapping a [`TileProvider`].
///
/// Create once at the start of `trace_a_star` and drop afterwards.
/// The cache is **not** thread-safe — it is designed for single-threaded
/// use within one A* call.
pub struct CachingProvider<'a, T: TileProvider> {
    inner: &'a T,
    /// Blocks loaded so far, keyed by [`BlockKey`].
    cache: HashMap<BlockKey, TileBlock>,
    stats: CacheStats,
}

impl<'a, T: TileProvider> CachingProvider<'a, T> {
    /// Create a new cache backed by `inner`.
    #[allow(dead_code)]
    pub fn new(inner: &'a T) -> Self {
        Self {
            inner,
            cache: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Create with a pre-allocated capacity hint.
    ///
    /// A reasonable estimate: `(search_width / 8 + 1) * (search_height / 8 + 1)`.
    pub fn with_capacity(inner: &'a T, expected_blocks: usize) -> Self {
        Self {
            inner,
            cache: HashMap::with_capacity(expected_blocks),
            stats: CacheStats::default(),
        }
    }

    /// Snapshot of cache statistics.
    #[inline]
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Number of distinct blocks currently in the cache.
    #[inline]
    pub fn cached_blocks(&self) -> usize {
        self.cache.len()
    }

    /// Pre-warm the cache for all blocks overlapping a tile rectangle.
    ///
    /// Call this before `trace_a_star` when the search area is known from
    /// `TraceOptions::left/top/right/bottom`.  All blocks are loaded in one
    /// pass; the A* inner loop then runs entirely from the in-memory cache.
    pub fn prefetch_rect(
        &mut self,
        tile_left:   isize,
        tile_top:    isize,
        tile_right:  isize,
        tile_bottom: isize,
    ) {
        let bx_min = (tile_left  / 8).max(0) as u16;
        let by_min = (tile_top   / 8).max(0) as u16;
        let bx_max = ((tile_right  + 7) / 8).max(0) as u16;
        let by_max = ((tile_bottom + 7) / 8).max(0) as u16;

        for bx in bx_min..=bx_max {
            for by in by_min..=by_max {
                let key = BlockKey::new(bx, by);
                if !self.cache.contains_key(&key) {
                    let block = self.inner.query_block(key);
                    self.cache.insert(key, block);
                    self.stats.misses += 1;
                    self.stats.blocks_loaded += 1;
                }
            }
        }
    }

    // ── Core tile access ──────────────────────────────────────────────

    /// Get or load the block for `key`.
    #[inline]
    fn block_for(&mut self, key: BlockKey) -> &TileBlock {
        if !self.cache.contains_key(&key) {
            let block = self.inner.query_block(key);
            self.cache.insert(key, block);
            self.stats.misses += 1;
            self.stats.blocks_loaded += 1;
        } else {
            self.stats.hits += 1;
        }
        &self.cache[&key]
    }

    /// Return the tile stack for `(x, y)` as an owned `Vec<TileShape>`.
    ///
    /// `TileShape` is `Copy`, so this is a `memcpy` of typically 1–4 elements
    /// from the already-loaded block — much cheaper than going back to disk.
    #[inline]
    pub fn tile_stack(&mut self, x: u16, y: u16) -> Vec<TileShape> {
        let key = BlockKey::from_tile(x, y);
        let block = self.block_for(key);
        let ox = (x % 8) as u8;
        let oy = (y % 8) as u8;
        block.tile_stack(ox, oy).to_vec()
    }

    /// Append the tile stack for `(x, y)` into an existing `Vec`.
    ///
    /// Avoids allocating a new `Vec` when the caller already owns a buffer.
    #[allow(dead_code)]
    #[inline]
    pub fn append_tile_stack(&mut self, x: u16, y: u16, out: &mut Vec<TileShape>) {
        let key = BlockKey::from_tile(x, y);
        let block = self.block_for(key);
        let ox = (x % 8) as u8;
        let oy = (y % 8) as u8;
        out.extend_from_slice(block.tile_stack(ox, oy));
    }

    /// Borrow the tile stack for `(x, y)` as a `&[TileShape]`.
    ///
    /// The returned slice is valid until the next mutable call on this
    /// `CachingProvider`.  Prefer this over [`tile_stack`](Self::tile_stack)
    /// when the caller does not need to outlive the provider.
    #[allow(dead_code)]
    #[inline]
    pub fn tile_stack_ref(&mut self, x: u16, y: u16) -> &[TileShape] {
        let key = BlockKey::from_tile(x, y);
        // Two-step to satisfy borrow checker: insert first if missing.
        if !self.cache.contains_key(&key) {
            let block = self.inner.query_block(key);
            self.cache.insert(key, block);
            self.stats.misses += 1;
            self.stats.blocks_loaded += 1;
        } else {
            self.stats.hits += 1;
        }
        let ox = (x % 8) as u8;
        let oy = (y % 8) as u8;
        self.cache[&key].tile_stack(ox, oy)
    }

    // ── Movement primitives ───────────────────────────────────────────
    //
    // Delegates to the free functions in `ecumene::movement` so there is
    // no duplicated movement logic.

    /// Test a single step from `(x, y, z)` in `dir` (no diagonal check).
    ///
    /// Returns the new standing Z at the destination, or `None` if blocked.
    pub fn test_step_single(
        &mut self,
        x: u16,
        y: u16,
        z: i8,
        dir: Heading,
        passable_mask: u64,
        can_fly: bool,
    ) -> Option<i8> {
        let (dx, dy) = dir.delta();
        let to_x = (x as i32 + dx).clamp(0, u16::MAX as i32) as u16;
        let to_y = (y as i32 + dy).clamp(0, u16::MAX as i32) as u16;

        let source_shapes = self.tile_stack(x, y);
        let (z_low, z_high) = compute_source_range(&source_shapes, z);

        let mut dest_shapes = self.tile_stack(to_x, to_y);
        dest_shapes.push(TileShape::cap());
        compute_dest_position(&dest_shapes, z, z_low, z_high, passable_mask, can_fly)
    }

    /// Full step check including diagonal adjacency validation.
    ///
    /// For diagonal directions the two adjacent cardinal tiles are also
    /// tested (UO requires both to be passable for a diagonal move).
    pub fn test_step(
        &mut self,
        x: u16,
        y: u16,
        z: i8,
        dir: Heading,
        passable_mask: u64,
        can_fly: bool,
    ) -> Option<i8> {
        let dest_z = self.test_step_single(x, y, z, dir, passable_mask, can_fly)?;

        // Diagonal: also check the two adjacent cardinal directions.
        if dir.is_diagonal() {
            let right = dir.turn(1);
            let left  = dir.turn(-1);
            self.test_step_single(x, y, z, right, passable_mask, can_fly)?;
            self.test_step_single(x, y, z, left,  passable_mask, can_fly)?;
        }

        Some(dest_z)
    }
}
