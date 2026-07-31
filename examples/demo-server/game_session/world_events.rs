//! World event → UO packet translation — delegates to the shared
//! [`common::world_events`] module.
//!
//! This thin wrapper adapts the shared implementation to the demo-server's
//! specific needs:
//! - Feeds packets into an optional `ObserverPipeline` for cross-validation.
//! - Provides the `sync_zone_change` helper that uses `DemoWorkerTx`.

use protocol::RawPacket;

use network::error;
use network::session::Session;

use framework::continuum::WorldEvent;
use framework::diorama::ObserverPipeline;

use common::world_events as shared;
use common::uo_engine::auth::AccessLevel;

use crate::DemoWorkerTx;

use super::PlayerState;

// ── World event → packets ─────────────────────────────────────────────────

/// Convert a world event into outbound UO packets, appending them to `out`.
pub(super) fn collect_world_event_packets(
    player: &mut PlayerState,
    event: &WorldEvent,
    access_level: AccessLevel,
    observer: &mut Option<ObserverPipeline>,
    out: &mut Vec<RawPacket>,
) {
    // Collect packets via the shared implementation with a no-op observer hook.
    // We'll feed the observer separately below, since both `player` and
    // `observer` need mutable access during the call.
    let start = out.len();
    shared::collect_world_event_packets(player, event, access_level, &mut |_| {}, out);

    // Feed newly produced packets into the observer pipeline.
    if let Some(obs) = observer {
        for pkt in &out[start..] {
            obs.ingest_s2c(&pkt.data);
        }
    }
}

/// Legacy wrapper: handle a single world event and send immediately.
pub(super) async fn handle_world_event(
    session: &mut Session,
    player: &mut PlayerState,
    event: &WorldEvent,
    access_level: AccessLevel,
    observer: &mut Option<ObserverPipeline>,
) -> error::Result<()> {
    let mut pkts = Vec::new();
    collect_world_event_packets(player, event, access_level, observer, &mut pkts);
    if !pkts.is_empty() {
        session.send_all(pkts).await?;
    }
    Ok(())
}

/// Append a DeleteObject (0x1D) packet to `out`.
#[allow(dead_code)]
pub(super) fn collect_delete_object(
    serial: u32,
    observer: &mut Option<ObserverPipeline>,
    out: &mut Vec<RawPacket>,
) {
    let start = out.len();
    shared::collect_delete_object(serial, &mut |_| {}, out);
    if let Some(obs) = observer {
        for pkt in &out[start..] {
            obs.ingest_s2c(&pkt.data);
        }
    }
}

// ── Zone-change synchronisation ───────────────────────────────────────────

/// Synchronise the client's visible world after a zone reset or restore.
pub(super) async fn sync_zone_change(
    player: &PlayerState,
    old_serials: &[u32],
    access_level: AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    observer: &mut Option<ObserverPipeline>,
) -> error::Result<()> {
    use framework::ecumene::Entity;
    use packets::interaction::DeleteObject;
    use packets::traits::encode_packet;

    // 1. Delete old entities from the client.
    for &serial in old_serials {
        if serial == player.serial {
            continue;
        }
        let pkt = RawPacket::s2c(encode_packet(&DeleteObject {
            id: 0x1D,
            serial,
        }));
        if let Some(obs) = observer {
            obs.ingest_s2c(&pkt.data);
        }
        session.send(pkt).await?;
    }

    // 2. Query new visible entities and send spawn packets.
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let new_entities = engine.query_area(player.view_rect).await;
    for entity in &new_entities {
        let serial = Entity::serial(entity);
        if serial == player.serial {
            continue;
        }
        // Filter hidden entities for non-GM observers.
        if let Some(snapshot) = entity.snapshot() {
            if snapshot.status_flags & 0x80 != 0 && access_level < AccessLevel::GameMaster {
                continue;
            }
        }
        let raw = entity.to_raw_bytes();
        if let Some(obs) = observer {
            obs.ingest_s2c(&raw);
        }
        session.send(RawPacket::s2c(raw)).await?;
    }

    Ok(())
}
