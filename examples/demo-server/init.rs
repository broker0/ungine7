//! Startup initialisation helpers.
//!
//! Functions extracted from `main()` that load the world, generate the
//! bootstrap packet sequence, populate zones and attach controllers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use log::{error, info, warn};

use protocol::RawPacket;

use u_core::ProtocolVersion;

use framework::continuum::{
    EntityStore, HashContainerStore, HashItemProps, Worker, Zone, WorkerCommand, ZoneItemProps,
};
use framework::diorama::{ObserverPipeline, generate_bootstrap};
use framework::ecumene::StaticDataProvider;
use framework::ecumene::Entity;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::item_props::ItemProps;
use common::uo_engine::log_loader::{load_world_from_logs, LogWorldData};
use common::uo_engine::serial_alloc::SerialAllocator;
use common::uo_engine::store::DemoStore;

use crate::commands::{DemoCommand, DemoWorkerTx};
use crate::controller_registry;
use crate::handler::DemoHandler;
use crate::spawn_points;

// ── Helpers ────────────────────────────────────────────────────────────────

/// Return an empty [`LogWorldData`] for starting without a `.uolog` file.
pub(crate) fn empty_world() -> LogWorldData {
    LogWorldData {
        entities: std::collections::HashMap::new(),
        player_serial: 0,
        player_world: 0,
        init_packets: Vec::new(),
        container_packets: Vec::new(),
        item_names: std::collections::HashMap::new(),
    }
}

// ── Lua controller spec parser ────────────────────────────────────────────

/// Parse a `--lua-anima` spec in the form `SERIAL:PATH`.
///
/// SERIAL can be decimal (`12345`) or hex with `0x` prefix (`0x00000001`).
#[cfg(feature = "lua")]
pub(crate) fn parse_lua_controller_spec(spec: &str) -> Option<(u32, PathBuf)> {
    let colon_pos = spec.find(':')?;
    let serial_str = &spec[..colon_pos];
    let path_str = &spec[colon_pos + 1..];

    let serial = if let Some(hex) = serial_str.strip_prefix("0x").or_else(|| serial_str.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        serial_str.parse::<u32>().ok()?
    };

    if path_str.is_empty() {
        warn!("--lua-anima: empty path for serial {:#010X}", serial);
        return None;
    }

    Some((serial, PathBuf::from(path_str)))
}

// ── World loading ─────────────────────────────────────────────────────────

/// Load world data from `.uolog` files.
///
/// Returns [`empty_world()`] if no files exist or loading fails.
pub(crate) fn load_world_from_log_args(log_paths: &[PathBuf]) -> LogWorldData {
    let existing: Vec<&Path> = log_paths.iter()
        .filter(|p| p.exists())
        .map(|p| p.as_path())
        .collect();

    if existing.is_empty() {
        // Check whether the user explicitly specified paths or just
        // got the default.
        let is_default = log_paths.len() == 1
            && log_paths[0].to_str() == Some("logs/demo.uolog");
        if is_default {
            info!("Log file not found (logs/demo.uolog), starting with empty world");
        } else {
            for p in log_paths {
                warn!("Log file not found: {}", p.display());
            }
        }
        return empty_world();
    }

    info!(
        "Loading world from {} log file(s): {}",
        existing.len(),
        existing.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
    );
    match load_world_from_logs(&existing) {
        Ok(data) => {
            info!(
                "Loaded: player={:#010X} world={} entities={} item_names={}",
                data.player_serial,
                data.player_world,
                data.entities.values().map(|m| m.len()).sum::<usize>(),
                data.item_names.len(),
            );
            data
        }
        Err(e) => {
            warn!("Failed to load logs: {e}");
            warn!("Starting with empty world (test accounts only)");
            empty_world()
        }
    }
}

// ── Playable serials ──────────────────────────────────────────────────────

/// Build the list of playable character serials from the log player and
/// the extra-playable table.
pub(crate) fn build_playable_serials(
    player_serial: u32,
    extra: &[u32],
) -> Vec<u32> {
    let mut playable: Vec<u32> = if player_serial != 0 {
        vec![player_serial]
    } else {
        Vec::new()
    };
    for &serial in extra {
        if serial != player_serial && playable.len() < 5 {
            playable.push(serial);
        }
    }
    if playable.is_empty() {
        // Fallback: at least one slot so the client gets a character list.
        playable.push(0x0000_0001);
    }
    info!(
        "Playable characters: {} serials ({:#010X}, ...)",
        playable.len(),
        playable[0],
    );
    playable
}

// ── Bootstrap generation ──────────────────────────────────────────────────

