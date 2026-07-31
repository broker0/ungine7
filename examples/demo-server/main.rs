//! Demo server: loads a world from `.uolog`, lets a client connect and walk.
//!
//! - NPC mobiles wander randomly via [`controller_registry::WanderController`]
//! - The player moves via direct `MobileStep` engine commands
//! - Game session logic lives in [`game_session`]
//!
//! At startup the log's init packets are fed through an
//! [`ObserverPipeline`](framework::diorama::ObserverPipeline)
//! to produce a canonical bootstrap packet sequence via
//! [`generate_bootstrap`](framework::diorama::generate_bootstrap).  This ensures the client receives all required
//! packets (SetMap, EnableFeatures, visible entities) in the correct order.

// ── Modules ───────────────────────────────────────────────────────────────

mod actions;
mod buffs;
mod combat;
mod commands;
#[allow(dead_code)]
mod constants;
mod controller_registry;
mod equipment_calc;
mod game_session;
mod game_util;
mod gathering;
mod logout;
mod resource_nodes;
mod crafting;
mod taming;
mod treasure_map;
mod handler;
mod houses;
mod doors;
mod init;
mod ships;
mod planks;
mod loot;
mod magic;
#[cfg(feature = "mirror")]
mod mirror;
mod potions;
mod server;
mod skills;
mod vendor;
mod bank;
mod spawn_points;
mod spawner_object;
mod teleporters;
mod regen;
#[cfg(feature = "lua")]
mod lua_script;

// ── Re-exports (used by nearly every submodule via `use crate::`) ─────────

pub(crate) use commands::{DemoCommand, DemoWorkerTx};
pub(crate) use handler::DemoZone;
pub(crate) use server::WorldData;

// ── Imports for main() ────────────────────────────────────────────────────

use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use log::{error, info, warn};

use log::LevelFilter;

use framework::continuum::{Worker, WorkerCommand};
use framework::continuum::WorldEvent;
use framework::ecumene::StaticDataProvider;
use framework::ecumene::StaticWorldData;

use network::listener::{ListenerConfig, Listener};

use u_core::ProtocolVersion;

use common::args::DataDirArgs;
use common::logging::init_logger;

use handler::DemoHandler;
use server::DemoServer;

// ── Constants ─────────────────────────────────────────────────────────────

const LISTEN_ADDR: &str = "0.0.0.0:2593";
const SERVER_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const SERVER_PORT: u16 = 2593;

/// Additional serials (besides the primary player from the log) that can
/// be selected as playable characters.  Edit this list to add more.
const EXTRA_PLAYABLE_SERIALS: &[u32] = &[
    0x03F8_4C13,
    0x0000_0002,
];

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "demo-server", about = "UO demo server with controllers")]
struct Args {
    /// Paths to .uolog files to load world from.
    ///
    /// Can be specified multiple times to load several logs sequentially
    /// (earlier logs first, later logs overwrite overlapping data).
    ///
    /// If omitted, defaults to `logs/demo.uolog` (unless `--load` is given).
    /// If no files exist the server starts with an empty world (only the
    /// built-in test accounts).  Mutually exclusive with `--load`.
    #[arg(long, conflicts_with = "load")]
    log: Vec<PathBuf>,

    /// Load the world from a JSON snapshot (`world_save.json`) instead of a
    /// `.uolog`.  Restores *all* zones (every world/facet the snapshot
    /// contains) and loads the persisted per-account characters from
    /// `accounts_save.json`.  Mutually exclusive with `--log`.
    #[arg(long, value_name = "PATH", conflicts_with = "log")]
    load: Option<PathBuf>,

    /// Path to a JSON file describing monster spawn points and templates.
    ///
    /// If omitted (or the file does not exist), a built-in default set of
    /// spawn points near Britain is used.  Pass `--no-spawns` to disable
    /// spawn points entirely.
    #[arg(long, value_name = "PATH", default_value = "spawn_points.json")]
    spawn_points: PathBuf,

    /// Disable all monster spawn points.
    #[arg(long, default_value_t = false)]
    no_spawns: bool,

    /// Client version to expect (format: major.minor.patch.build).
    #[arg(long = "client-version", default_value = "3.0.8.0")]
    client_version: ProtocolVersion,

