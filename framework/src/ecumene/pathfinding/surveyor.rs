//! A* pathfinder with block-level tile caching.
//!
//! Uses [`CachingProvider`] so each 8×8 map block is loaded at most once
//! per search, regardless of how many times the A* frontier visits tiles
//! within that block.
//!
//! # Accuracy note
//!
//! `query_block` returns direction-agnostic Z values for sloped land tiles
//! (averaged vertex Z).  For most UO terrain this makes no difference; on
//! strongly sloped land the Z estimate may be ±1–2 units off relative to
//! the exact direction-dependent value.  This is within the normal
//! `CLIMB_HEIGHT` tolerance and does not affect pathfinding correctness in
//! practice.

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{BinaryHeap, HashMap};
use std::time::Instant;

use log::{debug, info, warn};

use u_core::Heading;

use crate::ecumene::tile_provider::TileProvider;

use super::cache::CachingProvider;
use super::{AStarAction, AStarEvent, DistanceFunc, Point, TraceOptions};

// ── Internal position ─────────────────────────────────────────────────────

#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone)]
struct Pos(isize, isize, i8);

// ── Priority queue entry: (f, g, dir, dst, src) ───────────────────────────

struct ScoredPos(isize, isize, Heading, Pos, Pos);

impl PartialEq  for ScoredPos { fn eq(&self, o: &Self) -> bool { self.0 == o.0 } }
impl Eq         for ScoredPos {}
impl Ord        for ScoredPos {
    fn cmp(&self, o: &Self) -> Ordering { o.0.cmp(&self.0) } // min-heap by f
}
impl PartialOrd for ScoredPos {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> { Some(self.cmp(o)) }
}

// ── Surveyor ──────────────────────────────────────────────────────────────

/// Pathfinder backed by any [`TileProvider`], using a per-search block cache.
pub struct Surveyor<'a, T: TileProvider> {
    provider:      &'a T,
    passable_mask: u64,
    fly:           bool,
}

impl<'a, T: TileProvider> Surveyor<'a, T> {
    #[allow(dead_code)]
    pub fn new(provider: &'a T) -> Self {
        Self { provider, passable_mask: 0, fly: false }
    }

    pub fn with_options(provider: &'a T, opts: &TraceOptions) -> Self {
        Self {
            provider,
            passable_mask: opts.passable_mask.unwrap_or(0),
            fly:           opts.fly.unwrap_or(false),
        }
    }

    // ── A* ────────────────────────────────────────────────────────────

