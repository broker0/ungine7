//! LOS (line-of-sight) visualisation via spawned marker items.
//!
//! Two modes:
//!
//! - **Ray**: traces a single ray between two points, colouring each tile
//!   green (clear), red (first blocker), or grey (beyond blocker).
//! - **Field**: casts rays from a centre point to every tile on the
//!   perimeter of a square, collecting all intermediate tiles covered by
//!   the rays.  Each tile is coloured green (visible) or red (not visible).
//!
//! # Field scanning strategy
//!
//! The field mode does **not** check every tile independently.  Instead it
//! traces rays from the centre to each tile on the perimeter of the view
//! square (≈ `8 * radius` rays).  Every intermediate tile touched by a ray
//! inherits the visibility state at that point:
//!
//! - Tiles before the blocker → visible (green)
//! - Tiles at and beyond the blocker → blocked (red)
//!
//! This is controlled by [`FieldStrategy`].  If you prefer brute-force
//! per-tile checks, switch to [`FieldStrategy::EveryTile`].

use std::time::Duration;

use framework::ecumene::{LosValidator, TileProvider};
use framework::vessel::tile_shape::TileShape;

use crate::worker::PathServerWorkerTx;

use super::marker::{self, build_marker, SerialRange};

// ── Configuration ─────────────────────────────────────────────────────────

/// Default eye-height offset for humanoid mobiles.
pub const EYE_HEIGHT: i16 = 14;

/// Visual marker configuration for LOS visualisation.
#[derive(Debug, Clone)]
pub struct LosVisualConfig {
    /// Item graphic for markers (should be non-blocking in tiledata).
    /// Default: `0x0E73` (small gem).
    pub graphic: u16,
    /// Hue for clear (visible) tiles. Default: green `0x0043`.
    pub hue_clear: u16,
    /// Hue for the first blocking tile. Default: red `0x0026`.
    pub hue_blocked: u16,
    /// Hue for tiles beyond the blocker. Default: grey `0x0386`.
    pub hue_beyond: u16,
    /// Optional delay between spawning individual markers (for animated
    /// appearance).  Default: `Duration::ZERO` (all at once).
    pub step_delay: Duration,
    /// How long markers stay visible before automatic cleanup.
    /// Default: 3 seconds.
    pub linger: Duration,
}

impl Default for LosVisualConfig {
    fn default() -> Self {
        Self {
            graphic:     0x0E73,
            hue_clear:   0x0043,
            hue_blocked: 0x0026,
            hue_beyond:  0x0386,
            step_delay:  Duration::ZERO,
            linger:      Duration::from_secs(3),
        }
    }
}

/// How the field mode scans tiles.
///
/// Switch between perimeter-ray and brute-force strategies here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStrategy {
    /// Cast rays from the centre to every perimeter tile only.
    /// Intermediate tiles are coloured based on whether the ray was
    /// blocked before reaching them.  Fast: `O(8*radius)` LOS checks.
    Perimeter,
    /// Check `has_los` for every tile in the square independently.
    /// Thorough but slow: `O((2*radius+1)^2)` LOS checks.
    ///
    /// Alternative brute-force strategy; `run_field_every_tile` is fully
    /// implemented, but current call sites hardcode `Perimeter`.
    #[allow(dead_code)]
    EveryTile,
}

// ── Serial allocator ──────────────────────────────────────────────────────

/// LOS marker serials live in `0x6000_0000 ..= 0x6FFF_FFFE`, separate
/// from pathvis markers (`0x7000_0000..`).
static LOS_MARKER_SERIAL: SerialRange = SerialRange::new(0x6000_0000, 0x6FFF_FFFE);

// ── Terrain Z resolution ──────────────────────────────────────────────────

/// Resolve the standing Z at tile `(x, y)` from the tile stack.
///
/// Scans all `Surface` and `Slope` shapes in the tile stack and returns
/// the `z_stand` closest to `z_hint`.  Falls back to `z_hint` if the
/// tile stack is empty or contains only `Background` shapes.
///
/// This is simpler than `MovementValidator::resolve_standing_z` — it
/// ignores headroom, passability, and creature type — but is good
/// enough for placing visual markers on the ground.
fn resolve_marker_z<T: TileProvider>(provider: &T, x: u16, y: u16, z_hint: i8) -> i8 {
    use u_core::Heading;

    let shapes = provider.query_tile_stack(x, y, Heading::North);
    let mut best_z: Option<i8> = None;
    let mut best_dist: i16 = i16::MAX;

    for shape in &shapes {
        let z_stand = match shape {
            TileShape::Surface { z_stand, .. } => *z_stand,
            TileShape::Slope { z_stand, .. } => *z_stand,
            TileShape::Background { .. } => continue,
        };

        let dist = (z_stand as i16 - z_hint as i16).abs();
        if dist < best_dist {
            best_dist = dist;
            best_z = Some(z_stand);
        }
    }

    best_z.unwrap_or(z_hint)
}