    /// Accept encrypted client connections.
    ///
    /// Use `--encrypted=false` or `--encrypted false` for plain/unencrypted
    /// clients.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set,
          num_args = 0..=1, default_missing_value = "true")]
    encrypted: bool,

    /// Cluster test accounts into a small area around Britain bank.
    ///
    /// The value is the half-size of the spawn area in tiles (e.g.
    /// `--cluster 20` spawns in a 40×40 tile box).  Without this flag
    /// test accounts spawn at random locations across the full map.
    #[arg(long, value_name = "HALF_SIZE")]
    cluster: Option<u16>,

    /// Throttle outbound UpdateMobile (0x77) packets per entity.
    ///
    /// When set, the server will not send more than one position update
    /// per entity per interval (in milliseconds).  Intermediate moves
    /// are dropped — the client sees only the final position after
    /// the throttle window.  Useful for reducing bandwidth in dense
    /// areas at the cost of movement smoothness.
    ///
    /// 0 or omitted = no throttling (every move is sent immediately).
    #[arg(long, value_name = "MS", default_value_t = 0)]
    move_throttle: u64,

    /// Path to a Lua script to run in async mode (requires `lua` feature).
    /// The script is hot-reloaded when the file changes.
    #[cfg(feature = "lua")]
    #[arg(long)]
    lua_script: Option<PathBuf>,

    /// Attach a Lua script as a anima to an entity.
    ///
    /// Format: `SERIAL:PATH` where SERIAL is a hex entity serial (e.g.
    /// `0x00000001`) and PATH is the path to the Lua script file.
    /// Can be specified multiple times.
    ///
    /// Example: `--lua-anima 0x00000001:scripts/wander_ctrl.lua`
    #[cfg(feature = "lua")]
    #[arg(long, value_name = "SERIAL:PATH")]
    lua_controller: Vec<String>,

    /// Path to a Lua session script to auto-load for every player session
    /// (lua session mode only).  The script starts automatically when a
    /// player spawns, no `.slua` command needed.
    ///
    /// Example: `--session-script scripts/session/main.lua`
    #[cfg(feature = "lua")]
    #[arg(long, value_name = "PATH", default_value = "scripts/session/main.lua")]
    session_script: Option<PathBuf>,

    /// Path to a Lua controller script to attach to every player entity
    /// (controller session mode only).  The controller runs inside the
    /// worker tick and receives commands via `poll_command()` / `wait_command()`.
    ///
    /// Example: `--controller-script scripts/controller/main.lua`
    #[cfg(feature = "lua")]
    #[arg(long, value_name = "PATH", default_value = "scripts/controller/main.lua")]
    controller_script: Option<PathBuf>,

    /// Base directory for Lua scripts.
    ///
    /// All script paths (controller scripts, library `require` paths, etc.)
    /// are resolved relative to this directory.  Defaults to `scripts`.
    #[cfg(feature = "lua")]
    #[arg(long, value_name = "DIR", default_value = "scripts")]
    scripts_dir: PathBuf,

    /// Default session mode applied to new client connections:
    /// `rust` (always available), or `lua` / `controller` (require the `lua`
    /// feature).  An administrator can change this at runtime with the
    /// `.session` dot-command.  Defaults to `rust`.
    #[arg(long, value_name = "MODE", default_value = "rust")]
    session_mode: String,

    /// HTTP port for the `/ws/mirror` WebSocket endpoint.
    ///
    /// When provided, an axum HTTP server is started on this port alongside
    /// the UO TCP listener.  The mirror endpoint accepts raw S2C UO packets
    /// (one binary WebSocket frame per packet) and ingests them into the
    /// demo-server's world in real time.
    ///
    /// Requires the `mirror` compile-time feature.
    #[cfg(feature = "mirror")]
    #[arg(long, value_name = "PORT")]
    mirror_port: Option<u16>,

    #[command(flatten)]
    data: DataDirArgs,
}

