//! Concrete UO entity model, entity store, command handler, and RPC helpers
//! for the shadow continuum.
//!
//! This module provides everything needed to run a server-side physics zone
//! with UO entities:
//!
//! - [`entity::DemoEntity`] — three-variant enum (Item/Multi/Mobile)
//! - [`store::DemoStore`] — `HashMap`-based [`EntityStore`](framework::continuum::EntityStore)
//! - [`handler::EngineCommand`] / [`handler::EngineHandler`] — command enum and
//!   [`CommandHandler`](framework::continuum::CommandHandler) implementation
//! - [`rpc`] — async RPC helpers for communicating with the shadow worker
//! - [`ingest`] — S->C packet parsing into entity maps

pub mod auth;
pub mod base_handler;
pub mod controller;
pub mod entity;
pub mod handler;
pub mod ingest;
pub mod item_props;
pub mod log_loader;
pub mod mirror;
pub mod notoriety;
pub mod observer;
pub mod rpc;
pub mod serial_alloc;
pub mod snapshot;
pub mod stackable;
pub mod store;

use std::sync::Arc;

use u_core::Heading;
use files::map::MapTile;
use files::multi::MultiPart;
use files::statics::StaticTile;
use files::tiledata::{LandTile, StaticTileDef};

use framework::ecumene::StaticDataProvider;
use framework::ecumene::StaticWorldData;

/// Wraps `Option<Arc<StaticWorldData>>` and implements [`StaticDataProvider`],
/// delegating to the inner value when present.
pub struct StaticData(pub Option<Arc<StaticWorldData>>);

impl StaticDataProvider for StaticData {
    fn land_tile_def(&self, tile_id: u16) -> Option<&LandTile> {
        self.0.as_deref()?.land_tile_def(tile_id)
    }

    fn static_tile_def(&self, tile_id: u16) -> Option<&StaticTileDef> {
        self.0.as_deref()?.static_tile_def(tile_id)
    }

    fn land_tile_at(&self, world: u8, x: u16, y: u16) -> Option<&MapTile> {
        self.0.as_deref()?.land_tile_at(world, x, y)
    }

    fn land_tile_z_stand(&self, world: u8, x: u16, y: u16, direction: Heading) -> Option<(i8, i8, i8)> {
        self.0.as_deref()?.land_tile_z_stand(world, x, y, direction)
    }

    fn statics_at(&self, world: u8, x: u16, y: u16) -> Option<&[StaticTile]> {
        self.0.as_deref()?.statics_at(world, x, y)
    }

    fn multi_parts(&self, graphic: u16) -> &[MultiPart] {
        match &self.0 {
            Some(sd) => sd.multi_parts(graphic),
            None => &[],
        }
    }
}
