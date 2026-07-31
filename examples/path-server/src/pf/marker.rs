//! Shared marker-entity helpers for pathfinding/LOS visualisation.
//!
//! `visual.rs` (pathvis) and `los_visual.rs` (losvis) both spawn coloured
//! gem items to visualise their algorithms.  This module centralises the
//! common building blocks — marker construction, serial allocation, spawn,
//! and batch cleanup — so the two visualisers stay in sync.

use std::sync::atomic::{AtomicU32, Ordering};

use log::debug;

use framework::continuum::WorkerCommand;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::EngineCommand;

use crate::worker::{PathServerCommand, PathServerWorkerTx};

// ── Serial allocator ──────────────────────────────────────────────────────

/// A wrap-around serial allocator confined to a fixed range.
///
/// Marker serials live in dedicated ranges so they never collide with
/// player serials (`0x0000_0001..0x3FFF_FFFF`), mount serials
/// (`0x4000_0000..`), or item serials from normal gameplay:
///
/// - pathvis markers: `0x7000_0000 ..= 0x7FFF_FFFE`
/// - losvis  markers: `0x6000_0000 ..= 0x6FFF_FFFE`
pub struct SerialRange {
    next: AtomicU32,
    base: u32,
    ceiling: u32,
}

impl SerialRange {
    /// Create an allocator spanning `base ..= ceiling`.
    pub const fn new(base: u32, ceiling: u32) -> Self {
        Self {
            next: AtomicU32::new(base),
            base,
            ceiling,
        }
    }

    /// Allocate the next serial, wrapping back to `base` once the ceiling is
    /// exceeded (extremely unlikely in practice).
    pub fn alloc(&self) -> u32 {
        let s = self.next.fetch_add(1, Ordering::Relaxed);
        if s > self.ceiling {
            self.next.store(self.base, Ordering::Relaxed);
        }
        s
    }
}

// ── Marker entity builder ─────────────────────────────────────────────────

/// Build a `DemoEntity::Item` from typed fields.  The client sees a coloured
/// gem.  Uses a non-blocking graphic (no `IMPASSABLE` flag) so markers never
/// interfere with movement or the search itself.
pub fn build_marker(serial: u32, graphic: u16, x: u16, y: u16, z: i8, hue: u16) -> DemoEntity {
    DemoEntity::Item {
        serial,
        graphic,
        color: hue,
        amount: 1,
        x,
        y,
        z,
        is_container: false,
        hidden: false,
        facing: None,
    }
}

// ── Spawn / cleanup helpers ───────────────────────────────────────────────

/// Spawn a single marker entity into `world` via the worker.
pub async fn spawn_marker(
    worker_tx: &PathServerWorkerTx,
    world: u8,
    serial: u32,
    entity: DemoEntity,
) {
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            world,
            PathServerCommand::Engine(EngineCommand::SpawnEntity {
                entity_id: serial,
                data: entity,
            }),
        ))
        .await;
}

/// Batch-remove markers from `world` in a single worker command.
///
/// Uses [`PathServerCommand::RemoveEntitiesBatch`], which removes all entities
/// from the zone and routes `EntityRemoved` events directly through the
/// observer registry (per-session mpsc), bypassing the broadcast channel.
/// This avoids broadcast overflow when removing large numbers of markers.
///
/// `tag` is the log prefix (e.g. `"visual"` / `"losvis"`).
pub async fn remove_markers_batch(
    serials: Vec<u32>,
    worker_tx: &PathServerWorkerTx,
    world: u8,
    tag: &str,
) {
    if serials.is_empty() {
        return;
    }
    let n = serials.len();
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            world,
            PathServerCommand::RemoveEntitiesBatch { serials },
        ))
        .await;
    debug!("[{tag}] cleaned up {n} markers");
}
