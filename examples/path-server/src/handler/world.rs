use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::EngineCommand;
use common::uo_engine::snapshot;
use common::uo_engine::rpc::EngineProxy;
use framework::continuum::WorkerCommand;
use framework::vessel::traits::StaticDataProvider;

use crate::pf::TraceRequest;
use crate::pf::task::{run_pathfind, run_pathfind_visual, PathfindResult};
use crate::pf::visual::VisualConfig;
use crate::state::AppState;
use crate::worker::PathServerCommand;

pub async fn handle_request(state: Arc<AppState>, payload: Value) -> Json<Value> {
    let obj = match &payload {
        Value::Object(map) => map,
        _ => return Json(json!({"Error": "Expected JSON object"})),
    };

    let (cmd, data) = match obj.into_iter().next() {
        Some(pair) => pair,
        None => return Json(json!({"Error": "Empty object"})),
    };

    log::debug!("[http] command: {cmd}");

    match cmd.as_str() {
        "ItemsAdd"   => handle_items_add(state, data.clone()).await,
        "ItemsDel"   => handle_items_del(state, data.clone()).await,
        "WorldClear" => handle_world_clear(state, data.clone()).await,
        "WorldLoad"  => handle_world_load(state, data.clone()).await,
        "WorldSave"  => handle_world_save(state, data.clone()).await,
        "Query"      => handle_query(state, data.clone()).await,
        "TracePath"  => handle_trace_path(state, data.clone()).await,
        "TracePathVisual" => handle_trace_path_visual(state, data.clone()).await,
        "RenderArea" => Json(json!({"Error": "RenderArea must be sent to /api/render/ endpoint"})),
        cmd => Json(json!({"Error": format!("Unknown command: {}", cmd)})),
    }
}

// ── Helper: create an engine proxy for a given map ───────────────────────

fn engine_proxy(state: &AppState, world: u8) -> EngineProxy<PathServerCommand> {
    EngineProxy::<PathServerCommand>::new(state.worker_tx.clone(), world)
}

// ── ItemsAdd ──────────────────────────────────────────────────────────────

async fn handle_items_add(state: Arc<AppState>, data: Value) -> Json<Value> {
    let items = match data.get("items").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            log::warn!("[ItemsAdd] missing 'items' array in payload");
            return Json(json!({"Error": "Missing 'items' array"}));
        }
    };

    log::info!("[ItemsAdd] received {} items", items.len());

    // Default to world 0 (Felucca) if not specified at the top level.
    let world = data.get("world").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    if world >= 6 {
        return Json(json!({"Error": format!("Invalid world {world}")}));
    }

    let mut count = 0usize;

    for (idx, item) in items.iter().enumerate() {
        // Per-item world override (script sends world inside each item).
        let item_world = item.get("world").and_then(|v| v.as_u64())
            .map(|w| w as u8)
            .unwrap_or(world);

        if let (Some(serial), Some(graphic), Some(x), Some(y), Some(z_val)) = (
            item.get("serial").and_then(|v| v.as_u64()),
            item.get("graphic").and_then(|v| v.as_u64()),
            item.get("x").and_then(|v| v.as_i64()),
            item.get("y").and_then(|v| v.as_i64()),
            item.get("z").and_then(|v| v.as_i64()),
        ) {
            let serial = serial as u32;
            let graphic = graphic as u16;
            let x = x as u16;
            let y = y as u16;
            let z = z_val as i8;

            log::trace!(
                "[ItemsAdd] #{idx}: serial={serial:#010X} graphic={graphic:#06X} \
                 pos=({x},{y},{z}) world={item_world}"
            );

            let entity = DemoEntity::Item {
                serial,
                graphic,
                color: 0,
                amount: 1,
                x,
                y,
                z,
                is_container: false,
                hidden: false,
                facing: None,
            };

            engine_proxy(&state, item_world).spawn_entity(serial, entity).await;

            count += 1;
        } else {
            log::warn!(
                "[ItemsAdd] #{idx}: skipped — missing fields. raw json: {}",
                item
            );
        }
    }

    log::info!("[ItemsAdd] done: {count}/{} items sent to worker", items.len());
    Json(json!({"Success": {"added": count}}))
}

