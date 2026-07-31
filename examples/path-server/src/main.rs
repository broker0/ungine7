//! Path Server — Web API for pathfinding + UO TCP server.
//!
//! Runs two services in parallel:
//!   - HTTP on `--web-port` (default 8080) — JSON/PNG pathfinding API
//!   - WS   on `--web-port` `/ws/mirror`   — raw S2C UO packet mirror ingest
//!   - TCP  on `--uo-port`  (default 2593) — UO client protocol

use std::net::Ipv4Addr;
use std::sync::Arc;
use clap::Parser;
use log::info;

mod config;
mod state;
mod handler;
mod pf;
mod render;
mod worker;
mod server;
mod doors;

use config::Args;
use state::AppState;

use framework::continuum::{EntityStore, HashContainerStore, Worker, Zone, WorldEvent};
use common::uo_engine::store::DemoStore;
use common::uo_engine::serial_alloc::SerialAllocator;
use worker::PathServerHandler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    args.verbosity.logger(["path_server", "common"]).build()?;

    info!("=== path-server starting ===");
    info!("Web port:  {}", args.web_port);
    info!("UO port:   {}", args.uo_port);
    info!("Data dir:  {:?}", args.data_path());

    // ── Create worker channels ─────────────────────────────────────────
    let (worker_tx, worker_rx) =
        tokio::sync::mpsc::channel::<framework::continuum::WorkerCommand<
            common::uo_engine::entity::DemoEntity,
            worker::PathServerCommand,
        >>(10_000);

    let (event_tx, event_rx) =
        tokio::sync::mpsc::unbounded_channel::<WorldEvent>();
    let event_rx_for_handler = event_rx;

    // ── Centralised serial allocator ───────────────────────────────────
    //
    // One allocator shared by the worker handler and the session layer so
    // player/item/mount serials never collide.
    let serial_alloc = Arc::new(SerialAllocator::new());

    // ── Build AppState ─────────────────────────────────────────────────
    let state = Arc::new(
        AppState::new(&args, worker_tx.clone(), event_tx.clone(), serial_alloc.clone()).await?,
    );

    // ── Build Worker ──────────────────────────────────────────────────
    //
    // The single set of worker zones handles everything: UO client movement,
    // HTTP API (ItemsAdd/Del/Query/Save/Load), and pathfinding.
    let sd_for_factory = state.static_data.0.clone();
    let zone_factory: framework::continuum::worker::ZoneFactory<
        common::uo_engine::entity::DemoEntity,
        HashContainerStore,
    > = Box::new(move |map_id: u8| {
        Zone::<common::uo_engine::entity::DemoEntity, HashContainerStore>::new(
            map_id,
            sd_for_factory.clone().map(|sd| {
                sd as Arc<dyn framework::vessel::traits::StaticDataProvider>
            }),
            Box::new(DemoStore::new()),
            896,
            512,
        )
    });

    let worker = Worker::with_factory_and_sender(
        worker_rx,
        PathServerHandler::new(event_rx_for_handler, serial_alloc.clone()),
        zone_factory,
        event_tx.clone(),
    );

    tokio::spawn(worker.run());
    info!("Worker started");

    // ── HTTP server ────────────────────────────────────────────────────
    let http_state = state.clone();
    let web_port = args.web_port;
    let http_task = tokio::spawn(async move {
        let app = handler::build_router(http_state);
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", web_port))
            .await
            .unwrap_or_else(|e| panic!("Failed to bind HTTP port {web_port}: {e}"));
        let local_addr = listener.local_addr()
            .expect("Failed to get local address");
        info!("HTTP listening on http://127.0.0.1:{}", local_addr.port());
        info!("UI: http://127.0.0.1:{}/ui.html", local_addr.port());
        info!("Mirror WS: ws://127.0.0.1:{}/ws/mirror", local_addr.port());
        axum::serve(listener, app).await.expect("HTTP server error");
    });

    // ── UO TCP server ──────────────────────────────────────────────────
    let uo_state = state.clone();
    let uo_port = args.uo_port;
    let uo_listen = format!("0.0.0.0:{}", uo_port);
    let uo_task = tokio::spawn(async move {
        if let Err(e) = server::run_uo_listener(
            uo_state,
            &uo_listen,
            Ipv4Addr::new(127, 0, 0, 1),
            uo_port,
        )
        .await
        {
            log::error!("UO listener error: {e}");
        }
    });

    info!("Path-server ready");

    tokio::select! {
        _ = http_task => { log::error!("HTTP task exited unexpectedly"); }
        _ = uo_task   => { log::error!("UO task exited unexpectedly"); }
    }

    Ok(())
}