// ── Ray visualisation ─────────────────────────────────────────────────────

/// Trace result with spawned marker serials for later cleanup.
pub struct LosVisualResult {
    /// All marker serials spawned during this visualisation.
    pub marker_serials: Vec<u32>,
    /// True if line of sight is clear.
    pub has_los: bool,
    /// Total tiles traced.
    pub total_tiles: u32,
    /// Number of visible (clear) tiles.
    pub clear_count: u32,
    /// Number of blocked tiles (including beyond).
    pub blocked_count: u32,
}

/// Run LOS ray visualisation in a blocking context.
///
/// Call this inside `tokio::task::spawn_blocking` with a `LazyBlockProvider`.
/// Returns the trace result; marker spawning is done via `Handle::block_on`.
///
/// `z1`/`z2` are full LOS Z (with eye-height offset), used for the LOS
/// check.  `z_hint` is the source entity's standing Z, used as the
/// fallback hint when resolving ground Z for marker placement.
pub fn run_los_ray_blocking<T: TileProvider>(
    provider: &T,
    x1: u16, y1: u16, z1: i16,
    x2: u16, y2: u16, z2: i16,
    z_hint: i8,
    config: &LosVisualConfig,
    handle: &tokio::runtime::Handle,
    worker_tx: &PathServerWorkerTx,
    world: u8,
) -> LosVisualResult {
    let los = LosValidator::new(provider);
    let trace = los.trace(x1, y1, z1, x2, y2, z2);

    let mut marker_serials = Vec::with_capacity(trace.tiles.len());
    let mut clear_count: u32 = 0;
    let mut blocked_count: u32 = 0;

    let first_blocker = trace.first_blocker();

    for (idx, &(tx, ty, _tz)) in trace.tiles.iter().enumerate() {
        let is_blocker = trace.blockers.contains(&idx);
        let hue = if is_blocker {
            // This tile itself blocks LOS — red.
            config.hue_blocked
        } else if first_blocker.is_some_and(|fb| idx > fb) {
            // Beyond the first blocker but not a blocker itself — grey.
            config.hue_beyond
        } else {
            // Before first blocker (or no blocker at all) — green.
            config.hue_clear
        };

        if hue == config.hue_clear {
            clear_count += 1;
        } else {
            blocked_count += 1;
        }

        // Place marker on the ground (terrain standing Z), not at the
        // ray's interpolated eye-height Z.
        let marker_z = resolve_marker_z(provider, tx, ty, z_hint);
        let serial = LOS_MARKER_SERIAL.alloc();
        let entity = build_marker(serial, config.graphic, tx, ty, marker_z, hue);
        marker_serials.push(serial);

        let wtx = worker_tx.clone();
        handle.block_on(async {
            marker::spawn_marker(&wtx, world, serial, entity).await;
        });

        if !config.step_delay.is_zero() {
            std::thread::sleep(config.step_delay);
        }
    }

    LosVisualResult {
        has_los: trace.has_los,
        total_tiles: trace.tiles.len() as u32,
        clear_count,
        blocked_count,
        marker_serials,
    }
}

// ── Field visualisation ───────────────────────────────────────────────────