    /// Search for a path from `(s_x, s_y, s_z)` to `(d_x, d_y, d_z)`.
    ///
    /// `map_width` / `map_height` are used as default search-area bounds
    /// when `options.right` / `options.bottom` are not set.
    ///
    /// Results are appended to `points`.  When `options.all_points` is true,
    /// all explored tiles with their g-scores are returned; otherwise the
    /// found path is returned in start→end order.
    ///
    /// The `observer` callback is invoked for every significant A* event
    /// (node visited, node added to frontier, path node reconstructed).
    /// Return [`AStarAction::Cancel`] from the callback to abort the search.
    /// Use `|_| AStarAction::Continue` for a no-op observer.
    pub fn trace_a_star<F>(
        &self,
        s_x: isize, s_y: isize, s_z: i8, sdir: Heading,
        d_x: isize, d_y: isize, d_z: i8, _ddir: Heading,
        points:     &mut Vec<Point>,
        options:    &TraceOptions,
        map_width:  isize,
        map_height: isize,
        mut observer: F,
    )
    where
        F: FnMut(AStarEvent) -> AStarAction,
    {
        // ── Option defaults ───────────────────────────────────────────
        let x_accuracy = options.accuracy_x.unwrap_or(0);
        let y_accuracy = options.accuracy_y.unwrap_or(0);
        let z_accuracy = options.accuracy_z.unwrap_or(0);

        let cost_straight  = options.cost_move_straight.unwrap_or(1);
        let cost_diagonal  = options.cost_move_diagonal.unwrap_or(cost_straight);
        let cost_turn      = options.cost_turn.unwrap_or(1);
        let cost_multi     = options.cost_move_multi.unwrap_or(0);
        let cost_limit     = options.cost_limit.unwrap_or(isize::MAX);

        let allow_diagonal = options.allow_diagonal_move.unwrap_or(true);
        let all_points     = options.all_points.unwrap_or(false);
        let time_limit_ms  = options.time_limit.map(|v| v as u128).unwrap_or(u128::MAX);

        let h_dist     = options.heuristic_distance.unwrap_or(DistanceFunc::Diagonal);
        let h_straight = options.heuristic_straight.unwrap_or(5);
        let h_diagonal = options.heuristic_diagonal.unwrap_or(h_straight);

        let left   = options.left.unwrap_or(0);
        let top    = options.top.unwrap_or(0);
        let right  = options.right.unwrap_or(map_width);
        let bottom = options.bottom.unwrap_or(map_height);

        // ── Capacity estimate for block cache ─────────────────────────
        // Heuristic: assume the search touches roughly 1/4 of the search
        // area in blocks.  Better over- than under-estimate.
        let w_blocks = ((right  - left + 7) / 8).max(1) as usize;
        let h_blocks = ((bottom - top  + 7) / 8).max(1) as usize;
        let est_blocks = (w_blocks * h_blocks / 4).max(4);

        let mut cache = CachingProvider::with_capacity(self.provider, est_blocks);

        // Optional pre-warm: if bounds are tightly specified pre-load all
        // blocks so the A* loop never blocks on I/O.  Skip for very large
        // areas (>256 blocks) to avoid loading gigabytes of data upfront.
        if w_blocks * h_blocks <= 256 {
            cache.prefetch_rect(left, top, right, bottom);
            debug!(
                "A* pre-warmed {} blocks ({w_blocks}×{h_blocks})",
                cache.cached_blocks()
            );
        }

        // ── Heuristic ─────────────────────────────────────────────────
        let h_fn = |p: &Pos| -> isize {
            let dx = (d_x - p.0).abs();
            let dy = (d_y - p.1).abs();
            match h_dist {
                DistanceFunc::Manhattan => (dx + dy) * h_straight,
                DistanceFunc::Chebyshev => dx.max(dy) * h_straight,
                DistanceFunc::Diagonal  =>
                    h_straight * (dx + dy) + (h_diagonal - 2 * h_straight) * dx.min(dy),
                DistanceFunc::Euclidean =>
                    ((dx * dx + dy * dy) as f64).sqrt() as isize * h_straight,
            }
        };

        // ── A* data structures ────────────────────────────────────────
        let mut frontier: BinaryHeap<ScoredPos> = BinaryHeap::new();
        let mut visited:  HashMap<Pos, isize>   = HashMap::new();
        let mut back_path: HashMap<Pos, Pos>    = HashMap::new();

        let passable_mask = self.passable_mask;
        let can_fly       = self.fly;

        // ── Bounds-checked step via cache ─────────────────────────────
        macro_rules! check_step {
            ($cx:expr, $cy:expr, $cz:expr, $dir:expr) => {{
                let (dx, dy) = $dir.delta();
                let nx = $cx + dx as isize;
                let ny = $cy + dy as isize;
                if nx < left || nx >= right || ny < top || ny >= bottom {
                    None
                } else {
                    let xu = $cx.clamp(0, u16::MAX as isize) as u16;
                    let yu = $cy.clamp(0, u16::MAX as isize) as u16;
                    cache.test_step(xu, yu, $cz, $dir, passable_mask, can_fly)
                }
            }};
        }

        // ── Seed ──────────────────────────────────────────────────────
        let start   = Pos(s_x, s_y, s_z);
        let start_f = h_fn(&start);
        frontier.push(ScoredPos(start_f, 0, sdir, start, Pos(-1, -1, -1)));

        let t0 = Instant::now();
        let mut iter: u64 = 0;
        let mut best_dist = isize::MAX;
        let mut best_pos:  Option<Pos> = None;

        // ── Main loop ─────────────────────────────────────────────────
        while let Some(ScoredPos(_, curr_g, curr_dir, curr, src)) = frontier.pop() {
            let Pos(cx, cy, cz) = curr;

            iter += 1;

            if iter % 1_000 == 0 && t0.elapsed().as_millis() >= time_limit_ms {
                warn!("A* time limit reached after {iter} iters ({time_limit_ms}ms)");
                break;
            }
            if iter % 100_000 == 0 {
                debug!(
                    "A* iter={iter} pos=({cx},{cy}) g={curr_g} frontier={} cache={}",
                    frontier.len(), cache.stats()
                );
            }

            // Skip already-visited
            match visited.entry(curr) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(e)   => { e.insert(curr_g); }
            }
            back_path.insert(curr, src);

            // Notify observer: node expanded (visited).
            if observer(AStarEvent::Visited { x: cx, y: cy, z: cz, g: curr_g })
                == AStarAction::Cancel
            {
                info!("A* cancelled by observer at iter {iter}");
                break;
            }

            // Best-so-far
            let dx   = (d_x - cx).abs();
            let dy   = (d_y - cy).abs();
            let dz   = (d_z - cz).abs() as isize;
            let dist = dx.max(dy).max(dz);
            if dist < best_dist {
                best_dist = dist;
                best_pos  = Some(curr);
            }

            // Goal check
            if dx <= x_accuracy && dy <= y_accuracy && dz <= z_accuracy {
                info!("A* found goal at ({cx},{cy},{cz}) g={curr_g}");
                break;
            }

            // ── Expand neighbours ─────────────────────────────────────
            use Heading::*;

            let n = check_step!(cx, cy, cz, North);
            let e = check_step!(cx, cy, cz, East);
            let s = check_step!(cx, cy, cz, South);
            let w = check_step!(cx, cy, cz, West);

            let steps: [(Heading, Option<i8>); 8] = if allow_diagonal {
                let ne = if n.is_some() && e.is_some() { check_step!(cx, cy, cz, NorthEast) } else { None };
                let se = if s.is_some() && e.is_some() { check_step!(cx, cy, cz, SouthEast) } else { None };
                let sw = if s.is_some() && w.is_some() { check_step!(cx, cy, cz, SouthWest) } else { None };
                let nw = if n.is_some() && w.is_some() { check_step!(cx, cy, cz, NorthWest) } else { None };
                [(North,n),(NorthEast,ne),(East,e),(SouthEast,se),(South,s),(SouthWest,sw),(West,w),(NorthWest,nw)]
            } else {
                [(North,n),(East,e),(South,s),(West,w),(NorthEast,None),(SouthEast,None),(SouthWest,None),(NorthWest,None)]
            };

            for (dir, dest_z_opt) in steps {
                let dest_z = match dest_z_opt { Some(z) => z, None => continue };

                let (ddx, ddy) = dir.delta();
                let dest_x = cx + ddx as isize;
                let dest_y = cy + ddy as isize;
                let dest = Pos(dest_x, dest_y, dest_z);

                if visited.contains_key(&dest) { continue; }

                // Step cost
                let step_cost  = if dir.is_diagonal() { cost_diagonal } else { cost_straight };
                let turn_cost  = if dir == curr_dir { 0 } else { cost_turn };
                let mut dest_g = curr_g + step_cost + turn_cost;

                // Multi-occupancy penalty: tile is non-empty in the cache
                if cost_multi > 0 {
                    let xu = dest_x.clamp(0, u16::MAX as isize) as u16;
                    let yu = dest_y.clamp(0, u16::MAX as isize) as u16;
                    if !cache.tile_stack(xu, yu).is_empty() {
                        dest_g += cost_multi;
                    }
                }

                if dest_g > cost_limit { continue; }

                let dest_f = dest_g + h_fn(&dest);
                frontier.push(ScoredPos(dest_f, dest_g, dir, dest, curr));

                // Notify observer: node added to frontier.
                if observer(AStarEvent::Frontier { x: dest_x, y: dest_y, z: dest_z, f: dest_f })
                    == AStarAction::Cancel
                {
                    break;
                }
            }
        }