/// Generate the bootstrap packet sequence via [`ObserverPipeline`].
///
/// Feeds the log's init packets and nearby entities through the pipeline,
/// then produces the canonical S→C packet sequence a client needs to enter
/// the world.
pub(crate) fn generate_bootstrap_packets(
    log_data: &LogWorldData,
    static_data: &Option<Arc<dyn StaticDataProvider>>,
    version: ProtocolVersion,
) -> Vec<RawPacket> {
    let mut observer = ObserverPipeline::new(static_data.clone());
    for pkt_data in &log_data.init_packets {
        observer.ingest_s2c(pkt_data);
    }

    // Player position is now known from 0x1B in init_packets.
    let px = observer.pos.x;
    let py = observer.pos.y;
    let view_range = 18u16;

    // Feed only entities within view range so the bootstrap does not
    // flood the client with the entire world.
    if let Some(world_entities) = log_data.entities.get(&log_data.player_world) {
        let mut in_range = 0u32;
        let mut out_range = 0u32;
        for entity in world_entities.values() {
            let epos = entity.pos();
            let dx = (epos.x as i32 - px as i32).unsigned_abs() as u16;
            let dy = (epos.y as i32 - py as i32).unsigned_abs() as u16;
            if dx <= view_range && dy <= view_range {
                let raw = entity.to_raw_bytes();
                observer.ingest_s2c(&raw);
                in_range += 1;
            } else {
                out_range += 1;
            }
        }
        info!(
            "Bootstrap entities: {} in view range, {} outside (range={})",
            in_range, out_range, view_range
        );
    }

    let pkts = generate_bootstrap(
        &observer,
        static_data.as_deref(),
        version,
    );
    info!(
        "Bootstrap: {} packets (pos: ({},{},{}) serial={:#010X})",
        pkts.len(),
        observer.pos.x, observer.pos.y, observer.pos.z,
        observer.pos.serial,
    );
    pkts
}

// ── Serial allocator ──────────────────────────────────────────────────────

/// Create and populate the serial allocator, marking all loaded entity
/// serials (including equipment) and playable serials as occupied.
pub(crate) fn create_serial_allocator(
    log_data: &LogWorldData,
    playable_serials: &[u32],
) -> Arc<SerialAllocator> {
    let alloc = Arc::new(SerialAllocator::new());
    for world_entities in log_data.entities.values() {
        for entity in world_entities.values() {
            alloc.mark_occupied(entity.serial());
            // Mark equipped item serials.
            if let Some(m) = entity.mobile() {
                for eq in &m.items {
                    alloc.mark_occupied(eq.serial);
                }
            }
        }
    }
    // Mark playable serials (they may not exist in the log yet).
    for &s in playable_serials {
        alloc.mark_occupied(s);
    }
    info!(
        "Serial allocator: {} free mobiles, {} free items",
        alloc.free_mobiles(),
        alloc.free_items(),
    );
    alloc
}

// ── Snapshot loading (JSON world save) ─────────────────────────────────────

/// Create and populate the serial allocator from a JSON world snapshot,
/// marking all entity serials (including equipment) and playable serials.
pub(crate) fn create_serial_allocator_from_snapshot(
    world_save: &common::uo_engine::snapshot::WorldSaveData,
    playable_serials: &[u32],
) -> Arc<SerialAllocator> {
    let alloc = Arc::new(SerialAllocator::new());
    for zone in &world_save.zones {
        for (serial, entity) in &zone.entities {
            alloc.mark_occupied(*serial);
            if let Some(m) = entity.mobile() {
                for eq in &m.items {
                    alloc.mark_occupied(eq.serial);
                }
            }
        }
        // Container item serials.
        for container in zone.containers.values() {
            for item in &container.items {
                alloc.mark_occupied(item.serial);
            }
        }
    }
    for &s in playable_serials {
        alloc.mark_occupied(s);
    }
    info!(
        "Serial allocator (snapshot): {} free mobiles, {} free items",
        alloc.free_mobiles(),
        alloc.free_items(),
    );
    alloc
}

