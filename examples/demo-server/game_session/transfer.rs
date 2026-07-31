//! Cross-zone player transfer.
//!
//! Provides [`transfer_player`] — the core function for moving a player
//! entity (with all its inventory, item properties, and AI controllers)
//! from one zone to another atomically.
//!
//! Used by the `.world` dot-command and future teleporter/portal systems.

use std::sync::Arc;

use log::info;

use protocol::RawPacket;
use packets::character::DrawGamePlayer;
use packets::mobile_flags::MobileFlags;
use packets::traits::{encode_packet, ManualPacket};

use network::error;
use network::session::Session;

use framework::continuum::{WorkerCommand, WorldEvent};
use framework::diorama::ObserverPipeline;
use framework::ecumene::TileRect;

use crate::{DemoCommand, DemoWorkerTx};

use super::PlayerState;
use super::game_logic::PendingTeleport;

/// Transfer a player to a different zone (map/facet).
///
/// Performs the complete transfer sequence:
/// 1. Unregister observer from the old zone
/// 2. Atomically transfer entity + containers + item props via
///    [`engine_transfer_entity`](common::uo_engine::rpc::EngineProxy::transfer_entity) (no data loss, no race conditions)
/// 3. If the map_id changed, send `SetMap` to the client
/// 4. Update session-level player state
/// 5. Send `DrawGamePlayer` to the client
/// 6. Register observer in the new zone and stream initial entities
///
/// `target_map` is the destination zone's map_id.  If it equals the
/// player's current zone, this is a no-op (use intra-zone teleport
/// instead for same-zone moves).
pub(super) async fn transfer_player(
    session: &mut Session,
    player: &mut PlayerState,
    access_level: common::uo_engine::auth::AccessLevel,
    worker_tx: &DemoWorkerTx,
    world_data: &crate::WorldData,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    target_map: u8,
    target_x: u16,
    target_y: u16,
    target_z: i8,
) -> error::Result<()> {
    let old_world = player.world;

    if old_world == target_map {
        // Same zone — caller should use intra-zone teleport instead.
        info!(
            "[transfer] same-zone transfer ignored (map={}, serial={:#010X})",
            target_map, player.serial,
        );
        return Ok(());
    }

    info!(
        "[transfer] {:#010X}: map {} → {} at ({},{},{})",
        player.serial, old_world, target_map,
        target_x, target_y, target_z,
    );

    // 1. Unregister observer from the old zone.
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            old_world,
            DemoCommand::UnregisterObserver(player.serial),
        ))
        .await;

    // 2. Atomically transfer entity + containers + item props.
    let engine = crate::game_util::engine_for(worker_tx, old_world);
    let transfer_result = engine.transfer_entity(
        old_world,
        target_map,
        player.serial,
        target_x,
        target_y,
        target_z,
        Some(player.direction),
    )
    .await;

    if let Err(ref e) = transfer_result {
        log::error!(
            "[transfer] failed for {:#010X}: {:?}",
            player.serial, e,
        );
        // Re-register observer on the old zone so the player isn't
        // orphaned.
        let view_rect = TileRect::from_view(player.x, player.y, player.view_range);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let _ = worker_tx
            .send(WorkerCommand::MapCommand(
                old_world,
                DemoCommand::RegisterObserver(
                    player.serial,
                    old_world,
                    view_rect,
                    event_tx_for_observer.clone(),
                    reply_tx,
                ),
            ))
            .await;
        let _ = reply_rx.await;
        return Ok(());
    }

    // 3. Send SetMap to the client (facet change).
    {
        use packets::system::GeneralInfo;
        let set_map = GeneralInfo::SetMap { world: target_map };
        let set_map_bytes = set_map.to_bytes();
        if let Some(obs) = observer.as_mut() {
            obs.ingest_s2c(&set_map_bytes);
        }
        session.send(RawPacket::s2c(set_map_bytes)).await?;
    }

    // 4. Update session-level player state.
    player.world = target_map;
    player.x = target_x;
    player.y = target_y;
    player.z = target_z;
    let view_rect = TileRect::from_view(target_x, target_y, player.view_range);
    player.view_rect = view_rect;

    // 4b. Persist the new world on the character record so the character is
    //     re-listed and re-spawned in the correct world after logout.
    update_character_world(world_data, player.serial, target_map).await;

    // 5. Register observer in the new zone.
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            target_map,
            DemoCommand::RegisterObserver(
                player.serial,
                target_map,
                view_rect,
                event_tx_for_observer.clone(),
                reply_tx,
            ),
        ))
        .await;
    let _ = reply_rx.await;

    // 6. Send DrawGamePlayer so the client sees the character.
    let engine_new = crate::game_util::engine_for(worker_tx, target_map);
    let (graphic, color) = match engine_new.get_entity(player.serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.graphic, m.color),
        _ => (crate::constants::body::MALE_HUMAN, 0),
    };
    let dgp = DrawGamePlayer {
        id: 0x20,
        serial: player.serial,
        body_type: graphic,
        _pad0: (),
        hue: color,
        flags: MobileFlags(0),
        x: target_x,
        y: target_y,
        _pad1: (),
        direction: player.direction,
        z: target_z,
    };
    let pkt = RawPacket::s2c(encode_packet(&dgp));
    if let Some(obs) = observer.as_mut() {
        obs.ingest_s2c(&pkt.data);
    }
    session.send(pkt).await?;

    // 7. Drain all initial EntitySpawned events and send to client.
    while let Ok(event) = event_rx.try_recv() {
        super::world_events::handle_world_event(session, player, &event, access_level, observer).await?;
    }

    Ok(())
}

