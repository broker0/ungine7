//! Player-position observation and session tracking.
//!
//! The `diorama` module provides packet-driven observation of the game world.
//! It is used by replay tools, analysers, bots, and any code that needs to
//! follow the player character and visible objects through a stream of UO
//! packets.
//!
//! Movement primitives (position tracking, pending queues, active movement,
//! arbitration) live in the [`rythmos`](crate::rythmos) module.  This
//! module builds on top of `rythmos` by adding session-level tracking
//! (visible world, multi registry, view range) and tile-based Z resolution.
//!
//! # Components
//!
//! - [`VisibleWorld`] / [`VisibleItem`] — per-session set of objects the
//!   client currently sees (with integrated container cache), maintained by
//!   ingesting S→C packets.  [`VisibleSet`] is a backward-compatible alias.
//! - [`SessionView`] — per-session mutable state aggregating the current
//!   world index, visible world, multi registry, and feature flags.
//! - [`CompositeTileProvider`] — a [`TileProvider`](crate::ecumene::TileProvider)
//!   that layers visible items and multi shapes on top of static map data
//!   for client-side movement validation.  Also implements
//!   [`ZResolver`](crate::rythmos::ZResolver).
//! - [`ObserverPipeline`] — unified single-pass S→C / C→S processor that
//!   combines session tracking with position tracking and movement
//!   prediction.

pub mod bootstrap;
pub mod observer_event;
pub mod pipeline;
pub mod session_view;
pub mod staleness;
pub mod visible_world;
pub mod composite_tiles;

pub use bootstrap::generate_bootstrap;
pub use observer_event::{ObserverEvent, PopupMenuEntry};
pub use pipeline::{DrainReason, ObserverPipeline};
pub use session_view::SessionView;
pub use staleness::StalenessTracker;
pub use visible_world::{EntityData, VisibleItem, VisibleKind, VisibleSet, VisibleWorld, WorldEntity};
pub use composite_tiles::CompositeTileProvider;

// ── Packet → domain container conversion helpers ─────────────────────────
//
// These live in `diorama` because the diorama layer is already
// packet-aware, while `continuum` (where ContainerItem lives) is not.

use crate::continuum::ContainerItem;

/// Convert a parsed `ContainerContent` packet into a `Vec<ContainerItem>`.
pub fn container_items_from_content(cc: &packets::interaction::ContainerContent) -> Vec<ContainerItem> {
    match cc {
        packets::interaction::ContainerContent::Legacy(items) => {
            items.iter().map(|i| ContainerItem {
                serial: i.serial,
                graphic: i.graphic,
                amount: i.amount,
                x: i.x,
                y: i.y,
                color: i.color,
                grid_index: None,
            }).collect()
        }
        packets::interaction::ContainerContent::Modern(items) => {
            items.iter().map(|i| ContainerItem {
                serial: i.serial,
                graphic: i.graphic,
                amount: i.amount,
                x: i.x,
                y: i.y,
                color: i.color,
                grid_index: Some(i.grid_index),
            }).collect()
        }
    }
}

/// Convert a parsed `AddItemToContainer` packet into a `ContainerItem`.
pub fn container_item_from_add(add: &packets::interaction::AddItemToContainer) -> ContainerItem {
    ContainerItem {
        serial: add.serial(),
        graphic: add.graphic(),
        amount: add.amount(),
        x: add.x(),
        y: add.y(),
        color: add.color(),
        grid_index: add.grid_index(),
    }
}
