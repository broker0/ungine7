//! Player movement: 0x02 MoveRequest + view-rect synchronisation.

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};

use u_core::{Facing, Heading};

use packets::movement::{MoveAck, MoveReject, MoveRequest, Notoriety};

use framework::continuum::WorkerCommand;
use framework::diorama::ObserverPipeline;
use framework::ecumene::TileRect;

use crate::{DemoCommand, DemoWorkerTx};
use common::uo_engine::handler::EngineCommand;

use super::{PlayerState};

// ── 0x02 MoveRequest ──────────────────────────────────────────────────────

pub(super) async fn handle_move(
    packet: &RawPacket,
    player: &mut PlayerState,
    worker_tx: &DemoWorkerTx,
    observer: &mut Option<ObserverPipeline>,
) -> Option<Vec<RawPacket>> {
    let req = MoveRequest::from_bytes(&packet.data).ok()?;
    let heading = Heading::from_raw(req.heading())?;
    let running = req.is_running();
    let facing = Facing::from_heading(heading).with_running(running);

    // Feed observer with C→S packet.
    if let Some(obs) = observer {
        obs.ingest_c2s(&packet.data);
    }

    // Send MobileStep to engine — handles both turn and step.
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            player.world,
            DemoCommand::Engine(EngineCommand::MobileStep {
                serial: player.serial,
                direction: facing,
                reply: reply_tx,
            }),
        ))
        .await;

    let result = reply_rx.await.ok()?;

    match result {
        Some(step) => {
            // Update player state from engine result.
            player.x = step.x;
            player.y = step.y;
            player.z = step.z;
            player.direction = step.direction;

            // The MoveAck notoriety is the player's own colour (self-view):
            // criminals/murderers see their own flag, others see innocent.
            let self_noto = player
                .notoriety_ctx
                .as_ref()
                .map(|c| {
                    common::uo_engine::notoriety::NotorietyClass::from_u8(c.class).base_wire()
                })
                .unwrap_or(Notoriety::Innocent);
            let ack = MoveAck {
                id: MoveAck::ID,
                sequence: req.sequence,
                notoriety: self_noto,
            };
            Some(vec![RawPacket::s2c(encode_packet(&ack))])
        }
        None => {
            // Blocked — reject and snap client back.
            let reject = MoveReject {
                id: 0x21,
                sequence: req.sequence,
                x: player.x,
                y: player.y,
                direction: player.direction,
                z: player.z,
            };
            Some(vec![RawPacket::s2c(encode_packet(&reject))])
        }
    }
}

// ── View rect synchronisation ─────────────────────────────────────────────

/// Recompute the player's view rectangle from their current position
/// and, if it changed, notify the worker so the `ObserverRegistry`
/// generates edge-updates (EntitySpawned / EntityRemoved for the
/// strips that entered / left the view).
pub(super) async fn sync_view_rect(
    player: &mut PlayerState,
    worker_tx: &DemoWorkerTx,
) {
    let new_rect = TileRect::from_view(player.x, player.y, player.view_range);
    if new_rect != player.view_rect {
        player.view_rect = new_rect;
        let _ = worker_tx.send(WorkerCommand::MapCommand(
            player.world,
            DemoCommand::UpdateObserverView(player.serial, new_rect),
        )).await;
    }
}
