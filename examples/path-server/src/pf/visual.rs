//! Real-time A* visualisation via spawned marker items.
//!
//! Spawns coloured gem items into the game world as A* progresses, so that
//! connected UO clients can see frontier, visited, and final-path tiles in
//! real time.  Items use a non-blocking graphic (no `IMPASSABLE` flag) so
//! they do not interfere with movement or the search itself.
//!
//! # Architecture
//!
//! ```text
//! spawn_blocking (A* thread)
//!   │  Surveyor callback → SyncSender<AStarEvent>
//!   │  optional thread::sleep for throttling
//!   │
//!   ▼
//! async relay task (tokio::spawn)
//!   │  drains receiver on a timer interval
//!   │  SpawnEntity / RemoveEntity via worker_tx
//!   │  observer pipeline delivers to UO clients
//!   ▼
//! UO client sees markers appear in real time
//! ```

use std::sync::mpsc;
use std::time::Duration;

use log::{debug, info};

use framework::ecumene::pathfinding::{AStarAction, AStarEvent};

use crate::worker::PathServerWorkerTx;

use super::marker::{self, build_marker, SerialRange};

// ── Configuration ─────────────────────────────────────────────────────────

/// Visual marker configuration.
#[derive(Debug, Clone)]
pub struct VisualConfig {
    /// Item graphic for markers (should be non-blocking in tiledata).
    /// Default: `0x0E73` (small gem).
    pub graphic: u16,
    /// UO hue for frontier (open set) markers. Default: blue-ish `0x0059`.
    pub hue_frontier: u16,
    /// UO hue for visited (closed set) markers. Default: green-ish `0x0043`.
    pub hue_visited: u16,
    /// UO hue for final path markers. Default: red `0x0026`.
    pub hue_path: u16,
    /// Interval between relay batches (how often markers are flushed to the
    /// worker). Default: 50 ms.
    pub batch_interval: Duration,
    /// Optional sleep injected into the A* callback after every event.
    /// Slows down the search to make visualisation watchable.
    /// Default: 200 us.
    pub step_delay: Duration,
}

impl Default for VisualConfig {
    fn default() -> Self {
        Self {
            graphic:        0x0E73,
            hue_frontier:   0x0059,
            hue_visited:    0x0043,
            hue_path:       0x0026,
            batch_interval: Duration::from_millis(50),
            step_delay:     Duration::from_micros(200),
        }
    }
}

// ── Serial allocator ──────────────────────────────────────────────────────

/// Marker serials live in the range `0x7000_0000 ..= 0x7FFF_FFFE`.
/// This avoids collisions with player serials (`0x0000_0001..0x3FFF_FFFF`),
/// mount serials (`0x4000_0000..`), and item serials from normal gameplay.
static MARKER_SERIAL: SerialRange = SerialRange::new(0x7000_0000, 0x7FFF_FFFE);

// ── Relay: event → enum for channel ───────────────────────────────────────

/// Internal message sent from the A* callback to the async relay task.
pub(crate) enum VisualMessage {
    Event(AStarEvent),
    /// Signals that the A* search has completed.
    Done,
}

// ── Observer callback factory ─────────────────────────────────────────────

/// Build the closure that `Surveyor::trace_a_star` calls on every event.
///
/// The closure sends events to the relay via a sync channel and optionally
/// sleeps `step_delay` after each event to slow down the search.
pub(crate) fn make_observer(
    tx: mpsc::SyncSender<VisualMessage>,
    step_delay: Duration,
) -> impl FnMut(AStarEvent) -> AStarAction {
    move |event| {
        if tx.send(VisualMessage::Event(event)).is_err() {
            // Relay dropped — cancel the search.
            return AStarAction::Cancel;
        }
        if !step_delay.is_zero() {
            std::thread::sleep(step_delay);
        }
        AStarAction::Continue
    }
}

// ── Async relay task ──────────────────────────────────────────────────────

/// Drains `VisualMessage` events and spawns/removes marker items via the
/// worker.
///
/// Runs until the sender is dropped (A* thread finishes).
/// After processing all events, performs cleanup — removes all spawned
/// marker items except the final path markers (kept for review).
pub(crate) async fn run_visual_relay(
    rx: mpsc::Receiver<VisualMessage>,
    worker_tx: PathServerWorkerTx,
    world: u8,
    config: VisualConfig,
) -> VisualStats {
    let mut frontier_serials: Vec<u32> = Vec::new();
    let mut visited_serials: Vec<u32> = Vec::new();
    let mut path_serials: Vec<u32> = Vec::new();
    let mut total_spawned: u32 = 0;
    let total_removed: u32;

    let mut interval = tokio::time::interval(config.batch_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut done = false;

    while !done {
        // Wait for next batch interval tick.
        interval.tick().await;

        // Drain all pending events.
        loop {
            match rx.try_recv() {
                Ok(VisualMessage::Event(event)) => {
                    let (serial, entity) = match &event {
                        AStarEvent::Frontier { x, y, z, .. } => {
                            let s = MARKER_SERIAL.alloc();
                            frontier_serials.push(s);
                            (s, build_marker(s, config.graphic, *x as u16, *y as u16, *z, config.hue_frontier))
                        }
                        AStarEvent::Visited { x, y, z, .. } => {
                            let s = MARKER_SERIAL.alloc();
                            visited_serials.push(s);
                            (s, build_marker(s, config.graphic, *x as u16, *y as u16, *z, config.hue_visited))
                        }
                        AStarEvent::Path { x, y, z } => {
                            let s = MARKER_SERIAL.alloc();
                            path_serials.push(s);
                            (s, build_marker(s, config.graphic, *x as u16, *y as u16, *z, config.hue_path))
                        }
                    };

                    marker::spawn_marker(&worker_tx, world, serial, entity).await;
                    total_spawned += 1;
                }
                Ok(VisualMessage::Done) => {
                    done = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
    }

    info!(
        "[visual] A* done: {} frontier, {} visited, {} path markers spawned",
        frontier_serials.len(),
        visited_serials.len(),
        path_serials.len(),
    );

    // ── Cleanup: remove frontier and visited markers, keep path ────────
    // Small delay so the client can see the final state before cleanup.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let all_remove: Vec<u32> = frontier_serials.iter().chain(visited_serials.iter()).copied().collect();
    total_removed = all_remove.len() as u32;

    marker::remove_markers_batch(all_remove, &worker_tx, world, "visual").await;

    debug!(
        "[visual] cleanup: removed {} markers, kept {} path markers",
        total_removed,
        path_serials.len(),
    );

    VisualStats {
        frontier_count: frontier_serials.len() as u32,
        visited_count: visited_serials.len() as u32,
        path_count: path_serials.len() as u32,
        total_spawned,
        total_removed,
        path_serials,
    }
}

/// Statistics returned after a visual pathfinding run.
#[derive(Debug)]
pub struct VisualStats {
    pub frontier_count: u32,
    pub visited_count: u32,
    pub path_count: u32,
    pub total_spawned: u32,
    pub total_removed: u32,
    /// Serials of path markers still alive (caller can clean up later).
    pub path_serials: Vec<u32>,
}

// ── Cleanup helper ────────────────────────────────────────────────────────

/// Remove leftover path markers (called when the user runs `.pathvis clear`
/// or starts a new visual search).
pub async fn cleanup_markers(
    serials: &[u32],
    worker_tx: &PathServerWorkerTx,
    world: u8,
) {
    marker::remove_markers_batch(serials.to_vec(), worker_tx, world, "visual").await;
}
