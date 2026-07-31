//! Item manipulation: pick up, drop, equip, unequip.
//!
//! Handles packets:
//! - `PickUpItem` (0x07) — lift an item from ground, container, or equipment
//! - `DropItem` (0x08) — drop an item to ground or into a container
//! - `WearItem` (0x13) — equip an item onto a mobile
//!
//! All item mutations are now handled by atomic engine commands
//! (`PickUpItem`, `DropItem`, `EquipFromHeld`).  The session layer
//! only parses client packets, sends one RPC, and emits client packets
//! based on the result.

use log::debug;

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use packets::interaction::{
    DeleteObject, DropItem, EquipItem,
    PickUpItem, RejectMoveItem, RejectMoveItemReason, WearItem,
};

use common::uo_engine::auth::AccessLevel;
use common::uo_engine::handler::{
    DropTarget, DropResult, EquipResult, HeldItemInfo,
    ItemSource, PickUpReject, PickUpResult,
};

use crate::DemoWorkerTx;

use super::PlayerState;
use super::containers::OpenContainers;

// ── HeldItem ─────────────────────────────────────────────────────────────

/// An item currently held on the player's cursor.
#[derive(Debug, Clone)]
pub(super) struct HeldItem {
    pub serial: u32,
    pub graphic: u16,
    pub color: u16,
    pub amount: u16,
    pub source: ItemSource,
}

impl HeldItem {
    /// Convert to the engine's `HeldItemInfo` for Drop/Equip commands.
    pub fn to_held_info(&self) -> HeldItemInfo {
        HeldItemInfo {
            serial: self.serial,
            graphic: self.graphic,
            color: self.color,
            amount: self.amount,
        }
    }
}

// ── Container access policy ──────────────────────────────────────────────

/// Build the `accessible_containers` set for engine commands based on
/// the player's access level and currently open containers.
///
/// - **GameMaster and above** → `None` (bypass all container checks).
/// - **Player / Counselor / Seer** → `Some(set)` containing the serials
///   of all containers the player has legitimately opened (own backpack,
///   double-clicked world containers, nested sub-containers, etc.).
fn build_accessible_containers(
    access_level: AccessLevel,
    open_containers: &OpenContainers,
) -> Option<std::collections::HashSet<u32>> {
    if access_level >= AccessLevel::GameMaster {
        None // GM bypass
    } else {
        Some(open_containers.all_open_serials())
    }
}

/// Build a system message packet (gray, informational).
fn system_message(msg: &str) -> RawPacket {
    crate::game_util::system_message_gray(msg)
}

/// Try to return a held item to the player's backpack.
///
/// Used as a bounce-back when a drop is rejected (e.g. LOS failure) so
/// the item doesn't get stuck on the cursor.  Returns `true` if the item
/// was successfully placed in the backpack.
async fn bounce_item_to_backpack(
    p: &PlayerState,
    hi: &HeldItem,
    worker_tx: &DemoWorkerTx,
) -> bool {
    use framework::ecumene::Entity as EngineEntity;

    let engine = crate::game_util::engine_for(worker_tx, p.world);

    // Resolve the player's backpack serial.
    let bp_serial = match engine.get_entity(p.serial).await {
        Some(entity) => entity.backpack_serial(),
        None => None,
    };

    if let Some(bp) = bp_serial {
        let target = DropTarget::OnEntity {
            target_serial: bp,
            x: 0xFFFF,
            y: 0xFFFF,
        };
        let result = engine.drop_item(
            p.serial, hi.to_held_info(), target,
            None, // bypass access check — returning the player's own item
        ).await;
        !matches!(result, DropResult::Rejected)
    } else {
        false
    }
}

// ── PickUpItem (0x07) ────────────────────────────────────────────────────