        let duration = t0.elapsed();
        let stats = cache.stats();
        debug!(
            "A* done: {iter} iters, {} visited, {duration:?}, cache: {stats}",
            visited.len()
        );

        // ── Build result ──────────────────────────────────────────────
        if all_points {
            for (Pos(x, y, z), w) in &visited {
                points.push(Point { x: *x, y: *y, z: *z, w: *w });
            }
        } else if let Some(mut curr_pos) = best_pos {
            info!("A* reconstructing path from {curr_pos:?} (best_dist={best_dist})");
            let mut steps = 0usize;
            loop {
                let prev = match back_path.get(&curr_pos) {
                    Some(&p) => p,
                    None     => break,
                };
                let Pos(px, py, pz) = prev;
                points.push(Point { x: px, y: py, z: pz, w: 0 });
                observer(AStarEvent::Path { x: px, y: py, z: pz });
                if prev == start { break; }
                curr_pos = prev;
                steps += 1;
                if steps > 100_000 {
                    warn!("A* path reconstruction exceeded 100k steps, aborting");
                    break;
                }
            }
            points.reverse();
            let Pos(bx, by, bz) = best_pos.unwrap();
            points.push(Point { x: bx, y: by, z: bz, w: 0 });
            observer(AStarEvent::Path { x: bx, y: by, z: bz });
            info!("A* path length: {} tiles", points.len());
        } else {
            warn!("A* produced no result");
        }
    }
}
