//! Standalone pathfinding task.
//!
//! [`PathfindTask`] runs A* in a separate `tokio::task::spawn_blocking`
//! thread, fetching collision blocks lazily from the worker in small batches
//! as the A* frontier advances.
//!
//! # Flow
//!
//! ```text
//!  caller
//!    │  run_pathfind(request)
//!    ▼
//!  PathfindTask::spawn()  ──tokio::spawn──►  async run()
//!                                                │
//!                                                │ spawn_blocking {
//!                                                │   LazyBlockProvider::new(handle, worker_tx)
//!                                                │   Surveyor::trace_a_star(...)
//!                                                │     └─ on cache miss:
//!                                                │          handle.block_on(GetCollisionBlocks)
//!                                                │          → 9×9 block batch from worker
//!                                                │ }
//!                                                └─ reply.send(PathfindResult)
//! ```
//!
//! The worker is never blocked for the full search — it only serves small
//! batch requests as each new region of the map is entered by the frontier.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use log::warn;
use tokio::runtime::Handle;

use crate::worker::PathServerWorkerTx;
use super::preloaded::LazyBlockProvider;
use super::{Surveyor, Point, TraceRequest};
use framework::ecumene::pathfinding::AStarAction;

// ── PathfindResult ────────────────────────────────────────────────────────

/// Outcome of a [`PathfindTask`].
#[derive(Debug)]
pub enum PathfindResult {
    /// A path (or partial path when the goal was unreachable) was found.
    ///
    /// The `Vec<Point>` is in start→end order.  It may be shorter than the
    /// true optimal path if `time_limit` was reached.
    Found(Vec<Point>),
    /// The search was cancelled before a result was produced.
    Cancelled,
    /// The worker channel closed before block data could be fetched.
    WorkerGone,
}

// ── PathfindTask ──────────────────────────────────────────────────────────

/// Parameters for a single pathfinding task.
pub struct PathfindTask {
    request:    TraceRequest,
    map_width:  isize,
    map_height: isize,
    worker_tx:  PathServerWorkerTx,
    cancel:     Arc<AtomicBool>,
    reply:      tokio::sync::oneshot::Sender<PathfindResult>,
}

impl PathfindTask {
    pub fn new(
        request:    TraceRequest,
        map_width:  isize,
        map_height: isize,
        worker_tx:  PathServerWorkerTx,
        cancel:     Arc<AtomicBool>,
        reply:      tokio::sync::oneshot::Sender<PathfindResult>,
    ) -> Self {
        Self { request, map_width, map_height, worker_tx, cancel, reply }
    }

    /// Spawn the task.  Returns immediately; the result is sent via `reply`.
    pub fn spawn(self) {
        tokio::spawn(self.run());
    }

    async fn run(self) {
        let Self { request, map_width, map_height, worker_tx, cancel, reply } = self;

        if cancel.load(Ordering::Relaxed) {
            let _ = reply.send(PathfindResult::Cancelled);
            return;
        }

        // ── Determine search bounds ───────────────────────────────────
        let left   = request.options.left  .unwrap_or(0)          .max(0) as u16;
        let top    = request.options.top   .unwrap_or(0)          .max(0) as u16;
        let right  = request.options.right .unwrap_or(map_width)  .clamp(0, u16::MAX as isize) as u16;
        let bottom = request.options.bottom.unwrap_or(map_height) .clamp(0, u16::MAX as isize) as u16;

        // ── Capture runtime handle before entering spawn_blocking ─────
        // Handle::current() must be called from an async context.
        let handle = Handle::current();

        let cancel_clone = Arc::clone(&cancel);
        let map_id = request.world;

        let sx = request.sx; let sy = request.sy; let sz = request.sz;
        let sdir = request.start_heading();
        let dx = request.dx; let dy = request.dy; let dz = request.dz;
        let ddir = request.dest_heading();
        let opts = request.options.clone();

        // Clamp the A* search bounds to our tile rectangle.
        let mut search_opts = opts.clone();
        search_opts.left   = Some(search_opts.left  .unwrap_or(0) .max(left  as isize));
        search_opts.top    = Some(search_opts.top   .unwrap_or(0) .max(top   as isize));
        search_opts.right  = Some(search_opts.right .unwrap_or(map_width) .min(right as isize + 1));
        search_opts.bottom = Some(search_opts.bottom.unwrap_or(map_height).min(bottom as isize + 1));

        // ── Run A* on a blocking thread ───────────────────────────────
        // LazyBlockProvider fetches blocks in 9×9 batches on demand via
        // handle.block_on(), which is safe inside spawn_blocking.
        let result = tokio::task::spawn_blocking(move || {
            if cancel_clone.load(Ordering::Relaxed) {
                return PathfindResult::Cancelled;
            }

            let provider = LazyBlockProvider::new(map_id, handle, worker_tx);

            let surveyor = Surveyor::with_options(&provider, &search_opts);
            let mut points = Vec::new();

            surveyor.trace_a_star(
                sx, sy, sz, sdir,
                dx, dy, dz, ddir,
                &mut points,
                &search_opts,
                map_width,
                map_height,
                |_| AStarAction::Continue,
            );

            provider.log_stats();

            if cancel_clone.load(Ordering::Relaxed) && points.is_empty() {
                return PathfindResult::Cancelled;
            }

            PathfindResult::Found(points)
        })
        .await;

        let outcome = result.unwrap_or_else(|e| {
            warn!("pathfind spawn_blocking panicked: {e}");
            PathfindResult::Found(Vec::new())
        });

        let _ = reply.send(outcome);
    }
}