/// Handle a PickUpItem (0x07) packet.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_pick_up(
    packet: &RawPacket,
    player: &Option<PlayerState>,
    held_item: &mut Option<HeldItem>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    access_level: AccessLevel,
    open_containers: &OpenContainers,
) -> error::Result<bool> {
    if packet.id() != PickUpItem::ID {
        return Ok(false);
    }

    let pick = match PickUpItem::from_bytes(&packet.data) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };

    let p = match player {
        Some(p) => p,
        None => return Ok(true),
    };

    // Already holding something — reject.
    if held_item.is_some() {
        session
            .send(RawPacket::s2c(
                RejectMoveItem::new(RejectMoveItemReason::AlreadyHolding).to_bytes(),
            ))
            .await?;
        return Ok(true);
    }

    let item_serial = pick.serial;
    let requested = pick.amount.max(1);

    // One atomic engine call does everything.
    let accessible = build_accessible_containers(access_level, open_containers);
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    debug!(
        "[items] player=0x{:08X}: engine_pick_up_item serial=0x{:08X}, amount={}",
        p.serial, item_serial, requested,
    );
    let result = engine.pick_up_item(
        p.serial, item_serial, requested,
        2, // max_range for ground items
        accessible,
    ).await;

    match result {
        PickUpResult::Ok(picked) => {
            let was_equipped = matches!(picked.source, ItemSource::Equipped { .. });
            // Send DeleteObject for the original item if taken from
            // equipment (client needs to remove the visual).
            // Container and ground items are handled by world events:
            // - Ground: EntityRemoved broadcast by the engine.
            // - Container: ContainerContentsUpdated broadcast by the engine.
            match &picked.source {
                ItemSource::Equipped { .. } => {
                    debug!(
                        "[items] sending DeleteObject serial=0x{:08X} (source={:?})",
                        item_serial, picked.source,
                    );
                    session
                        .send(RawPacket::s2c(encode_packet(&DeleteObject {
                            id: DeleteObject::ID,
                            serial: item_serial,
                        })))
                        .await?;
                }
                ItemSource::Container { .. } => {
                    // ContainerContentsUpdated event handles broadcast
                    // to all sessions with this container open.
                }
                ItemSource::Ground => {
                    // Ground items: EntityRemoved/EntitySpawned events are
                    // broadcast by the engine, the client will see them
                    // via the world event handler.
                }
            }

            // Partial stack remainder for container items is also handled
            // by ContainerContentsUpdated (ItemRemoved + ItemAdded).

            debug!(
                "[items] 0x{:08X} picked up 0x{:08X} ({} from {:?})",
                p.serial, picked.serial, picked.amount, picked.source,
            );

            *held_item = Some(HeldItem {
                serial: picked.serial,
                graphic: picked.graphic,
                color: picked.color,
                amount: picked.amount,
                source: picked.source,
            });

            // Send weight update to the client.
            let held_info = held_item.as_ref().map(|h| (h.serial, h.graphic, h.amount));
            super::util::send_weight_update(p, held_info, session, worker_tx).await?;

            // If the item came off equipment it may have been a "plus" weapon
            // granting a skill bonus — re-send skills so the bonus is removed.
            if was_equipped {
                super::util::send_skill_update_after_equipment_change(p, session, worker_tx).await?;
            }
        }
        PickUpResult::Rejected(reason) => {
            let reject_reason = match reason {
                PickUpReject::NotFound => RejectMoveItemReason::OutOfSight,
                PickUpReject::CannotLift => RejectMoveItemReason::CannotLift,
                PickUpReject::OutOfRange => RejectMoveItemReason::OutOfRange,
                PickUpReject::NotAccessible => RejectMoveItemReason::BelongsToAnother,
                PickUpReject::NoLineOfSight => RejectMoveItemReason::OutOfSight,
            };
            session
                .send(RawPacket::s2c(
                    RejectMoveItem::new(reject_reason).to_bytes(),
                ))
                .await?;

            // Send "out of sight" system message for LOS failures.
            if matches!(reason, PickUpReject::NoLineOfSight) {
                session.send(system_message("That is out of sight.")).await?;
            }
        }
    }

    Ok(true)
}

// ── DropItem (0x08) ──────────────────────────────────────────────────────