/// Run LOS field visualisation in a blocking context.
///
/// Scans the area around `(cx, cy, cz)` with the given `radius` and spawns
/// coloured markers for each tile.
///
/// `is_mobile`: if true, adds [`EYE_HEIGHT`] to the source Z for LOS checks.
///
/// `strategy`: controls whether rays are cast to perimeter only or every tile.
///
/// `invert_hues`: when `true`, swaps clear/beyond colours.  Used when
/// visualising an *enemy* entity's field of view: visible tiles (danger)
/// are red, non-visible tiles (safe) are green.  Blocker tiles stay red.
pub fn run_los_field_blocking<T: TileProvider>(
    provider: &T,
    cx: u16, cy: u16, cz: i8,
    radius: u16,
    is_mobile: bool,
    strategy: FieldStrategy,
    invert_hues: bool,
    config: &LosVisualConfig,
    handle: &tokio::runtime::Handle,
    worker_tx: &PathServerWorkerTx,
    world: u8,
) -> LosVisualResult {
    let z_source: i16 = cz as i16 + if is_mobile { EYE_HEIGHT } else { 0 };

    match strategy {
        FieldStrategy::Perimeter => run_field_perimeter(
            provider, cx, cy, z_source, cz, radius, invert_hues, config, handle, worker_tx, world,
        ),
        FieldStrategy::EveryTile => run_field_every_tile(
            provider, cx, cy, z_source, cz, radius, invert_hues, config, handle, worker_tx, world,
        ),
    }
}

/// Per-tile visibility state for field visualisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileVis {
    /// Visible: before any blocker on at least one ray.
    Clear,
    /// This tile itself blocks LOS on at least one ray.
    Blocker,
    /// Beyond the first blocker — not a blocker itself.
    Beyond,
}

/// Perimeter strategy: cast rays from centre to each perimeter tile.
///
/// Tiles touched by rays inherit their LOS state at that point.
/// If multiple rays pass through the same tile, the best state wins
/// (Clear > Beyond > Blocker — optimistic merge for non-blockers).
fn run_field_perimeter<T: TileProvider>(
    provider: &T,
    cx: u16, cy: u16, z_source: i16, cz: i8,
    radius: u16,
    invert_hues: bool,
    config: &LosVisualConfig,
    handle: &tokio::runtime::Handle,
    worker_tx: &PathServerWorkerTx,
    world: u8,
) -> LosVisualResult {
    let los = LosValidator::new(provider);

    // Collect perimeter tiles of the square.
    let x_min = cx.saturating_sub(radius);
    let y_min = cy.saturating_sub(radius);
    let x_max = cx.saturating_add(radius);
    let y_max = cy.saturating_add(radius);

    let mut perimeter: Vec<(u16, u16)> = Vec::new();
    // Top and bottom edges
    for x in x_min..=x_max {
        perimeter.push((x, y_min));
        if y_max != y_min {
            perimeter.push((x, y_max));
        }
    }
    // Left and right edges (excluding corners already added)
    for y in (y_min + 1)..y_max {
        perimeter.push((x_min, y));
        if x_max != x_min {
            perimeter.push((x_max, y));
        }
    }

    // Map from tile (x, y) → best visibility state.
    let mut tile_vis: std::collections::HashMap<(u16, u16), TileVis> =
        std::collections::HashMap::new();

    for &(px, py) in &perimeter {
        // Resolve standing Z at the perimeter tile and use it (+ eye
        // height) as the ray target, so rays follow terrain contour
        // instead of all targeting the same flat Z.
        let pz_stand = resolve_marker_z(provider, px, py, cz);
        let z_target = pz_stand as i16 + (z_source - cz as i16); // same eye offset

        let trace = los.trace(cx, cy, z_source, px, py, z_target);

        let first_blocker = trace.first_blocker();

        for (idx, &(tx, ty, _tz)) in trace.tiles.iter().enumerate() {
            let is_blocker = trace.blockers.contains(&idx);
            let vis = if is_blocker {
                TileVis::Blocker
            } else if first_blocker.is_some_and(|fb| idx > fb) {
                TileVis::Beyond
            } else {
                TileVis::Clear
            };

            // Optimistic merge: Clear beats Beyond beats Blocker.
            // If any ray sees a tile as Clear, it stays Clear.
            // A Blocker can be upgraded to Beyond if another ray passes
            // through it without blocking, and to Clear if it's visible
            // on some other ray.
            let entry = tile_vis.entry((tx, ty)).or_insert(vis);
            match (*entry, vis) {
                (_, TileVis::Clear) => *entry = TileVis::Clear,
                (TileVis::Blocker, TileVis::Beyond) => *entry = TileVis::Beyond,
                _ => {}
            }
        }
    }

    // Spawn markers for all discovered tiles.
    let mut marker_serials = Vec::with_capacity(tile_vis.len());
    let mut clear_count: u32 = 0;
    let mut blocked_count: u32 = 0;

    // Sort tiles for deterministic spawn order (top-left to bottom-right).
    let mut tiles: Vec<((u16, u16), TileVis)> = tile_vis.into_iter().collect();
    tiles.sort_by_key(|&((x, y), _)| (y, x));

    for ((tx, ty), vis) in &tiles {
        // When inverted (enemy field): visible = danger (red), hidden = safe (green).
        // Blockers always stay red.
        let hue = match vis {
            TileVis::Clear   => if invert_hues { config.hue_blocked } else { config.hue_clear },
            TileVis::Blocker => config.hue_blocked,
            TileVis::Beyond  => if invert_hues { config.hue_clear } else { config.hue_beyond },
        };
        if *vis == TileVis::Clear {
            clear_count += 1;
        } else {
            blocked_count += 1;
        }

        let marker_z = resolve_marker_z(provider, *tx, *ty, cz);
        let serial = LOS_MARKER_SERIAL.alloc();
        let entity = build_marker(serial, config.graphic, *tx, *ty, marker_z, hue);
        marker_serials.push(serial);

        let wtx = worker_tx.clone();
        handle.block_on(async {
            marker::spawn_marker(&wtx, world, serial, entity).await;
        });

        if !config.step_delay.is_zero() {
            std::thread::sleep(config.step_delay);
        }
    }

    LosVisualResult {
        has_los: true, // not meaningful for field mode
        total_tiles: tiles.len() as u32,
        clear_count,
        blocked_count,
        marker_serials,
    }
}