// ── main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    init_logger()
        .level(LevelFilter::Info)
        .level_for("demo_server", LevelFilter::Debug).level_for("network::listener", LevelFilter::Warn)
        .build()?;

    info!("=== demo-server starting ===");

    // ── Resolve default session mode ────────────────────────────────────
    let default_session_mode: game_session::SessionMode = match args.session_mode.parse() {
        Ok(m) => m,
        Err(e) => {
            warn!("--session-mode: {e}; falling back to `rust`");
            game_session::SessionMode::default()
        }
    };
    info!("Default session mode: {default_session_mode}");

    // ── Load static world data (optional) ──────────────────────────────
    let static_data: Option<Arc<dyn StaticDataProvider>> = match args.data.path() {
        Some(dir) => match StaticWorldData::load(dir) {
            Ok(sd) => {
                info!("World data: loaded from {}", dir.display());
                Some(Arc::new(sd))
            }
            Err(e) => {
                warn!("World data: failed to load from {}: {e}", dir.display());
                warn!("            Movement validation will not work without map data");
                None
            }
        },
        None => {
            info!("World data: disabled (--no-data)");
            None
        }
    };

    // ── Choose world source: JSON snapshot (--load) XOR .uolog (--log) ──
    //
    // `--load` and `--log` are mutually exclusive (enforced by clap).  When
    // `--load` is given the entire world (all zones, every facet) is restored
    // from the JSON snapshot and the persisted per-account characters are
    // loaded.  Otherwise the world is built from `.uolog` files (default:
    // `logs/demo.uolog`) and per-account storage starts empty.
    let snapshot: Option<common::uo_engine::snapshot::WorldSaveData> =
        if let Some(ref load_path) = args.load {
            match common::uo_engine::snapshot::load_from_file(load_path) {
                Ok(data) => {
                    info!(
                        "Loading world from snapshot {}: player={:#010X} world={} zones={}",
                        load_path.display(),
                        data.player_serial,
                        data.player_world,
                        data.zones.len(),
                    );
                    Some(data)
                }
                Err(e) => {
                    error!("Failed to load snapshot {}: {e}", load_path.display());
                    std::process::exit(1);
                }
            }
        } else {
            None
        };

    // Log data is only loaded in `.uolog` mode.  In snapshot mode we still
    // need a (placeholder) value for the few fields that read from it
    // (player_world for controller spawn world), derived from the snapshot.
    let log_data = if snapshot.is_some() {
        init::empty_world()
    } else {
        let log_paths = if args.log.is_empty() {
            vec![std::path::PathBuf::from("logs/demo.uolog")]
        } else {
            args.log.clone()
        };
        init::load_world_from_log_args(&log_paths)
    };

    // Unified player metadata (from whichever source is active).
    let (player_serial, player_world) = match &snapshot {
        Some(s) => (s.player_serial, s.player_world),
        None => (log_data.player_serial, log_data.player_world),
    };

    if player_serial == 0 {
        info!("No player serial — starting with empty world (test accounts only)");
    }

    // ── Build list of playable serials ─────────────────────────────────
    let playable_serials = init::build_playable_serials(
        player_serial,
        EXTRA_PLAYABLE_SERIALS,
    );

    // ── Generate bootstrap via ObserverPipeline ────────────────────────
    let bootstrap_packets = match &snapshot {
        Some(s) => init::generate_bootstrap_from_snapshot(s, &static_data, args.client_version),
        None => init::generate_bootstrap_packets(&log_data, &static_data, args.client_version),
    };

    // ── Create worker ──────────────────────────────────────────────────
    let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(100_000);

    // Create the mpsc event channel externally so the DemoHandler owns
    // the receiver and the worker owns a clone of the sender.
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<WorldEvent>();

    // ── Create centralised serial allocator ────────────────────────────
    let serial_alloc = match &snapshot {
        Some(s) => init::create_serial_allocator_from_snapshot(s, &playable_serials),
        None => init::create_serial_allocator(&log_data, &playable_serials),
    };

    // ── Zone factory + spawn manager ───────────────────────────────────
    let zone_factory = init::make_zone_factory(static_data.clone());
    let spawn_mgr = init::build_spawn_manager(&args.spawn_points, args.no_spawns);

    #[cfg(feature = "lua")]
    let scripts_dir = args.scripts_dir.clone();
    #[cfg(not(feature = "lua"))]
    let scripts_dir = std::path::PathBuf::from("scripts");

    let mut worker = Worker::with_factory_and_sender(
        worker_rx,
        DemoHandler::new(event_rx, serial_alloc.clone(), spawn_mgr, scripts_dir.clone()),
        zone_factory,
        event_tx.clone(),
    );

    // ── Populate zones (from snapshot or log) ──────────────────────────
    //
    // Snapshot mode: zone data is kept for async dispatch via RestoreSnapshot
    // after the worker and restore task are running.  The serial allocator
    // was already pre-seeded by `create_serial_allocator_from_snapshot`.
    //
    // Log mode: zones are populated synchronously here so that the NPC
    // serials are available immediately for `spawn_wander_controllers`.
    let snapshot_zones: Option<Vec<common::uo_engine::snapshot::ZoneSaveData>> =
        snapshot.map(|s| s.zones);

    let log_npc_serials: Vec<u32> = if snapshot_zones.is_none() {
        init::populate_zones(
            &mut worker,
            &log_data,
            &playable_serials,
            &static_data,
            &serial_alloc,
        )
    } else {
        Vec::new()
    };

    // ── Attach wander controllers (log mode only) ──────────────────────
    //
    // In snapshot mode every NPC already has a persisted `controller` meta
    // entry and will be re-attached by the `SnapshotRestored` restore task
    // below.  Attaching wander here would create a duplicate controller.
    if snapshot_zones.is_none() {
        init::spawn_wander_controllers(
            log_npc_serials,
            worker_tx.clone(),
            player_world,
        );
    }

    // Collect Lua anima specs before moving args.
    #[cfg(feature = "lua")]
    let lua_ctrl_specs: Vec<(u32, PathBuf)> = args
        .lua_controller
        .iter()
        .filter_map(|spec| init::parse_lua_controller_spec(spec))
        .collect();

    #[cfg(feature = "lua")]
    init::spawn_lua_controllers(
        lua_ctrl_specs,
        worker_tx.clone(),
        player_world,
        args.scripts_dir.clone(),
    );

    // ── Event broadcast channel ────────────────────────────────────────
    //
    // Created unconditionally so the controller-restore task (below) can
    // receive `SnapshotRestored` events regardless of whether the `lua`
    // feature is enabled.  Lua scripting subscribes to the same channel.
    let event_broadcast_tx = {
        let (tx, _) = tokio::sync::broadcast::channel::<WorldEvent>(65536);
        worker.handler.base.set_lua_forward(tx.clone());
        tx
    };

    // Start the worker.
    tokio::spawn(worker.run());

    // ── Logout reaper task ─────────────────────────────────────────────
    //
    // Manages per-character logout timers.  Sessions arm the timer on
    // disconnect and cancel it on re-login.  When a timer fires the reaper
    // transfers the character into the offline-storage zone (0xFE) via
    // `transfer_entity`, which atomically carries the entity, all containers,
    // item props and AI state.  The storage zone is saved by the ordinary
    // world snapshot.
    let (reaper_tx, reaper_rx) = tokio::sync::mpsc::channel::<logout::ReaperCmd>(256);
    {
        let reaper_worker_tx = worker_tx.clone();
        tokio::spawn(logout::run_logout_reaper(reaper_worker_tx, reaper_rx));
    }

    // ── Controller restore task ────────────────────────────────────────
    //
    // Listens for SnapshotRestored events and re-attaches controllers for
    // all controller types (wander, lua, spawner, teleporter, …).
    // Also handles pending-logout characters: if a save+restart happened
    // while a character's 20-second logout timer was still running, the
    // META_LOGOUT_PENDING flag was saved with the entity.  On restore we
    // arm the reaper with delay=0 so the transfer to the storage zone
    // happens immediately (safe behaviour: log them out right away).
    // Unconditional — works with or without the `lua` feature so that
    // non-Lua controllers (wander, monster, teleporter, …) are always
    // restored after an in-game `.load` or a CLI `--load`.
    {
        let restore_tx = worker_tx.clone();
        let restore_reaper_tx = reaper_tx.clone();
        let mut restore_rx = event_broadcast_tx.subscribe();
        let restore_scripts_dir = scripts_dir.clone();
        tokio::spawn(async move {
            loop {
                match restore_rx.recv().await {
                    Ok(WorldEvent::SnapshotRestored { map_id, controller_metas, logout_pending, player_serials }) => {
                        info!("[restore] restoring {} controllers on map {}", controller_metas.len(), map_id);
                        for (serial, id) in &controller_metas {
                            match controller_registry::create_controller(id, &restore_scripts_dir) {
                                Ok(controller) => {
                                    // Don't persist again — meta is already in the snapshot.
                                    let cmd = WorkerCommand::MapCommand(
                                        map_id,
                                        DemoCommand::AttachController(*serial, controller),
                                    );
                                    if restore_tx.send(cmd).await.is_err() {
                                        warn!("[restore] worker channel closed");
                                        return;
                                    }
                                    info!("[restore] restored {:?} for 0x{:08X}", id, serial);
                                }
                                Err(e) => {
                                    log::error!("[restore] failed to create controller {:?} for 0x{:08X}: {}", id, serial, e);
                                }
                            }
                        }

                        // Re-arm the logout reaper for characters that were
                        // mid-logout when the snapshot was saved.  Use delay=0
                        // so the transfer to the storage zone is immediate.
                        if !logout_pending.is_empty() {
                            info!(
                                "[restore] {} pending-logout character(s) on map {} — transferring to storage immediately",
                                logout_pending.len(), map_id,
                            );
                        }

                        // Crash-recovery: transfer orphaned online players to
                        // storage.  These are player characters that were in a
                        // live-world zone when the snapshot was taken and had no
                        // active session after the server restarted (e.g. the
                        // server crashed or was killed without a pre-restart save).
                        if !player_serials.is_empty() {
                            info!(
                                "[restore] {} crash-recovery orphan(s) on map {} — transferring to storage",
                                player_serials.len(), map_id,
                            );
                        }

                        // Common helper: parse return_addr and arm the reaper.
                        let arm_reaper = |serial: u32, return_addr: &str| -> Option<logout::ReaperCmd> {
                            let parts: Vec<&str> = return_addr.split('|').collect();
                            if parts.len() >= 5 {
                                Some(logout::ReaperCmd::Arm {
                                    serial,
                                    world: parts[0].parse::<u8>().unwrap_or(map_id),
                                    x:     parts[1].parse::<u16>().unwrap_or(0),
                                    y:     parts[2].parse::<u16>().unwrap_or(0),
                                    z:     parts[3].parse::<i8>().unwrap_or(0),
                                    dir:   parts[4].parse::<u8>().unwrap_or(0),
                                    delay: std::time::Duration::ZERO,
                                })
                            } else {
                                warn!(
                                    "[restore] malformed return_addr '{}' for 0x{:08X}; using (0,0,0)",
                                    return_addr, serial,
                                );
                                Some(logout::ReaperCmd::Arm {
                                    serial, world: map_id, x: 0, y: 0, z: 0, dir: 0,
                                    delay: std::time::Duration::ZERO,
                                })
                            }
                        };

                        for (serial, return_addr) in logout_pending.iter().chain(player_serials.iter()) {
                            if let Some(cmd) = arm_reaper(*serial, return_addr) {
                                if restore_reaper_tx.send(cmd).await.is_err() {
                                    warn!("[restore] reaper channel closed, cannot arm 0x{:08X}", serial);
                                } else {
                                    info!("[restore] armed immediate logout for 0x{:08X}", serial);
                                }
                            }
                        }
                    }
                    Ok(_) => {} // ignore other events
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[restore] broadcast lagged, lost {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // ── Restore zones from snapshot (CLI --load) ───────────────────────
    //
    // Sent AFTER the worker is running and the restore task has subscribed
    // to the broadcast channel, so that `SnapshotRestored` events emitted
    // by `RestoreSnapshot` are guaranteed to be heard.
    //
    // `reset_alloc: false` — the allocator was pre-seeded by
    // `create_serial_allocator_from_snapshot`; we must not reset it for
    // each zone or earlier zones' serials would be forgotten.
    if let Some(zones) = snapshot_zones {
        for zone_data in zones {
            let map_id = zone_data.map_id;
            let cmd = WorkerCommand::MapCommand(
                map_id,
                DemoCommand::Engine(common::uo_engine::handler::EngineCommand::RestoreSnapshot {
                    data: zone_data,
                    reset_alloc: false,
                    crash_recovery: true,
                }),
            );
            if worker_tx.send(cmd).await.is_err() {
                error!("[init] worker channel closed while dispatching snapshot zones");
                break;
            }
        }
        info!("[init] snapshot zones dispatched via RestoreSnapshot");
    }

    // ── Lua scripting (optional) ───────────────────────────────────────
    #[cfg(feature = "lua")]
    let lua_cmd_tx = {
        let (lua_cmd_tx, lua_cmd_rx) = tokio::sync::mpsc::channel(1024);
        let initial_script = args.lua_script.clone();
        let lua_worker_tx = worker_tx.clone();
        let lua_event_tx = event_tx.clone();
        let lua_broadcast = event_broadcast_tx.clone();
        let lua_serial_alloc = serial_alloc.clone();
        let lua_scripts_dir = args.scripts_dir.clone();
        tokio::spawn(lua_script::run_lua_manager(
            lua_worker_tx,
            lua_event_tx,
            lua_broadcast,
            lua_cmd_rx,
            0, // map_id for entity cleanup
            initial_script,
            lua_serial_alloc,
            lua_scripts_dir,
        ));
        lua_cmd_tx
    };

    // ── Initialize auth subsystem (moira) ────────────────────────────────
    let account_store = Arc::new(common::uo_engine::auth::MemoryAccountStore::new());
    let authenticator = Arc::new(common::uo_engine::auth::PlainAuthenticator {
        store: account_store.clone(),
        admin_usernames: vec!["admin".to_string()],
    });
    let session_manager = Arc::new(common::uo_engine::auth::SimpleSessionManager::new());
    info!("Auth: moira initialized (auto-create={}, ttl={}s)",
        account_store.auto_create,
        session_manager.ttl.as_secs(),
    );

    // ── Start listener ─────────────────────────────────────────────────
    let server_addr = SocketAddrV4::new(SERVER_IP, SERVER_PORT);
    let config = ListenerConfig::new(LISTEN_ADDR);

    // Load persisted per-account characters (names / appearance / world).
    //
    // Only loaded in snapshot (`--load`) mode, where the character entities
    // are actually restored into their zones — keeping `accounts_save.json`
    // and the live world consistent.  In `.uolog` mode per-account storage
    // starts empty (created characters live only for the session, unless an
    // admin `.save` + later `--load` is used).
    let loaded_accounts = if args.load.is_some() {
        let path = std::path::Path::new(game_util::ACCOUNTS_SAVE_PATH);
        if path.exists() {
            match common::uo_engine::snapshot::load_accounts_from_file(path) {
                Ok(map) => {
                    let chars: usize = map.values().map(|v| v.len()).sum();
                    info!(
                        "Loaded {} character(s) across {} account(s) from {}",
                        chars, map.len(), game_util::ACCOUNTS_SAVE_PATH,
                    );
                    map
                }
                Err(e) => {
                    log::warn!(
                        "Failed to load {}: {e}; starting with empty account store",
                        game_util::ACCOUNTS_SAVE_PATH,
                    );
                    std::collections::HashMap::new()
                }
            }
        } else {
            std::collections::HashMap::new()
        }
    } else {
        std::collections::HashMap::new()
    };

    let handler = DemoServer {
        worker_tx,
        world_data: Arc::new(WorldData {
            player_serial,
            player_world,
            playable_serials,
            entities: log_data.entities,
            bootstrap_packets,
            cluster: args.cluster,
            move_throttle_ms: args.move_throttle,
            #[cfg(feature = "lua")]
            session_script: args.session_script.clone(),
            #[cfg(feature = "lua")]
            controller_script: args.controller_script.clone(),
            #[cfg(feature = "lua")]
            scripts_dir: args.scripts_dir.clone(),
            default_session_mode: std::sync::atomic::AtomicU8::new(default_session_mode.as_u8()),
            test_serials: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            account_characters: tokio::sync::RwLock::new(loaded_accounts),
            reaper_tx,
        }),
        static_data,
        serial_alloc: serial_alloc.clone(),
        login_handler: common::login_handler::LoginHandler {
            server_name: "Demo Server".to_string(),
            server_addr,
            version: args.client_version,
            encrypted: args.encrypted,
            authenticator,
            session_manager,
        },
        #[cfg(feature = "lua")]
        lua_cmd_tx,
    };

    if let Some(half) = args.cluster {
        info!("Cluster spawn: {}×{} tiles around Britain bank", half * 2, half * 2);
    }
    if args.move_throttle > 0 {
        info!("Move throttle: {}ms per entity", args.move_throttle);
    }

    // ── Mirror WebSocket server (optional, feature = "mirror") ─────────
    #[cfg(feature = "mirror")]
    if let Some(port) = args.mirror_port {
        let mirror_worker_tx = handler.worker_tx.clone();
        tokio::spawn(async move {
            let app = mirror::build_mirror_router(mirror_worker_tx);
            let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await {
                Ok(l) => l,
                Err(e) => {
                    log::error!("Failed to bind mirror port {}: {}", port, e);
                    return;
                }
            };
            let local_addr = listener.local_addr().expect("Failed to get mirror ws local address");
            info!("Mirror WS: ws://127.0.0.1:{}/ws/mirror", local_addr.port());
            if let Err(e) = axum::serve(listener, app).await {
                log::error!("Mirror HTTP server error: {}", e);
            }
        });
    }

    info!("Listening on {LISTEN_ADDR}");
    Listener::new(config, handler).run().await?;

    Ok(())
}
