//! Foundational entity & tile abstractions.
//!
//! This module defines the base traits and types that the rest of the
//! continuum depends on: [`Entity`] (world entity abstraction),
//! [`TileShape`] (physical tile geometry), and [`StaticDataProvider`]
//! (read-only access to tiledata, map, statics, and multi definitions).
//!
//! These types are intentionally kept separate from [`crate::ecumene`] so
//! that higher-level modules (`continuum/`, `diorama/`) can depend on the
//! abstractions without pulling in the full world machinery.

pub mod tile_shape;
pub mod traits;
pub mod objects;

pub use tile_shape::TileShape;
pub use traits::StaticDataProvider;
pub use objects::{EntitySnapshot, Entity, NotorietyContext};