/// Generate the bootstrap packet sequence from a JSON world snapshot.
///
/// The snapshot has no cached raw `EnableFeatures` / `EnableMapDiff` packets
/// (those come from a live log), so only the structural essentials are
/// produced (0x1B locale/body, 0xBF SetMap, view range).  The client still
/// works: `enter_world` re-sends `EnableFeaturesLegacy` and the real entity
/// position at spawn time.
pub(crate) fn generate_bootstrap_from_snapshot(
    world_save: &common::uo_engine::snapshot::WorldSaveData,
    static_data: &Option<Arc<dyn StaticDataProvider>>,
    version: ProtocolVersion,
) -> Vec<RawPacket> {
    let mut observer = ObserverPipeline::new(static_data.clone());

    // Find the player entity in its world to seed position/body.
    let player_serial = world_save.player_serial;
    let player_world = world_save.player_world;
    let player_mobile = world_save
        .zones
        .iter()
        .find(|z| z.map_id == player_world)
        .and_then(|z| {
            z.entities
                .iter()
                .find(|(s, _)| *s == player_serial)
                .and_then(|(_, e)| e.mobile())
        });

    if let Some(m) = player_mobile {
        // Seed the observer with a synthetic 0x1B at the player's position so
        // `generate_bootstrap` emits a correct CharacterLocaleAndBody/SetMap.
        let clb = common::spawn::build_character_locale_and_body(
            m.serial, m.graphic, m.x, m.y, m.z, m.direction, 0x1800, 0x1000,
        );
        observer.ingest_s2c(&clb.data);
        // SetMap so the observer's current_world matches the player's facet.
        {
            use packets::system::GeneralInfo;
            use packets::traits::ManualPacket;
            let set_map = GeneralInfo::SetMap { world: player_world };
            observer.ingest_s2c(&set_map.to_bytes());
        }
    } else {
        warn!(
            "Snapshot bootstrap: player {:#010X} not found in world {}; using defaults",
            player_serial, player_world,
        );
    }

    let pkts = generate_bootstrap(&observer, static_data.as_deref(), version);
    info!(
        "Bootstrap (snapshot): {} packets (pos: ({},{},{}) serial={:#010X})",
        pkts.len(),
        observer.pos.x, observer.pos.y, observer.pos.z,
        observer.pos.serial,
    );
    pkts
}

// ── Zone factory ──────────────────────────────────────────────────────────
/// Create the zone factory closure used by the worker.
pub(crate) fn make_zone_factory(
    static_data: Option<Arc<dyn StaticDataProvider>>,
) -> framework::continuum::worker::ZoneFactory<DemoEntity, HashContainerStore, HashItemProps<ItemProps>> {
    Box::new(move |map_id: u8| {
        Zone::<DemoEntity, HashContainerStore, HashItemProps<ItemProps>>::new(
            map_id,
            static_data.clone(),
            Box::new(DemoStore::new()),
            896,
            512,
        )
    })
}

// ── Spawn-point manager ───────────────────────────────────────────────────

/// Build the spawn-point manager from CLI args.
pub(crate) fn build_spawn_manager(
    spawn_path: &Path,
    no_spawns: bool,
) -> spawn_points::SpawnManager {
    if no_spawns {
        info!("Spawn points: disabled (--no-spawns)");
        return spawn_points::SpawnManager::empty();
    }
    if spawn_path.exists() {
        match spawn_points::load_config(spawn_path) {
            Ok(cfg) => {
                info!(
                    "Spawn points: loaded {} point(s), {} template(s) from {}",
                    cfg.points.len(), cfg.templates.len(), spawn_path.display(),
                );
                spawn_points::SpawnManager::new(cfg)
            }
            Err(e) => {
                warn!("Spawn points: failed to load {}: {e}", spawn_path.display());
                warn!("              falling back to built-in defaults");
                spawn_points::SpawnManager::new(spawn_points::default_config())
            }
        }
    } else {
        info!("Spawn points: using built-in defaults (no {} file)", spawn_path.display());
        spawn_points::SpawnManager::new(spawn_points::default_config())
    }
}

// ── Zone population ───────────────────────────────────────────────────────