/// Handle a DropItem (0x08) packet.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_drop(
    packet: &RawPacket,
    player: &Option<PlayerState>,
    held_item: &mut Option<HeldItem>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    access_level: AccessLevel,
    open_containers: &OpenContainers,
) -> error::Result<bool> {
    if packet.id() != DropItem::ID {
        return Ok(false);
    }

    let drop = match DropItem::from_bytes(&packet.data) {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };

    let p = match player {
        Some(p) => p,
        None => return Ok(true),
    };

    let hi = match held_item.take() {
        Some(hi) => hi,
        None => {
            session
                .send(RawPacket::s2c(
                    RejectMoveItem::new(RejectMoveItemReason::CannotLift).to_bytes(),
                ))
                .await?;
            return Ok(true);
        }
    };

    let (target_x, target_y, target_z, container_serial) = match &drop {
        DropItem::Legacy(d) => (d.x, d.y, d.z, d.container_serial),
        DropItem::Modern(d) => (d.x, d.y, d.z, d.container_serial),
    };

    let is_ground = container_serial == 0xFFFF_FFFF || container_serial == 0;

    let target = if is_ground {
        DropTarget::Ground { x: target_x, y: target_y, z: target_z }
    } else {
        // Could be a container, a ground item, or an item inside a
        // container.  The engine resolves the action based on the
        // target type.
        DropTarget::OnEntity {
            target_serial: container_serial,
            x: target_x,
            y: target_y,
        }
    };

    let accessible = build_accessible_containers(access_level, open_containers);
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    debug!(
        "[items] player=0x{:08X}: engine_drop_item held=0x{:08X} amount={}",
        p.serial, hi.serial, hi.amount,
    );
    let result = engine.drop_item(
        p.serial, hi.to_held_info(), target,
        accessible,
    ).await;

    debug!(
        "[items] player=0x{:08X}: drop result for held 0x{:08X}: {:?}",
        p.serial, hi.serial, result,
    );

    match &result {
        DropResult::DroppedOnGround { serial } => {
            debug!(
                "[items] 0x{:08X} dropped 0x{:08X} on ground at ({},{},{})",
                p.serial, serial, target_x, target_y, target_z,
            );
            // EntitySpawned event handles broadcast to all observers.
        }
        DropResult::MergedOnGround { target_serial, new_amount } => {
            debug!(
                "[items] 0x{:08X} merged into ground stack 0x{:08X} (now {})",
                p.serial, target_serial, new_amount,
            );
            // EntityUpdated event handles broadcast.
        }
        DropResult::DroppedInContainer { container_serial, serial, x, y } => {
            // ContainerContentsUpdated event handles broadcast to all
            // sessions with this container open.
            debug!(
                "[items] 0x{:08X} dropped 0x{:08X} into container 0x{:08X} at ({},{})",
                p.serial, serial, container_serial, x, y,
            );
        }
        DropResult::MergedInContainer { target_serial, new_amount, .. } => {
            // ContainerContentsUpdated event handles broadcast.
            debug!(
                "[items] 0x{:08X} merged 0x{:08X} into container stack 0x{:08X} (now {})",
                p.serial, hi.serial, target_serial, new_amount,
            );
        }
        DropResult::FallbackGround { serial } => {
            debug!(
                "[items] 0x{:08X} dropped 0x{:08X} on ground (fallback)",
                p.serial, serial,
            );
        }
        DropResult::Rejected | DropResult::RejectedNoLos => {
            // Bounce-back: return the item based on where it came from
            // so the client doesn't lose track of it.
            match &hi.source {
                ItemSource::Ground => {
                    // Picked up from the ground — drop back at player's feet.
                    let target = DropTarget::Ground { x: p.x, y: p.y, z: p.z };
                    let _ = engine.drop_item(
                        p.serial, hi.to_held_info(), target,
                        None, // bypass — returning the player's own item
                    ).await;
                }
                ItemSource::Container { .. } | ItemSource::Equipped { .. } => {
                    // Picked up from backpack / container / equipment —
                    // return to backpack; fall back to ground if that fails.
                    let bounced = bounce_item_to_backpack(
                        p, &hi, worker_tx,
                    ).await;

                    if !bounced {
                        let target = DropTarget::Ground { x: p.x, y: p.y, z: p.z };
                        let _ = engine.drop_item(
                            p.serial, hi.to_held_info(), target,
                            None,
                        ).await;
                    }
                }
            }

            // Send "out of sight" message for LOS failures.
            if matches!(result, DropResult::RejectedNoLos) {
                session.send(system_message("That is out of sight.")).await?;
            }

            // Do NOT send RejectMoveItem (0x27) here.  The item has
            // already been placed into the backpack (or on the ground)
            // and the client will see it via ContainerContentsUpdated /
            // EntitySpawned world events.  Sending 0x27 on top of that
            // causes the client to briefly "bounce" the item back to its
            // old position, producing a visible flicker.
        }
    }

    // Send weight update after any drop (held_item was consumed in all cases).
    let held_info = held_item.as_ref().map(|h| (h.serial, h.graphic, h.amount));
    super::util::send_weight_update(p, held_info, session, worker_tx).await?;

    Ok(true)
}