// ── ItemsDel ──────────────────────────────────────────────────────────────

async fn handle_items_del(state: Arc<AppState>, data: Value) -> Json<Value> {
    let serials = match data.get("serials").and_then(|v| v.as_array()) {
        Some(s) => s,
        None => return Json(json!({"Error": "Missing 'serials' array"})),
    };

    let world = data.get("world").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    if world >= 6 {
        return Json(json!({"Error": format!("Invalid world {world}")}));
    }

    let mut count = 0usize;

    let engine = engine_proxy(&state, world);
    for s in serials {
        if let Some(serial) = s.as_u64() {
            engine.remove_entity(serial as u32).await;
            count += 1;
        }
    }

    Json(json!({"Success": {"removed": count}}))
}

// ── WorldClear ────────────────────────────────────────────────────────────

async fn handle_world_clear(state: Arc<AppState>, data: Value) -> Json<Value> {
    let world = data.get("world").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    engine_proxy(&state, world).reset_zone(
        Vec::new(),
        framework::continuum::HashContainerStore::new(),
    ).await;

    Json(json!({"Success": {}}))
}

// ── WorldLoad ─────────────────────────────────────────────────────────────

async fn handle_world_load(state: Arc<AppState>, data: Value) -> Json<Value> {
    let filename = data.get("file_name").and_then(|v| v.as_str()).unwrap_or("world.json");

    let dir = match &state.data_dir {
        Some(d) => d.clone(),
        None => return Json(json!({"Error": "No data directory configured (pass --data-dir)"})),
    };
    let path = dir.join(filename);

    let world_data = match snapshot::load_from_file(&path) {
        Ok(d) => d,
        Err(e) => return Json(json!({"Error": format!("Load failed: {e}")})),
    };

    let mut loaded_zones = 0usize;

    for zone_data in world_data.zones {
        let map_id = zone_data.map_id;
        if map_id >= 6 {
            continue;
        }

        engine_proxy(&state, map_id).restore_snapshot(zone_data).await;
        loaded_zones += 1;
    }

    log::info!("WorldLoad: loaded {loaded_zones} zones from {filename}");
    Json(json!({"Success": {"loaded": filename, "zones": loaded_zones}}))
}

// ── WorldSave ─────────────────────────────────────────────────────────────

async fn handle_world_save(state: Arc<AppState>, data: Value) -> Json<Value> {
    let filename = data.get("file_name").and_then(|v| v.as_str()).unwrap_or("world.json");

    let dir = match &state.data_dir {
        Some(d) => d.clone(),
        None => return Json(json!({"Error": "No data directory configured (pass --data-dir)"})),
    };
    let path = dir.join(filename);

    // Collect snapshots from all 6 worlds via the worker.
    let mut zone_saves = Vec::new();
    for world_id in 0u8..6 {
        if let Some(zone_data) = engine_proxy(&state, world_id).save_snapshot().await {
            zone_saves.push(zone_data);
        }
    }

    let total_entities: usize = zone_saves.iter().map(|z| z.entities.len()).sum();

    let save_data = snapshot::WorldSaveData {
        zones: zone_saves,
        player_serial: 0,
        player_world: 0,
    };

    if let Err(e) = snapshot::save_to_file(&save_data, &path) {
        return Json(json!({"Error": format!("Save failed: {e}")}));
    }

    log::info!("WorldSave: saved {total_entities} entities to {filename}");
    Json(json!({"Success": {"saved": filename, "entities": total_entities}}))
}

// ── Query ─────────────────────────────────────────────────────────────────

async fn handle_query(state: Arc<AppState>, data: Value) -> Json<Value> {
    let world = data.get("world").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    if world >= 6 {
        return Json(json!({"Error": format!("Invalid world {world}")}));
    }

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let engine = engine_proxy(&state, world);
    let _ = engine.tx()
        .send(WorkerCommand::MapCommand(
            world,
            PathServerCommand::Engine(EngineCommand::QueryAllEntities {
                reply: reply_tx,
            }),
        ))
        .await;

    let entities = match reply_rx.await {
        Ok(e) => e,
        Err(_) => return Json(json!({"Error": "Worker unavailable"})),
    };

    let items: Vec<Value> = entities
        .iter()
        .map(|(serial, entity)| {
            let pos = framework::ecumene::Entity::pos(entity);
            json!({
                "serial": serial,
                "graphic": framework::ecumene::Entity::graphic(entity),
                "x": pos.x,
                "y": pos.y,
                "z": pos.z,
            })
        })
        .collect();

    Json(json!({"QueryReply": {"items": items}}))
}

