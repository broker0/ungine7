//! World event → UO packet translation — delegates to the shared
//! [`common::world_events`] module.
//!
//! This thin wrapper adapts the shared implementation to the path-server's
//! needs.  No observer pipeline cross-validation is performed here.

use protocol::RawPacket;

use network::error;
use network::session::Session;

use framework::continuum::WorldEvent;

use common::world_events as shared;
use common::uo_engine::auth::AccessLevel;

use super::session::PlayerState;

// ── World event → packets ─────────────────────────────────────────────────

/// Convert a world event into outbound UO packets, appending them to `out`.
pub(super) fn collect_world_event_packets(
    player: &mut PlayerState,
    event: &WorldEvent,
    out: &mut Vec<RawPacket>,
) {
    // Path-server is a dev/debug tool — show all entities, including hidden.
    shared::collect_world_event_packets(player, event, AccessLevel::Developer, &mut |_| {}, out);
}

/// Handle a single world event and send immediately (used during spawn).
pub(super) async fn handle_world_event(
    session: &mut Session,
    player: &mut PlayerState,
    event: &WorldEvent,
) -> error::Result<()> {
    shared::handle_world_event(session, player, event, AccessLevel::Developer, &mut |_| {}).await
}

// ── Zone-change synchronisation ───────────────────────────────────────────

/// Synchronise the client after a zone reset or restore.
pub(super) async fn sync_zone_change(
    player: &PlayerState,
    old_serials: &[u32],
    session: &mut Session,
    worker_tx: &crate::worker::PathServerWorkerTx,
) -> error::Result<()> {
    use framework::ecumene::Entity;
    use packets::interaction::DeleteObject;
    use packets::traits::encode_packet;
    use common::uo_engine::rpc::EngineProxy;
    use crate::worker::PathServerCommand;

    // 1. Delete old entities from the client.
    for &serial in old_serials {
        if serial == player.serial {
            continue;
        }
        session.send(RawPacket::s2c(encode_packet(&DeleteObject {
            id: 0x1D,
            serial,
        }))).await?;
    }

    // 2. Query new visible entities and send spawn packets.
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let new_entities = engine.query_area(player.view_rect).await;
    for entity in &new_entities {
        let serial = Entity::serial(entity);
        if serial == player.serial {
            continue;
        }
        let raw = if entity.is_mobile() {
            // Re-encode mobile packets in the correct format for this client.
            use packets::world::DrawMobile;
            use packets::traits::ManualPacket;
            use u_core::ProtocolVersion;
            let raw_bytes = entity.to_raw_bytes();
            if player.client_version >= ProtocolVersion::CV_70331 {
                DrawMobile::from_bytes(&raw_bytes)
                    .ok()
                    .map(|m| m.to_bytes_versioned(player.client_version))
                    .unwrap_or(raw_bytes)
            } else {
                raw_bytes
            }
        } else {
            entity.to_raw_bytes()
        };
        session.send(RawPacket::s2c(raw)).await?;
    }

    Ok(())
}