// ── RPC helpers ───────────────────────────────────────────────────────────

/// Run pathfinding via the worker, awaiting the result.
pub async fn run_pathfind(
    worker_tx: &PathServerWorkerTx,
    request: TraceRequest,
    map_width: isize,
    map_height: isize,
) -> PathfindResult {
    let cancel = Arc::new(AtomicBool::new(false));
    run_pathfind_cancellable(worker_tx, request, map_width, map_height, cancel).await
}

/// Run pathfinding with external cancellation support.
pub async fn run_pathfind_cancellable(
    worker_tx: &PathServerWorkerTx,
    request: TraceRequest,
    map_width: isize,
    map_height: isize,
    cancel: Arc<AtomicBool>,
) -> PathfindResult {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let task = PathfindTask::new(request, map_width, map_height, worker_tx.clone(), cancel, reply_tx);
    task.spawn();
    reply_rx.await.unwrap_or_else(|_| PathfindResult::WorkerGone)
}

// ── Visual pathfinding ────────────────────────────────────────────────────

use super::visual::{self, VisualConfig, VisualMessage, VisualStats};

/// Outcome of a visual pathfinding run: the A* result plus visualisation stats.
pub struct VisualPathfindResult {
    pub pathfind: PathfindResult,
    pub stats: VisualStats,
}

/// Run pathfinding with real-time item visualisation.
///
/// Spawns coloured marker items as the A* search progresses, then cleans up
/// frontier/visited markers and keeps path markers alive.
///
/// The `world` parameter specifies which game world to spawn markers in.
pub async fn run_pathfind_visual(
    worker_tx: &PathServerWorkerTx,
    request: TraceRequest,
    map_width: isize,
    map_height: isize,
    world: u8,
    config: VisualConfig,
) -> VisualPathfindResult {
    let cancel = Arc::new(AtomicBool::new(false));

    // Channel between the A* blocking thread and the async relay.
    // Bounded to avoid unbounded memory growth if A* outpaces the relay.
    let (vis_tx, vis_rx) = std::sync::mpsc::sync_channel::<VisualMessage>(4096);

    let step_delay = config.step_delay;

    // Spawn the async relay that reads events and spawns items.
    let relay_worker_tx = worker_tx.clone();
    let relay_handle = tokio::spawn(async move {
        visual::run_visual_relay(vis_rx, relay_worker_tx, world, config).await
    });

    // Run A* in the standard PathfindTask but with a visual observer.
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let cancel_clone = Arc::clone(&cancel);

    let pf_worker_tx = worker_tx.clone();
    let req = request.clone();

    let left   = req.options.left  .unwrap_or(0)          .max(0) as u16;
    let top    = req.options.top   .unwrap_or(0)          .max(0) as u16;
    let right  = req.options.right .unwrap_or(map_width)  .clamp(0, u16::MAX as isize) as u16;
    let bottom = req.options.bottom.unwrap_or(map_height) .clamp(0, u16::MAX as isize) as u16;

    let handle = Handle::current();
    let sx = req.sx; let sy = req.sy; let sz = req.sz;
    let sdir = req.start_heading();
    let dx = req.dx; let dy = req.dy; let dz = req.dz;
    let ddir = req.dest_heading();
    let opts = req.options.clone();

    let mut search_opts = opts.clone();
    search_opts.left   = Some(search_opts.left  .unwrap_or(0)         .max(left  as isize));
    search_opts.top    = Some(search_opts.top   .unwrap_or(0)         .max(top   as isize));
    search_opts.right  = Some(search_opts.right .unwrap_or(map_width) .min(right as isize + 1));
    search_opts.bottom = Some(search_opts.bottom.unwrap_or(map_height).min(bottom as isize + 1));

    let map_id = request.world;

    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            if cancel_clone.load(Ordering::Relaxed) {
                let _ = vis_tx.send(VisualMessage::Done);
                return PathfindResult::Cancelled;
            }

            let provider = LazyBlockProvider::new(map_id, handle, pf_worker_tx);
            let surveyor = Surveyor::with_options(&provider, &search_opts);
            let mut points = Vec::new();

            let mut observer = visual::make_observer(vis_tx.clone(), step_delay);
            surveyor.trace_a_star(
                sx, sy, sz, sdir,
                dx, dy, dz, ddir,
                &mut points,
                &search_opts,
                map_width,
                map_height,
                &mut observer,
            );

            provider.log_stats();

            // Signal the relay that the search is complete.
            let _ = vis_tx.send(VisualMessage::Done);

            if cancel_clone.load(Ordering::Relaxed) && points.is_empty() {
                return PathfindResult::Cancelled;
            }

            PathfindResult::Found(points)
        })
        .await;

        let outcome = result.unwrap_or_else(|e| {
            warn!("visual pathfind spawn_blocking panicked: {e}");
            PathfindResult::Found(Vec::new())
        });

        let _ = reply_tx.send(outcome);
    });

    // Await both: the pathfind result and the relay cleanup.
    let pathfind = reply_rx.await.unwrap_or_else(|_| PathfindResult::WorkerGone);
    let stats = relay_handle.await.unwrap_or_else(|e| {
        warn!("visual relay task panicked: {e}");
        VisualStats {
            frontier_count: 0,
            visited_count: 0,
            path_count: 0,
            total_spawned: 0,
            total_removed: 0,
            path_serials: Vec::new(),
        }
    });

    VisualPathfindResult { pathfind, stats }
}
