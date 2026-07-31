//! Script manager adapter for demo-server.
//!
//! Delegates lifecycle management to [`framework::mitos::run_script_manager`],
//! wrapping the demo-server's [`DemoBackend`].
//! Provides entity cleanup via serial allocator snapshots.

use std::path::PathBuf;
use std::sync::Arc;

use log::info;
use tokio::sync::broadcast;

use framework::continuum::{WorkerCommand, WorldEvent};
use framework::mitos;

use common::uo_engine::handler::EngineCommand;
use common::uo_engine::serial_alloc::{AllocSnapshot, SerialAllocator};

use crate::{DemoCommand, DemoWorkerTx};

use super::runtime::DemoBackend;

// ── Re-export command type ────────────────────────────────────────────────

/// Commands that can be sent to the Lua script manager.
///
/// The variant names changed from the old API:
/// - `Run(PathBuf)` → `RunFile(PathBuf)`
/// - `Reload` stays `Reload`
/// - `Stop` stays `Stop`
/// - `RunCode { code, reply }` is new (for WebSocket eval)
pub type LuaCommand = mitos::ScriptCommand;

// ── Lifecycle callbacks ───────────────────────────────────────────────────

/// Demo-server specific lifecycle hooks.
struct DemoCallbacks {
    worker_tx: DemoWorkerTx,
    map_id: u8,
    serial_alloc: Arc<SerialAllocator>,
    snapshot: Option<AllocSnapshot>,
}

impl mitos::ManagerCallbacks for DemoCallbacks {
    fn on_before_spawn(&mut self) {
        self.snapshot = Some(self.serial_alloc.snapshot());
    }

    fn on_after_stop(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            let Some(snap) = self.snapshot.take() else { return };
            let freed = self.serial_alloc.free_since(&snap);
            if freed.is_empty() {
                return;
            }

            info!(
                "[lua] cleanup: removing {} entity/entities via allocator snapshot",
                freed.len(),
            );

            for serial in freed {
                let cmd = WorkerCommand::MapCommand(
                    self.map_id,
                    DemoCommand::Engine(EngineCommand::RemoveEntity { entity_id: serial }),
                );
                let _ = self.worker_tx.send(cmd).await;
            }
        })
    }
}

// ── Command loop ──────────────────────────────────────────────────────────

/// Run the Lua script manager command loop.
///
/// Wraps [`framework::mitos::run_script_manager`] with the demo backend
/// and entity cleanup callbacks.  Scripts subscribe directly to the
/// `lua_broadcast_tx` channel (capacity 65536) — no intermediate bridge.
pub async fn run_lua_manager(
    worker_tx: DemoWorkerTx,
    event_tx: tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    lua_broadcast_tx: broadcast::Sender<WorldEvent>,
    cmd_rx: tokio::sync::mpsc::Receiver<LuaCommand>,
    map_id: u8,
    initial_script: Option<PathBuf>,
    serial_alloc: Arc<SerialAllocator>,
    scripts_dir: PathBuf,
) {
    let backend = DemoBackend {
        worker_tx: worker_tx.clone(),
        event_tx,
        serial_alloc: serial_alloc.clone(),
        scripts_dir: scripts_dir.clone(),
    };

    let callbacks = DemoCallbacks {
        worker_tx,
        map_id,
        serial_alloc,
        snapshot: None,
    };

    mitos::run_script_manager(
        backend,
        lua_broadcast_tx,
        cmd_rx,
        initial_script,
        Some(scripts_dir),
        callbacks,
    ).await;
}