/// Brute-force strategy: check LOS for every tile in the square.
///
/// Colours: green = visible, red = tile itself blocks LOS (wall/tree),
/// grey = not visible (something between this tile and the centre blocks).
fn run_field_every_tile<T: TileProvider>(
    provider: &T,
    cx: u16, cy: u16, z_source: i16, cz: i8,
    radius: u16,
    invert_hues: bool,
    config: &LosVisualConfig,
    handle: &tokio::runtime::Handle,
    worker_tx: &PathServerWorkerTx,
    world: u8,
) -> LosVisualResult {
    let los = LosValidator::new(provider);

    let x_min = cx.saturating_sub(radius);
    let y_min = cy.saturating_sub(radius);
    let x_max = cx.saturating_add(radius);
    let y_max = cy.saturating_add(radius);

    let mut marker_serials = Vec::new();
    let mut clear_count: u32 = 0;
    let mut blocked_count: u32 = 0;

    for ty in y_min..=y_max {
        for tx in x_min..=x_max {
            if tx == cx && ty == cy {
                continue; // skip centre tile
            }

            // Resolve terrain Z at the target tile for both the LOS
            // check and the marker placement.
            let tz_stand = resolve_marker_z(provider, tx, ty, cz);
            let z_target = tz_stand as i16 + (z_source - cz as i16);
            let visible = los.has_los(cx, cy, z_source, tx, ty, z_target);

            // Check if this tile itself has blocking shapes (wall, tree, etc.)
            let tile_is_blocker = {
                use u_core::Heading;
                let shapes = provider.query_tile_stack(tx, ty, Heading::North);
                shapes.iter().any(|s| s.blocks_los())
            };

            let hue = if tile_is_blocker {
                config.hue_blocked  // red: this tile is a wall/tree (always)
            } else if visible {
                if invert_hues { config.hue_blocked } else { config.hue_clear }
            } else {
                if invert_hues { config.hue_clear } else { config.hue_beyond }
            };

            if hue == config.hue_clear {
                clear_count += 1;
            } else {
                blocked_count += 1;
            }

            let serial = LOS_MARKER_SERIAL.alloc();
            let entity = build_marker(serial, config.graphic, tx, ty, tz_stand, hue);
            marker_serials.push(serial);

            let wtx = worker_tx.clone();
            handle.block_on(async {
                marker::spawn_marker(&wtx, world, serial, entity).await;
            });

            if !config.step_delay.is_zero() {
                std::thread::sleep(config.step_delay);
            }
        }
    }

    LosVisualResult {
        has_los: true,
        total_tiles: (clear_count + blocked_count),
        clear_count,
        blocked_count,
        marker_serials,
    }
}

// ── Cleanup ───────────────────────────────────────────────────────────────

/// Remove all LOS marker items.
pub async fn cleanup_los_markers(
    serials: &[u32],
    worker_tx: &PathServerWorkerTx,
    world: u8,
) {
    marker::remove_markers_batch(serials.to_vec(), worker_tx, world, "losvis").await;
}