/// Update the `world` field of the character record matching `serial`, then
/// persist the account map to disk.
///
/// Looks the character up by serial across all accounts (serials are unique
/// per server run).  No-op if no record matches (e.g. test accounts, or
/// log-loaded characters that were never created through the client).
async fn update_character_world(
    world_data: &crate::WorldData,
    serial: u32,
    world: u8,
) {
    let mut changed = false;
    {
        let mut map = world_data.account_characters.write().await;
        for records in map.values_mut() {
            if let Some(rec) = records.iter_mut().find(|r| r.serial == serial) {
                if rec.world != world {
                    rec.world = world;
                    changed = true;
                }
                break;
            }
        }
    }
    if changed {
        crate::game_util::persist_accounts(world_data).await;
    }
}

// ── Teleporter trigger ───────────────────────────────────────────────────────

/// Execute a pending teleport, if any.
///
/// Only an explicit [`PendingTeleport`] queued by another handler is honoured:
/// - Recall to a rune in another world (see [`super::recall`]).
/// - A cross-world teleporter controller that delegated the move to this
///   session via `WorldEvent::TargetedCrossWorldTeleport` (see
///   [`super::infra`]).
///
/// Step-on detection itself lives engine-side: `process_step_on_triggers`
/// drives the per-object `TeleporterController` (see
/// [`crate::controller_registry`]), so the session no longer scans the tile
/// under the player.
///
/// Cross-world destinations go through [`transfer_player`]; same-world
/// destinations use a plain intra-zone teleport (mirrors `.tele` / recall).
///
/// Returns `Ok(true)` if a teleport was performed (the caller should refresh
/// the view), `Ok(false)` if nothing happened.
#[allow(clippy::too_many_arguments)]
pub(super) async fn maybe_handle_teleport(
    session: &mut Session,
    player: &mut PlayerState,
    pending: &mut Option<PendingTeleport>,
    access_level: common::uo_engine::auth::AccessLevel,
    worker_tx: &DemoWorkerTx,
    world_data: &crate::WorldData,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
) -> error::Result<bool> {
    // Only an explicit queued teleport is honoured.
    let Some(p) = pending.take() else {
        return Ok(false);
    };
    let dest = crate::teleporters::TeleportDest { world: p.world, x: p.x, y: p.y, z: p.z };

    // Avoid teleporting onto the exact tile we're already on (prevents a
    // self-retriggering loop when a teleporter's destination is itself).
    if dest.world == player.world && dest.x == player.x && dest.y == player.y {
        return Ok(false);
    }

    if dest.world == player.world {
        intra_zone_teleport(session, player, worker_tx, dest).await?;
    } else {
        transfer_player(
            session, player, access_level, worker_tx, world_data, observer,
            event_rx, event_tx_for_observer,
            dest.world, dest.x, dest.y, dest.z,
        ).await?;
    }
    Ok(true)
}

/// Teleport the player within the same zone (no facet change).
///
/// Mirrors the `.tele` dot-command / recall: issue an engine `TeleportEntity`,
/// update session state, and tell the client via `DrawGamePlayer`.
async fn intra_zone_teleport(
    session: &mut Session,
    player: &mut PlayerState,
    worker_tx: &DemoWorkerTx,
    dest: crate::teleporters::TeleportDest,
) -> error::Result<()> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    engine.teleport(player.serial, dest.x, dest.y, dest.z, Some(player.direction)).await;

    player.x = dest.x;
    player.y = dest.y;
    player.z = dest.z;
    player.view_rect = TileRect::from_view(dest.x, dest.y, player.view_range);

    let (graphic, color) = match engine.get_entity(player.serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.graphic, m.color),
        _ => (crate::constants::body::MALE_HUMAN, 0),
    };
    let dgp = DrawGamePlayer {
        id: 0x20,
        serial: player.serial,
        body_type: graphic,
        _pad0: (),
        hue: color,
        flags: MobileFlags(0),
        x: dest.x,
        y: dest.y,
        _pad1: (),
        direction: player.direction,
        z: dest.z,
    };
    session.send(RawPacket::s2c(encode_packet(&dgp))).await?;

    info!(
        "[teleport] {:#010X}: intra-zone teleport to ({},{},{}) on world {}",
        player.serial, dest.x, dest.y, dest.z, player.world,
    );
    Ok(())
}
