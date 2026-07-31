//! A* pathfinding over tile-based UO terrain.
//!
//! This module provides a generic A* pathfinder ([`Surveyor`]) and a
//! block-level tile cache ([`CachingProvider`]) that work with any
//! [`TileProvider`](super::TileProvider) implementation.
//!
//! The data types ([`TraceOptions`], [`DistanceFunc`], [`Point`]) describe
//! the search parameters and results.  They are independent of any I/O or
//! async runtime — all async orchestration (spawning, cancellation, lazy
//! block fetching) belongs in the application layer.
//!
//! # Architecture
//!
//! ```text
//! Application layer (e.g. path-server)
//!   │
//!   │  provides a TileProvider, builds TraceOptions
//!   ▼
//! Surveyor<T: TileProvider>::trace_a_star(...)
//!   │
//!   │  uses CachingProvider internally for block-level caching
//!   │  delegates step validation to movement free functions
//!   ▼
//! ecumene::movement::{compute_source_range, compute_dest_position}
//! ```

pub mod cache;
pub mod surveyor;

// ── Re-exports ────────────────────────────────────────────────────────────

pub use cache::{CachingProvider, CacheStats};
pub use surveyor::Surveyor;

// ── A* observer callback types ────────────────────────────────────────────

/// Events emitted during A* search for external observation / visualisation.
#[derive(Debug, Clone)]
pub enum AStarEvent {
    /// Node expanded — moved from frontier (open set) to visited (closed set).
    Visited { x: isize, y: isize, z: i8, g: isize },
    /// Node pushed into the frontier (open set).
    Frontier { x: isize, y: isize, z: i8, f: isize },
    /// Final path node (emitted during result reconstruction).
    Path { x: isize, y: isize, z: i8 },
}

/// Control flow response from the observer callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AStarAction {
    /// Continue the search.
    Continue,
    /// Cancel the search immediately.
    Cancel,
}

// ── Distance function ─────────────────────────────────────────────────────

/// Heuristic distance function for A* search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DistanceFunc {
    Manhattan,
    Chebyshev,
    Diagonal,
    Euclidean,
}

impl Default for DistanceFunc {
    fn default() -> Self {
        Self::Diagonal
    }
}

// ── Trace options ─────────────────────────────────────────────────────────

/// Full set of A* tuning parameters, all optional (sensible defaults apply).
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TraceOptions {
    // ── Movement mode ──────────────────────────────────────────────────────
    /// Allow diagonal movement (NE/SE/SW/NW). Default: false.
    pub allow_diagonal_move: Option<bool>,
    /// Enable flying movement (gargoyle hover-over tiles). Default: false.
    pub fly: Option<bool>,
    /// Extra passable-override mask (raw TileFlags bits). Default: 0.
    pub passable_mask: Option<u64>,

    // ── Heuristic ─────────────────────────────────────────────────────────
    /// Distance function for heuristic. Default: Diagonal.
    pub heuristic_distance: Option<DistanceFunc>,
    /// Weight of a straight step in the heuristic. Default: 5.
    pub heuristic_straight: Option<isize>,
    /// Weight of a diagonal step in the heuristic. Default: heuristic_straight.
    pub heuristic_diagonal: Option<isize>,

    // ── Cost model ────────────────────────────────────────────────────────
    /// Penalty for changing direction. Default: 1.
    pub cost_turn: Option<isize>,
    /// Cost of a straight step. Default: 1.
    pub cost_move_straight: Option<isize>,
    /// Cost of a diagonal step. Default: cost_move_straight.
    pub cost_move_diagonal: Option<isize>,
    /// Extra cost when the destination tile is occupied by a multi. Default: 0.
    pub cost_move_multi: Option<isize>,
    /// Abort if accumulated cost exceeds this. Default: isize::MAX.
    pub cost_limit: Option<isize>,

    // ── Search area ────────────────────────────────────────────────────────
    pub left:   Option<isize>,
    pub top:    Option<isize>,
    pub right:  Option<isize>,
    pub bottom: Option<isize>,

    // ── Goal tolerance ─────────────────────────────────────────────────────
    /// Max X distance from goal to be considered "reached". Default: 0.
    pub accuracy_x: Option<isize>,
    /// Max Y distance from goal to be considered "reached". Default: 0.
    pub accuracy_y: Option<isize>,
    /// Max Z distance from goal to be considered "reached". Default: 0.
    pub accuracy_z: Option<isize>,

    // ── Output control ────────────────────────────────────────────────────
    /// Return all explored tiles instead of just the path. Default: false.
    pub all_points: Option<bool>,
    /// Abort search after this many milliseconds. Default: none (unlimited).
    pub time_limit: Option<isize>,
}

// ── Point ─────────────────────────────────────────────────────────────────

/// A single point in the result path (or explored-set when all_points=true).
///
/// `w` carries the g-score when `all_points` is true; it is `0` for path points.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Point {
    pub x: isize,
    pub y: isize,
    pub z: i8,
    pub w: isize,
}