// ── TracePath ─────────────────────────────────────────────────────────────

async fn handle_trace_path(state: Arc<AppState>, data: Value) -> Json<Value> {
    let req: TraceRequest = match serde_json::from_value(data) {
        Ok(r) => r,
        Err(e) => return Json(json!({"Error": format!("Invalid TracePath request: {e}")})),
    };

    let world = req.world as usize;
    if world >= 6 {
        return Json(json!({"Error": format!("Invalid world {world}")}));
    }

    let (map_width, map_height) = state
        .static_data
        .0
        .as_deref()
        .and_then(|sd| sd.map_tile_dimensions(req.world))
        .map(|(w, h)| (w as isize, h as isize))
        .unwrap_or((6144, 4096));

    let result = run_pathfind(&state.worker_tx, req, map_width, map_height).await;

    let points = match result {
        PathfindResult::Found(pts) => pts,
        PathfindResult::Cancelled => {
            return Json(json!({"Error": "Pathfinding cancelled"}));
        }
        PathfindResult::WorkerGone => {
            return Json(json!({"Error": "Worker unavailable"}));
        }
    };

    let success = !points.is_empty();
    let length = points.len();

    let pts: Vec<Value> = points
        .iter()
        .map(|p| json!({"x": p.x, "y": p.y, "z": p.z, "w": p.w}))
        .collect();

    Json(json!({
        "TraceReply": {
            "points": pts,
            "length": length,
            "success": success,
        }
    }))
}

// ── TracePathVisual ───────────────────────────────────────────────────────

async fn handle_trace_path_visual(state: Arc<AppState>, data: Value) -> Json<Value> {
    let req: TraceRequest = match serde_json::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => return Json(json!({"Error": format!("Invalid TracePathVisual request: {e}")})),
    };

    let world = req.world as usize;
    if world >= 6 {
        return Json(json!({"Error": format!("Invalid world {world}")}));
    }

    let (map_width, map_height) = state
        .static_data
        .0
        .as_deref()
        .and_then(|sd| sd.map_tile_dimensions(req.world))
        .map(|(w, h)| (w as isize, h as isize))
        .unwrap_or((6144, 4096));

    let mut config = VisualConfig::default();
    if let Some(delay) = data.get("step_delay_us").and_then(|v| v.as_u64()) {
        config.step_delay = std::time::Duration::from_micros(delay);
    }
    if let Some(batch) = data.get("batch_interval_ms").and_then(|v| v.as_u64()) {
        config.batch_interval = std::time::Duration::from_millis(batch);
    }

    let result = run_pathfind_visual(
        &state.worker_tx,
        req,
        map_width,
        map_height,
        world as u8,
        config,
    ).await;

    let points = match &result.pathfind {
        PathfindResult::Found(pts) => pts,
        PathfindResult::Cancelled => {
            return Json(json!({"Error": "Pathfinding cancelled"}));
        }
        PathfindResult::WorkerGone => {
            return Json(json!({"Error": "Worker unavailable"}));
        }
    };

    let success = !points.is_empty();
    let length = points.len();

    let pts: Vec<Value> = points
        .iter()
        .map(|p| json!({"x": p.x, "y": p.y, "z": p.z, "w": p.w}))
        .collect();

    Json(json!({
        "TraceReply": {
            "points": pts,
            "length": length,
            "success": success,
            "visual": {
                "frontier_count": result.stats.frontier_count,
                "visited_count": result.stats.visited_count,
                "path_count": result.stats.path_count,
                "total_spawned": result.stats.total_spawned,
                "total_removed": result.stats.total_removed,
            }
        }
    }))
}
