//! [`LazyBlockProvider`] — on-demand collision block fetcher with optional prefetch.
//!
//! On a cache miss the provider fetches a square of blocks centered on the
//! requested block.  The half-size of that square is controlled by the
//! compile-time constant [`PREFETCH_RADIUS`]:
//!
//! * `0` — fetch exactly the one missing block (pure lazy, one round-trip per block)
//! * `N` — fetch a `(2N+1)×(2N+1)` square, amortising round-trip overhead
//!   at the cost of loading some blocks the A* frontier may never reach
//!
//! `CachingProvider` inside `Surveyor` ensures each block is requested at
//! most once per search regardless of how many times the frontier revisits
//! tiles within it.

use std::cell::RefCell;
use std::collections::HashMap;

use log::{debug, trace};
use tokio::runtime::Handle;

use u_core::{BlockKey, Heading};

use framework::continuum::WorkerCommand;
use framework::ecumene::{TileBlock, TileProvider};
use framework::vessel::tile_shape::TileShape;

use common::uo_engine::handler::EngineCommand;

use crate::worker::{PathServerCommand, PathServerWorkerTx};

// ── Prefetch radius ───────────────────────────────────────────────────────

/// Half-size of the square fetch window (in blocks).
///
/// On a cache miss for block `(bx, by)` the provider requests all blocks in
/// the range `[bx-R .. bx+R] × [by-R .. by+R]` in a single round-trip.
///
/// | `R` | blocks per fetch | tile area  |
/// |-----|-----------------|------------|
/// |  0  |        1        |   8×8      |
/// |  2  |       25        |  40×40     |
/// |  4  |       81        |  72×72     |
/// |  8  |      289        | 136×136    |
const PREFETCH_RADIUS: u16 = 4;

// ── LazyBlockCache ────────────────────────────────────────────────────────

/// Inner mutable state.  Wrapped in `RefCell` so the `TileProvider` trait
/// (`&self`) can populate the cache.  Single-threaded (inside `spawn_blocking`).
struct LazyBlockCache {
    cache: HashMap<BlockKey, TileBlock>,
    map_id: u8,
    handle: Handle,
    worker_tx: PathServerWorkerTx,
    /// Total round-trips sent to the worker.
    fetches: u32,
    /// Total blocks received (≥ fetches when PREFETCH_RADIUS > 0).
    blocks_loaded: u32,
}

impl LazyBlockCache {
    /// Fetch the block at `key` plus a surrounding square from the worker.
    ///
    /// When `PREFETCH_RADIUS == 0` uses `GetCollisionBlock` (single block).
    /// When `PREFETCH_RADIUS > 0` uses `GetCollisionBlocks` batch covering
    /// `(2R+1)×(2R+1)` blocks.
    ///
    /// Calls `Handle::block_on` — safe inside `spawn_blocking`.
    fn fetch(&mut self, key: BlockKey) {
        let map_id    = self.map_id;
        let worker_tx = self.worker_tx.clone();

        let blocks: Vec<TileBlock> = if PREFETCH_RADIUS == 0 {
            self.handle.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel::<TileBlock>();
                let ok = worker_tx
                    .send(WorkerCommand::MapCommand(
                        map_id,
                        PathServerCommand::Engine(EngineCommand::GetCollisionBlock {
                            block: key,
                            reply: tx,
                        }),
                    ))
                    .await
                    .is_ok();
                if ok { rx.await.ok().into_iter().collect() } else { Vec::new() }
            })
        } else {
            let bx_min = key.bx.saturating_sub(PREFETCH_RADIUS);
            let by_min = key.by.saturating_sub(PREFETCH_RADIUS);
            let bx_max = key.bx.saturating_add(PREFETCH_RADIUS);
            let by_max = key.by.saturating_add(PREFETCH_RADIUS);

            self.handle.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel::<Vec<TileBlock>>();
                let ok = worker_tx
                    .send(WorkerCommand::MapCommand(
                        map_id,
                        PathServerCommand::Engine(EngineCommand::GetCollisionBlocks {
                            tile_left:   bx_min * 8,
                            tile_top:    by_min * 8,
                            tile_right:  bx_max * 8 + 7,
                            tile_bottom: by_max * 8 + 7,
                            reply: tx,
                        }),
                    ))
                    .await
                    .is_ok();
                if ok { rx.await.unwrap_or_default() } else { Vec::new() }
            })
        };

        let loaded = blocks.len();
        for block in blocks {
            self.cache.entry(block.block_key).or_insert(block);
        }
        self.fetches += 1;
        self.blocks_loaded += loaded as u32;

        trace!(
            "lazy fetch #{} (R={PREFETCH_RADIUS}) at block ({},{}): \
             {loaded} blocks (cache={})",
            self.fetches, key.bx, key.by, self.cache.len(),
        );
    }

    fn get_or_fetch(&mut self, key: BlockKey) -> TileBlock {
        if !self.cache.contains_key(&key) {
            self.fetch(key);
        }
        self.cache.get(&key).cloned().unwrap_or_else(|| TileBlock::empty(key))
    }
}

// ── LazyBlockProvider ─────────────────────────────────────────────────────

/// [`TileProvider`] that fetches collision blocks on demand from the worker.
///
/// Wraps [`LazyBlockCache`] in a `RefCell` so the immutable `TileProvider`
/// interface can mutate the internal cache.
///
/// **Not `Sync`** — single-threaded use only (inside `spawn_blocking`).
pub struct LazyBlockProvider {
    inner: RefCell<LazyBlockCache>,
}

impl LazyBlockProvider {
    pub fn new(
        map_id: u8,
        handle: Handle,
        worker_tx: PathServerWorkerTx,
    ) -> Self {
        Self {
            inner: RefCell::new(LazyBlockCache {
                cache: HashMap::new(),
                map_id,
                handle,
                worker_tx,
                fetches: 0,
                blocks_loaded: 0,
            }),
        }
    }

    pub fn log_stats(&self) {
        let c = self.inner.borrow();
        debug!(
            "LazyBlockProvider: {} fetches, {} blocks loaded, {} cached",
            c.fetches, c.blocks_loaded, c.cache.len()
        );
    }
}

impl TileProvider for LazyBlockProvider {
    fn query_tile_stack(&self, x: u16, y: u16, _direction: Heading) -> Vec<TileShape> {
        let key = BlockKey::from_tile(x, y);
        let block = self.inner.borrow_mut().get_or_fetch(key);
        let ox = (x % 8) as u8;
        let oy = (y % 8) as u8;
        block.tile_stack(ox, oy).to_vec()
    }

    fn query_block(&self, key: BlockKey) -> TileBlock {
        self.inner.borrow_mut().get_or_fetch(key)
    }
}