// ── WearItem (0x13) ──────────────────────────────────────────────────────

/// Handle a WearItem (0x13) packet.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_wear(
    packet: &RawPacket,
    player: &Option<PlayerState>,
    held_item: &mut Option<HeldItem>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    _access_level: AccessLevel,
) -> error::Result<bool> {
    if packet.id() != WearItem::ID {
        return Ok(false);
    }

    let wear = match WearItem::from_bytes(&packet.data) {
        Ok(w) => w,
        Err(_) => return Ok(false),
    };

    let p = match player {
        Some(p) => p,
        None => return Ok(true),
    };

    let hi = match held_item.take() {
        Some(hi) => hi,
        None => {
            session
                .send(RawPacket::s2c(
                    RejectMoveItem::new(RejectMoveItemReason::CannotLift).to_bytes(),
                ))
                .await?;
            return Ok(true);
        }
    };

    // Only allow equipping on self for now.
    if wear.player_serial != p.serial {
        *held_item = Some(hi);
        session
            .send(RawPacket::s2c(
                RejectMoveItem::new(RejectMoveItemReason::BelongsToAnother).to_bytes(),
            ))
            .await?;
        return Ok(true);
    }

    let result = crate::game_util::engine_for(worker_tx, p.world)
        .equip_from_held(
            p.serial, hi.to_held_info(), wear.layer,
        ).await;

    match result {
        EquipResult::Ok { displaced } => {
            // Send EquipItem confirmation to the client.
            let eq_pkt = EquipItem {
                id: EquipItem::ID,
                item_serial: hi.serial,
                graphic: hi.graphic,
                _pad0: (),
                layer: wear.layer,
                player_serial: p.serial,
                color: hi.color,
            };
            session
                .send(RawPacket::s2c(encode_packet(&eq_pkt)))
                .await?;

            debug!(
                "[items] 0x{:08X} equipped 0x{:08X} on layer {:?}",
                p.serial, hi.serial, wear.layer,
            );

            // If there was a displaced item, put it on the cursor.
            if let Some(displaced) = displaced {
                debug!(
                    "[items] 0x{:08X} displaced 0x{:08X} from layer {:?}",
                    p.serial, displaced.serial, displaced.layer,
                );

                // Send DeleteObject for the displaced item.
                session
                    .send(RawPacket::s2c(encode_packet(&DeleteObject {
                        id: DeleteObject::ID,
                        serial: displaced.serial,
                    })))
                    .await?;

                *held_item = Some(HeldItem {
                    serial: displaced.serial,
                    graphic: displaced.graphic,
                    color: displaced.color,
                    amount: 1,
                    source: ItemSource::Equipped { mobile_serial: p.serial },
                });
            }
        }
        EquipResult::NotAMobile => {
            *held_item = Some(hi);
            session
                .send(RawPacket::s2c(
                    RejectMoveItem::new(RejectMoveItemReason::CannotLift).to_bytes(),
                ))
                .await?;
        }
    }

    // Send weight update after equip (held_item consumed, equipment changed).
    let held_info = held_item.as_ref().map(|h| (h.serial, h.graphic, h.amount));
    super::util::send_weight_update(p, held_info, session, worker_tx).await?;

    // Re-send skills with equipment bonuses applied (equipped item may be a
    // "plus" weapon granting a skill bonus).
    super::util::send_skill_update_after_equipment_change(p, session, worker_tx).await?;

    Ok(true)
}
