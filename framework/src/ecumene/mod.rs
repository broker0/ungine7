//! Shared world model — static terrain, movement validation, pathfinding,
//! and multi-object support.
//!
//! This module is self-contained and can be used independently of `continuum/`.
//! It provides everything needed for movement validation (tile shapes,
//! providers, multi-object registry, collision snapshots, and the movement
//! algorithm) as well as A* pathfinding ([`pathfinding`]).
//!
//! Session-level observation state (visible set, session view, diorama tile
//! provider) has moved to [`crate::diorama`] and is re-exported here for
//! backwards compatibility.
//!
//! # Architecture
//!
//! | Level | Struct | Sharing | Mutability |
//! |-------|--------|---------|------------|
//! | 0 | [`StaticWorldData`] | `Arc`, all sessions | Immutable |
//! | 0.5 | [`DiffOverlay`] | Owned, per-session | Mutable (direct) |
//! | 1 | Engine zones (in `continuum/`) | per-session | Mutable via async RPC |
//! | 2 | [`SessionView`](crate::diorama::SessionView) / [`VisibleWorld`](crate::diorama::VisibleWorld) (in `diorama/`) | Owned, per-session | Mutable (direct) |

// ── Primitives ────────────────────────────────────────────────────────────
pub mod tile_rect;
pub mod tile_block;

// ── Traits ────────────────────────────────────────────────────────────────
pub mod tile_provider;

// ── Multi-object support ──────────────────────────────────────────────────
pub mod multi_def;
pub mod multi_spatial;
pub mod shape_provider;
pub mod entity_registry;

// ── Providers & movement ──────────────────────────────────────────────────
pub mod static_provider;
pub mod snapshot;
pub mod movement;
pub mod line_of_sight;

// ── Pathfinding ───────────────────────────────────────────────────────────
pub mod pathfinding;

// ── Map diff overlay ──────────────────────────────────────────────────────
pub mod land_z;
pub mod diff_overlay;
pub mod diff_provider;

// ── Static world data ─────────────────────────────────────────────────────
pub mod static_world_data;

// ── Re-exports (world-local) ──────────────────────────────────────────────
pub use tile_rect::TileRect;
pub use crate::vessel::tile_shape::TileShape;
pub use tile_block::TileBlock;
pub use tile_provider::TileProvider;
pub use crate::vessel::traits::StaticDataProvider;
pub use crate::vessel::objects::Entity;
pub use multi_def::{MultiDef, MultiExtent, PartEntry};
pub use multi_spatial::{SpatialIndex, BlockSpatialIndex, BBoxSpatialIndex};
pub use shape_provider::ShapeProvider;
pub use entity_registry::{EntityRegistry, CacheMode, StalenessConfig};
pub use static_provider::StaticTileProvider;
pub use snapshot::CollisionSnapshot;
pub use movement::MovementValidator;
pub use line_of_sight::LosValidator;
pub use line_of_sight::LosRay;
pub use line_of_sight::LosTrace;
pub use static_world_data::StaticWorldData;
pub use diff_overlay::DiffOverlay;
pub use diff_provider::DiffAwareDataProvider;
pub use pathfinding::{Surveyor, CachingProvider, CacheStats, TraceOptions, DistanceFunc, Point};
