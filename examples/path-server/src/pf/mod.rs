//! Pathfinding data types and orchestration.
//!
//! Core A* algorithm, block cache, and tuning types live in
//! [`framework::ecumene::pathfinding`] and are re-exported here for
//! convenience.  This module adds application-level concerns:
//!
//! - [`TraceRequest`] — HTTP/TCP request envelope (includes `world` id).
//! - [`task`] — async spawning, cancellation, worker integration.
//! - [`preloaded`] — lazy block fetcher backed by the worker channel.
//! - [`zone_adapter`] — adapts a live `Zone` as a `TileProvider`.

pub mod marker;
pub mod preloaded;
pub mod task;
pub mod visual;
pub mod los_visual;
pub mod zone_adapter;

use serde::Deserialize;
use u_core::Heading;

// ── Re-exports from framework ─────────────────────────────────────────────

pub use framework::ecumene::pathfinding::{TraceOptions, Point, Surveyor};

// ── TracePath request ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TraceRequest {
    pub world: u8,
    pub sx: isize,
    pub sy: isize,
    pub sz: i8,
    #[serde(default)]
    pub sdir: u8,
    pub dx: isize,
    pub dy: isize,
    pub dz: i8,
    #[serde(default)]
    pub ddir: u8,
    #[serde(default)]
    pub options: TraceOptions,
}

impl TraceRequest {
    /// Start direction as [`Heading`], defaulting to [`Heading::South`].
    pub fn start_heading(&self) -> Heading {
        Heading::from_raw(self.sdir).unwrap_or(Heading::South)
    }

    /// Destination direction as [`Heading`], defaulting to [`Heading::South`].
    pub fn dest_heading(&self) -> Heading {
        Heading::from_raw(self.ddir).unwrap_or(Heading::South)
    }
}