/// Populate worker zones from loaded log data.
///
/// Returns the list of NPC mobile serials (for controller attachment).
pub(crate) fn populate_zones(
    worker: &mut Worker<DemoEntity, HashContainerStore, DemoHandler, HashItemProps<ItemProps>>,
    log_data: &LogWorldData,
    playable_serials: &[u32],
    static_data: &Option<Arc<dyn StaticDataProvider>>,
    serial_alloc: &SerialAllocator,
) -> Vec<u32> {
    // Populate zones with entities from the log.
    let mut npc_serials: Vec<u32> = Vec::new();
    for (&world_id, world_entities) in &log_data.entities {
        let sd = static_data.clone();
        let mut zone = Zone::<DemoEntity, HashContainerStore, HashItemProps<ItemProps>>::new(
            world_id,
            sd,
            Box::new(DemoStore::new()),
            896,
            512,
        );
        for entity in world_entities.values() {
            let is_playable = playable_serials.contains(&entity.serial());
            if entity.is_mobile() && !is_playable {
                npc_serials.push(entity.serial());
            }
            // Entities loaded from .uolog default to `is_player: false` (see
            // ingest.rs).  Promote playable characters so the engine routes
            // their death through `handle_kill_player` (ghost) instead of
            // `handle_kill_mobile` (NPC corpse + full loot).
            let mut entity = entity.clone();
            if is_playable {
                if let Some(m) = entity.mobile_mut() {
                    m.is_player = true;
                }
            }
            zone.spawn(entity.serial(), entity);
        }
        worker.zones.insert(world_id, zone);
    }

    // Build the equipment reverse index from all zones populated above.
    // Entities loaded from .uolog are spawned directly via zone.spawn(),
    // bypassing EngineHandler, so their equipment is not indexed yet.
    for zone in worker.zones.values() {
        worker.handler.index_zone_equipment(zone);
    }

    info!("Populated {} zones", worker.zones.len());

    // Ingest container packets (0x24, 0x25, 0x3C) into zone container stores.
    // Also mark container item serials as occupied in the allocator.
    {
        use packets::interaction::{AddItemToContainer, ContainerContent};
        use packets::traits::ManualPacket;

        let mut ingested = 0usize;
        for (world_id, pkt_data) in &log_data.container_packets {
            if pkt_data.is_empty() { continue; }
            if let Some(zone) = worker.zones.get_mut(world_id) {
                let container_serial = match pkt_data[0] {
                    0x24 => {
                        if pkt_data.len() < 7 { continue; }
                        let serial = u32::from_be_bytes([pkt_data[1], pkt_data[2], pkt_data[3], pkt_data[4]]);
                        let gump_model = u16::from_be_bytes([pkt_data[5], pkt_data[6]]);
                        zone.containers.ingest_open(serial, gump_model);
                        // Mark entity as container in the entity store.
                        if let Some(DemoEntity::Item { is_container, .. }) =
                            zone.store.get_mut(serial)
                        {
                            *is_container = true;
                        }
                        serial
                    }
                    0x25 => {
                        let Ok(add) = AddItemToContainer::from_bytes(pkt_data) else { continue };
                        let cs = add.container_serial();
                        let item = framework::diorama::container_item_from_add(&add);
                        serial_alloc.mark_occupied(item.serial);
                        zone.containers.ingest_item_upsert(cs, item);
                        cs
                    }
                    0x3C => {
                        let Ok(cc) = ContainerContent::from_bytes(pkt_data) else { continue };
                        let Some(cs) = cc.container_serial() else { continue };
                        let items = framework::diorama::container_items_from_content(&cc);
                        for item in &items {
                            serial_alloc.mark_occupied(item.serial);
                        }
                        zone.containers.ingest_content(cs, items);
                        cs
                    }
                    _ => continue,
                };
                ingested += 1;
                let _ = container_serial;
            }
        }
        if ingested > 0 {
            info!("Ingested {} container packets", ingested);
        }
    }

    // Ingest item names extracted from SendSpeech (0x1C) into ItemProps.
    {
        let mut stored = 0usize;
        for (&serial, name) in &log_data.item_names {
            for zone in worker.zones.values_mut() {
                if zone.store.get(serial).is_some() {
                    zone.item_props.insert(serial, ItemProps::with_name(name));
                    stored += 1;
                }
            }
        }
        if stored > 0 {
            info!("Stored {} item names from log", stored);
        }
    }

    npc_serials
}

// ── Controller attachment ─────────────────────────────────────────────────

/// Spawn a background task that attaches WanderControllers to NPC mobiles.
pub(crate) fn spawn_wander_controllers(
    npc_serials: Vec<u32>,
    worker_tx: DemoWorkerTx,
    world_id: u8,
) {
    tokio::spawn(async move {
        // Give the worker a moment to start.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut attached = 0;
        for serial in &npc_serials {
            let controller = Box::new(controller_registry::WanderController::new(Duration::from_secs(3)));
            let _ = worker_tx.send(WorkerCommand::MapCommand(
                world_id,
                DemoCommand::AttachControllerPersist {
                    serial: *serial,
                    controller,
                    controller_id: "wander:3".to_string(),
                },
            )).await;
            attached += 1;
        }
        info!("[demo] attached WanderController to {} NPCs", attached);
    });
}

/// Spawn a background task that attaches Lua controllers specified via
/// `--lua-anima`.
#[cfg(feature = "lua")]
pub(crate) fn spawn_lua_controllers(
    specs: Vec<(u32, PathBuf)>,
    worker_tx: DemoWorkerTx,
    world_id: u8,
    scripts_dir: PathBuf,
) {
    if specs.is_empty() {
        return;
    }
    tokio::spawn(async move {
        // Give the worker a moment to start.
        tokio::time::sleep(Duration::from_millis(100)).await;

        for (serial, path) in &specs {
            match crate::lua_script::LuaController::from_file(path, Some(&scripts_dir)) {
                Ok(controller) => {
                    let _ = worker_tx.send(WorkerCommand::MapCommand(
                        world_id,
                        DemoCommand::AttachController(*serial, Box::new(controller)),
                    )).await;
                    info!(
                        "[demo] attached LuaController to {:#010X} ({})",
                        serial,
                        path.display()
                    );
                }
                Err(e) => {
                    error!(
                        "[demo] failed to load Lua anima for {:#010X} from {}: {}",
                        serial,
                        path.display(),
                        e
                    );
                }
            }
        }
    });
}
