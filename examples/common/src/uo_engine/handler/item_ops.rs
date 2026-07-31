//! Item storage, lookup, and manipulation operations.
//!
//! This module consolidates everything related to items:
//!
//! - **Result types** — [`PickUpResult`], [`DropResult`], [`EquipResult`],
//!   [`ConsumeResult`], and their supporting types
//! - **Lookup helpers** — `find_item_info`, `find_container_of_item`,
//!   `resolve_container_position`
//! - **Container event emission** — `emit_container_event`
//! - **Atomic operations** — `handle_pick_up_item`, `handle_drop_item`,
//!   `handle_consume_item`, `handle_equip_from_held`
//!
//! The [`EngineHandler`](super::EngineHandler) delegates to these functions
//! from its `match cmd` dispatch.

use log::{debug, trace, warn};

use framework::continuum::{ContainerContentChange, ContainerItem, Zone, WorldEvent};
use framework::continuum::item_props::ZoneItemProps;
use framework::continuum::container::{HashContainerStore, ZoneContainers};
use framework::ecumene::Entity as EngineEntity;
use u_core::Pos3D;

use crate::uo_engine::entity::DemoEntity;
use crate::uo_engine::serial_alloc::SerialAllocator;
use crate::uo_engine::stackable::is_stackable;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Reverse index: equipped-item serial → mobile serial.
pub(super) type EquipmentIndex = HashMap<u32, u32>;

// ── Item operation result types ─────────────────────────────────────────

/// Where an item was found during pickup.
#[derive(Debug, Clone)]
pub enum ItemSource {
    /// Top-level entity on the ground.
    Ground,
    /// Inside a container.
    Container {
        container_serial: u32,
        /// Gump-relative position of the item in the container.
        x: u16,
        y: u16,
    },
    /// Equipped on a mobile.
    Equipped { mobile_serial: u32 },
}

/// Info about the remainder left behind after a partial stack split.
#[derive(Debug, Clone)]
pub struct RemainderInfo {
    /// Newly allocated serial for the remainder.
    pub serial: u32,
    /// Amount left in the remainder.
    pub amount: u16,
}

/// Information about an item that was picked up.
#[derive(Debug, Clone)]
pub struct PickedUpItem {
    /// Serial of the item portion now held on cursor.
    /// For partial stacks this is the **original** serial; the remainder
    /// gets a freshly allocated one.
    pub serial: u32,
    pub graphic: u16,
    pub color: u16,
    /// Amount actually taken.
    pub amount: u16,
    /// Where the item came from.
    pub source: ItemSource,
    /// If a partial stack was taken, info about the remainder left behind.
    pub remainder: Option<RemainderInfo>,
}

/// Result of a successful `PickUpItem` command.
#[derive(Debug, Clone)]
pub enum PickUpResult {
    /// Item was picked up successfully.
    Ok(PickedUpItem),
    /// Rejected — with a reason code suitable for `RejectMoveItem`.
    Rejected(PickUpReject),
}

/// Reasons a pickup can be rejected.
#[derive(Debug, Clone, Copy)]
pub enum PickUpReject {
    /// Item not found in any store.
    NotFound,
    /// Cannot pick up mobiles or multis.
    CannotLift,
    /// Target is out of range.
    OutOfRange,
    /// The item's container is not accessible to this player (e.g.
    /// another player's backpack that hasn't been opened).
    NotAccessible,
    /// No line of sight to the item (blocked by wall/fence/etc.).
    NoLineOfSight,
}

/// Where to drop an item.
#[derive(Debug, Clone)]
pub enum DropTarget {
    /// Drop on the ground at world coordinates.
    Ground { x: u16, y: u16, z: i8 },
    /// Drop onto an entity — could be a container (insert/auto-stack) or
    /// a stackable item (merge).  The engine resolves the action based on
    /// the target type.
    ///
    /// `x` and `y` are gump-relative coordinates for container drops.
    /// When both are `0xFFFF` the server auto-places (and auto-stacks if
    /// a matching stack exists).
    OnEntity { target_serial: u32, x: u16, y: u16 },
}

/// Result of a `DropItem` command.
#[derive(Debug, Clone)]
pub enum DropResult {
    /// Item was placed on the ground as a new entity.
    DroppedOnGround { serial: u32 },
    /// Item was merged into an existing ground stack.
    MergedOnGround { target_serial: u32, new_amount: u16 },
    /// Item was placed into a container (possibly auto-placed).
    DroppedInContainer {
        container_serial: u32,
        /// Serial of the item in the container (same as held serial).
        serial: u32,
        /// Final gump-relative coordinates (may differ from requested
        /// when auto-placed).
        x: u16,
        y: u16,
    },
    /// Item was merged into an existing stack inside a container.
    MergedInContainer {
        container_serial: u32,
        /// Serial of the target stack that absorbed the held item.
        target_serial: u32,
        new_amount: u16,
        x: u16,
        y: u16,
    },
    /// Target container not found or not a container.
    /// Item was placed on the ground as fallback.
    FallbackGround { serial: u32 },
    /// Rejected (e.g. held item info invalid, graphic mismatch).
    Rejected,
    /// Rejected because line of sight is blocked.
    RejectedNoLos,
}

/// Information about a held item (for Drop/Equip commands).
#[derive(Debug, Clone)]
pub struct HeldItemInfo {
    pub serial: u32,
    pub graphic: u16,
    pub color: u16,
    pub amount: u16,
}

/// Result of an `EquipFromHeld` command.
#[derive(Debug, Clone)]
pub enum EquipResult {
    /// Equipped successfully. `displaced` is the item that was on the
    /// same layer before (if any).
    Ok {
        displaced: Option<DisplacedItem>,
    },
    /// The target mobile was not found or is not a mobile.
    NotAMobile,
}

/// An item that was displaced from an equipment layer by a new equip.
#[derive(Debug, Clone)]
pub struct DisplacedItem {
    pub serial: u32,
    pub graphic: u16,
    pub color: u16,
    pub layer: packets::layer::Layer,
}

/// Result of a `ConsumeItem` command.
#[derive(Debug, Clone)]
pub struct ConsumeResult {
    /// Amount remaining after consumption (0 = fully consumed).
    pub remaining: u16,
    /// Graphic of the consumed item.
    pub graphic: u16,
    /// Whether the item was on the ground (vs in a container).
    pub was_ground_item: bool,
}

// ── Lookup helpers ──────────────────────────────────────────────────────

/// Chebyshev distance between two points.
pub(super) fn chebyshev(x1: u16, y1: u16, x2: u16, y2: u16) -> u16 {
    let dx = (x1 as i32 - x2 as i32).unsigned_abs() as u16;
    let dy = (y1 as i32 - y2 as i32).unsigned_abs() as u16;
    dx.max(dy)
}

/// Search for an item by serial across all storage locations.
///
/// Returns `(serial, graphic, color, amount)` if found, `None` otherwise.
///
/// Search order:
/// 1. Top-level entities in the zone store (items, mobiles, multis).
/// 2. Equipped items on mobiles (`DemoEntity::Mobile::items`).
/// 3. Items inside containers (`HashContainerStore`).
pub(super) fn find_item_info<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    serial: u32,
    equipment_index: &EquipmentIndex,
) -> Option<(u32, u16, u16, u16)> {
    // 1. Top-level entity.
    if let Some(entity) = zone.store.get(serial) {
        return match entity {
            DemoEntity::Mobile(m) => {
                Some((m.serial, m.graphic, m.color, 1))
            }
            DemoEntity::Item { serial, graphic, color, amount, .. } => {
                Some((*serial, *graphic, *color, *amount))
            }
            DemoEntity::Multi { serial, graphic, .. } => {
                Some((*serial, *graphic, 0, 1))
            }
        };
    }

    // 2. Equipped items — O(1) via reverse index.
    if let Some(&mobile_serial) = equipment_index.get(&serial) {
        if let Some(DemoEntity::Mobile(m)) = zone.store.get(mobile_serial) {
            if let Some(eq) = m.items.iter().find(|i| i.serial == serial) {
                return Some((eq.serial, eq.graphic, eq.color.unwrap_or(0), 1));
            }
        }
    }

    // 3. Items inside containers.
    if let Some(cs) = zone.containers.find_container_of_item(serial) {
        if let Some(container) = zone.containers.get(cs) {
            if let Some(item) = container.find_item(serial) {
                return Some((item.serial, item.graphic, item.color, item.amount));
            }
        }
    }

    None
}

/// Find which container holds a given item serial.
///
/// Returns the container serial, or `None` if the item is not in any container.
///
/// Delegates to [`HashContainerStore::find_container_of_item`] which uses
/// the O(1) reverse index.
pub(super) fn find_container_of_item<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    item_serial: u32,
) -> Option<u32> {
    zone.containers.find_container_of_item(item_serial)
}

/// Walk from a container serial up to its root parent entity and return
/// the world position `(x, y)`.
///
/// The chain terminates when:
/// - `current` is found in `zone.store` (ground item / mobile) → return its position.
/// - `current` is found as an equipped item on a mobile → return the mobile's position.
/// - `current` is found inside another container → walk up one level.
///
/// Returns `None` for orphaned containers (e.g. bank boxes, vendor
/// containers) that have no world-entity root.
pub(super) fn resolve_container_position<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    container_serial: u32,
    equipment_index: &EquipmentIndex,
) -> Option<(u16, u16)> {
    resolve_container_position_3d(zone, container_serial, equipment_index).map(|(x, y, _z)| (x, y))
}

/// Like [`resolve_container_position`] but also returns the Z coordinate.
pub(super) fn resolve_container_position_3d<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    container_serial: u32,
    equipment_index: &EquipmentIndex,
) -> Option<(u16, u16, i8)> {
    let mut current = container_serial;
    for depth in 0..16 {
        // 1. Is it a world entity (ground item, mobile, multi)?
        if let Some(entity) = zone.store.get(current) {
            let pos = EngineEntity::pos(entity);
            debug!(
                "[resolve_pos] depth={}: 0x{:08X} found in zone.store at ({},{},{})",
                depth, current, pos.x, pos.y, pos.z,
            );
            return Some((pos.x, pos.y, pos.z));
        }

        // 2. Is it equipped on a mobile? O(1) via reverse index.
        if let Some(&mob_serial) = equipment_index.get(&current) {
            if let Some(DemoEntity::Mobile(m)) = zone.store.get(mob_serial) {
                debug!(
                    "[resolve_pos] depth={}: 0x{:08X} found equipped on mobile \
                     0x{:08X} at ({},{})",
                    depth, current, mob_serial, m.x, m.y,
                );
                return Some((m.x, m.y, m.z));
            }
        }

        debug!(
            "[resolve_pos] depth={}: 0x{:08X} NOT found in zone.store, \
             NOT equipped. Checking containers...",
            depth, current,
        );

        // 3. Is it an item inside another container? Walk up.
        match find_container_of_item(zone, current) {
            Some(parent) => {
                debug!(
                    "[resolve_pos] depth={}: 0x{:08X} found inside container 0x{:08X}, walking up",
                    depth, current, parent,
                );
                current = parent;
            }
            None => {
                warn!(
                    "[resolve_pos] depth={}: 0x{:08X} not found anywhere — ORPHAN \
                     (not in store, not equipped, not in any container)",
                    depth, current,
                );
                return None;
            }
        }
    }
    warn!(
        "[resolve_pos] depth limit reached for container 0x{:08X}",
        container_serial,
    );
    None
}

/// Emit a [`WorldEvent::ContainerContentsUpdated`] if the container's
/// world position can be resolved.  No-op for orphaned containers
/// (bank boxes, vendor containers).
pub(super) fn emit_container_event<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    equipment_index: &EquipmentIndex,
    container_serial: u32,
    changes: Vec<ContainerContentChange>,
) {
    if changes.is_empty() {
        return;
    }
    if let Some((x, y)) = resolve_container_position(zone, container_serial, equipment_index) {
        trace!(
            "[container_event] EMIT ContainerContentsUpdated: container=0x{:08X}, \
             pos=({},{}), {} change(s): {:?}",
            container_serial, x, y, changes.len(),
            changes.iter().map(|c| match c {
                ContainerContentChange::ItemAdded { item_serial, .. } =>
                    format!("Add(0x{:08X})", item_serial),
                ContainerContentChange::ItemRemoved { item_serial } =>
                    format!("Rm(0x{:08X})", item_serial),
                ContainerContentChange::ItemUpdated { item_serial, amount, .. } =>
                    format!("Upd(0x{:08X},amt={})", item_serial, amount),
            }).collect::<Vec<_>>(),
        );
        let _ = event_tx.send(WorldEvent::ContainerContentsUpdated {
            map_id: zone.map_id,
            container_serial,
            x,
            y,
            changes,
        });
    } else {
        warn!(
            "[container_event] DROPPED — resolve_container_position returned None for \
             container=0x{:08X}, skipping {} change(s)",
            container_serial, changes.len(),
        );
    }
}

// ── Atomic item operations ──────────────────────────────────────────────

/// Atomically pick up an item from the zone.
///
/// `accessible_containers` controls the container access policy:
/// - `None` — GM bypass: all containers are accessible.
/// - `Some(set)` — only containers whose serial is in the set are
///   accessible.  The set should contain the player's own backpack,
///   all nested sub-containers the player has opened, and any world
///   containers the player has explicitly opened (double-clicked).
pub(super) fn handle_pick_up_item<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial_alloc: &Arc<SerialAllocator>,
    equipment_index: &mut EquipmentIndex,
    player_serial: u32,
    item_serial: u32,
    requested_amount: u16,
    max_range: u16,
    accessible_containers: Option<&HashSet<u32>>,
) -> PickUpResult {
    let requested = requested_amount.max(1);

    /// Eye-height offset for LOS checks (same as combat/magic).
    const EYE_HEIGHT: i16 = 14;

    // Whether LOS + access checks are enforced.  `accessible_containers`
    // being `None` indicates a GM bypass — skip LOS too.
    let enforce_checks = accessible_containers.is_some();

    // Get player position for range + LOS checks.
    let (px, py, pz) = match zone.store.get(player_serial) {
        Some(DemoEntity::Mobile(m)) => (m.x, m.y, m.z as i16),
        _ => (0, 0, 0),
    };

    // 1. Check top-level entity (ground item).
    if let Some(entity) = zone.store.get(item_serial) {
        match entity {
            DemoEntity::Item { serial, graphic, color, amount, x, y, z, .. } => {
                let serial = *serial;
                let graphic = *graphic;
                let color = *color;
                let total = *amount;
                let ix = *x;
                let iy = *y;
                let iz = *z;

                // Range check for ground items.
                if max_range > 0 && chebyshev(px, py, ix, iy) > max_range {
                    return PickUpResult::Rejected(PickUpReject::OutOfRange);
                }

                // LOS check for ground items.
                if enforce_checks && !zone.has_los(
                    px, py, pz + EYE_HEIGHT,
                    ix, iy, iz as i16,
                ) {
                    debug!(
                        "[pick_up] REJECTED: no LOS from ({},{},{}) to ground item \
                         0x{:08X} at ({},{},{})",
                        px, py, pz, serial, ix, iy, iz,
                    );
                    return PickUpResult::Rejected(PickUpReject::NoLineOfSight);
                }

                let take = requested.min(total);
                let remaining = total.saturating_sub(take);

                if remaining == 0 {
                    // Take the entire stack — remove entity.
                    let last_pos = Pos3D::new(ix, iy, iz);
                    zone.remove(serial);
                    let _ = event_tx.send(WorldEvent::EntityRemoved {
                        map_id: zone.map_id,
                        serial,
                        last_pos,
                    });

                    return PickUpResult::Ok(PickedUpItem {
                        serial,
                        graphic,
                        color,
                        amount: take,
                        source: ItemSource::Ground,
                        remainder: None,
                    });
                } else {
                    // Partial stack — held item keeps the original serial;
                    // remainder gets a freshly allocated serial.
                    let remainder_serial = serial_alloc.alloc_item()
                        .expect("item serial space exhausted");

                    // Remove the old entity and spawn remainder with new serial.
                    let pos = Pos3D::new(ix, iy, iz);
                    zone.remove(serial);
                    let _ = event_tx.send(WorldEvent::EntityRemoved {
                        map_id: zone.map_id,
                        serial,
                        last_pos: pos,
                    });

                    let remainder_entity = DemoEntity::Item {
                        serial: remainder_serial, graphic, color,
                        amount: remaining,
                        x: ix, y: iy, z: iz,
                        is_container: false, hidden: false,
                        facing: None,
                    };
                    let snap = remainder_entity.snapshot();
                    zone.spawn(remainder_serial, remainder_entity);
                    let _ = event_tx.send(WorldEvent::EntitySpawned {
                        map_id: zone.map_id,
                        serial: remainder_serial,
                        pos,
                        entity: snap,
                    });

                    // Clone item properties to the remainder.
                    if let Some(props) = zone.item_props.get(serial) {
                        let cloned = props.clone();
                        zone.item_props.insert(remainder_serial, cloned);
                    }
                    return PickUpResult::Ok(PickedUpItem {
                        serial,
                        graphic,
                        color,
                        amount: take,
                        source: ItemSource::Ground,
                        remainder: Some(RemainderInfo {
                            serial: remainder_serial,
                            amount: remaining,
                        }),
                    });
                }
            }
            DemoEntity::Mobile(_) | DemoEntity::Multi { .. } => {
                return PickUpResult::Rejected(PickUpReject::CannotLift);
            }
        }
    }

    // 2. Check equipped items — try to unequip from the player.
    if let Some(entity) = zone.store.get_mut(player_serial) {
        if let DemoEntity::Mobile(m) = entity {
            if let Some(idx) = m.items.iter().position(|eq| eq.serial == item_serial) {
                let eq = m.items.remove(idx);
                equipment_index.remove(&item_serial);
                // Emit EntityUpdated for the mobile.
                if let Some(e) = zone.store.get(player_serial) {
                    let pos = EngineEntity::pos(e);
                    let snap = e.snapshot();
                    let _ = event_tx.send(WorldEvent::EntityUpdated {
                        map_id: zone.map_id,
                        serial: player_serial,
                        pos,
                        entity: snap,
                    });
                }
                return PickUpResult::Ok(PickedUpItem {
                    serial: eq.serial,
                    graphic: eq.graphic,
                    color: eq.color.unwrap_or(0),
                    amount: 1,
                    source: ItemSource::Equipped { mobile_serial: player_serial },
                    remainder: None,
                });
            }
        }
    }

    // 3. Check container items.
    // First, find the item and its info via the reverse index.
    let mut found_info: Option<(u32, u16, u16, u16, u32, u16, u16)> = None; // (serial, graphic, color, amount, container_serial, cx, cy)
    if let Some(container_serial) = zone.containers.find_container_of_item(item_serial) {
        if let Some(container) = zone.containers.get(container_serial) {
            if let Some(ci) = container.find_item(item_serial) {
                found_info = Some((ci.serial, ci.graphic, ci.color, ci.amount, container_serial, ci.x, ci.y));
            }
        }
    }

    if let Some((serial, graphic, color, total, container_serial, cx, cy)) = found_info {
        // Determine if this is a ground (world) container or a non-ground
        // container (backpack, nested, bank box, etc.).
        let is_ground_container = zone.store.get(container_serial).is_some();

        if enforce_checks {
            if is_ground_container {
                // Ground container: check range + LOS + accessible set.
                const CONTAINER_PICKUP_RANGE: u16 = 2;
                if let Some(DemoEntity::Item { x: gx, y: gy, z: gz, .. }) =
                    zone.store.get(container_serial)
                {
                    let (gx, gy, gz) = (*gx, *gy, *gz);
                    if chebyshev(px, py, gx, gy) > CONTAINER_PICKUP_RANGE {
                        debug!(
                            "[pick_up] REJECTED: ground container 0x{:08X} out of range",
                            container_serial,
                        );
                        return PickUpResult::Rejected(PickUpReject::OutOfRange);
                    }
                    if !zone.has_los(
                        px, py, pz + EYE_HEIGHT,
                        gx, gy, gz as i16,
                    ) {
                        debug!(
                            "[pick_up] REJECTED: no LOS to ground container 0x{:08X}",
                            container_serial,
                        );
                        return PickUpResult::Rejected(PickUpReject::NoLineOfSight);
                    }
                }
                // The container must also be in the accessible set
                // (i.e. the player has it open).  Even though it's a
                // ground container in range+LOS, the server tracks open
                // state separately and can close it (e.g. on movement).
                if let Some(allowed) = accessible_containers {
                    if !allowed.contains(&container_serial) {
                        debug!(
                            "[pick_up] REJECTED: ground container 0x{:08X} not in \
                             accessible set (player 0x{:08X}, item 0x{:08X})",
                            container_serial, player_serial, serial,
                        );
                        return PickUpResult::Rejected(PickUpReject::NotAccessible);
                    }
                }
            } else {
                // Non-ground container (backpack, nested, vendor, etc.):
                // must be in the accessible set (i.e. previously opened).
                if let Some(allowed) = accessible_containers {
                    if !allowed.contains(&container_serial) {
                        debug!(
                            "[pick_up] REJECTED: container 0x{:08X} not in accessible set \
                             (player 0x{:08X}, item 0x{:08X})",
                            container_serial, player_serial, serial,
                        );
                        return PickUpResult::Rejected(PickUpReject::NotAccessible);
                    }
                }

                // LOS check — resolve the container's world position.
                if let Some((cx3, cy3, cz3)) = resolve_container_position_3d(zone, container_serial, equipment_index) {
                    if !zone.has_los(
                        px, py, pz + EYE_HEIGHT,
                        cx3, cy3, cz3 as i16,
                    ) {
                        debug!(
                            "[pick_up] REJECTED: no LOS from ({},{},{}) to container \
                             0x{:08X} at ({},{},{})",
                            px, py, pz, container_serial, cx3, cy3, cz3,
                        );
                        return PickUpResult::Rejected(PickUpReject::NoLineOfSight);
                    }
                }
            }
        }

        let take = requested.min(total);
        let remaining = total.saturating_sub(take);

        debug!(
            "[pick_up] container item 0x{:08X} in container 0x{:08X}: \
             total={}, requested={}, take={}, remaining={}",
            serial, container_serial, total, requested, take, remaining,
        );

        // Log container contents before mutation.
        if let Some(info) = zone.containers.get(container_serial) {
            debug!(
                "[pick_up] container 0x{:08X} BEFORE: {} items",
                container_serial, info.item_count(),
            );
        }

        if remaining == 0 {
            // Take entire stack — remove from container.
            let removed = zone.containers.remove_item_from(container_serial, serial);
            debug!(
                "[pick_up] full take: remove_item(0x{:08X}) = {}",
                serial, removed,
            );

            // Log container contents after mutation.
            if let Some(info) = zone.containers.get(container_serial) {
                debug!(
                    "[pick_up] container 0x{:08X} AFTER: {} items",
                    container_serial, info.item_count(),
                );
            }

            emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
                ContainerContentChange::ItemRemoved { item_serial: serial },
            ]);

            return PickUpResult::Ok(PickedUpItem {
                serial,
                graphic,
                color,
                amount: take,
                source: ItemSource::Container { container_serial, x: cx, y: cy },
                remainder: None,
            });
        } else {
            // Partial stack — held item keeps the original serial;
            // remainder gets a freshly allocated serial.
            let remainder_serial = serial_alloc.alloc_item()
                .expect("item serial space exhausted");

            // Remove old item from container.
            let removed = zone.containers.remove_item_from(container_serial, serial);
            debug!(
                "[pick_up] partial split: remove_item(0x{:08X}) = {}, \
                 remainder_serial=0x{:08X}, remainder_amount={}",
                serial, removed, remainder_serial, remaining,
            );

            // Insert remainder with new serial back into the container.
            zone.containers.ingest_item_upsert(container_serial, ContainerItem {
                serial: remainder_serial,
                graphic,
                amount: remaining,
                x: cx,
                y: cy,
                color,
                grid_index: None,
            });

            // Log container contents after mutation.
            if let Some(info) = zone.containers.get(container_serial) {
                debug!(
                    "[pick_up] container 0x{:08X} AFTER: {} items",
                    container_serial, info.item_count(),
                );
            }

            // Clone item properties to the remainder.
            if let Some(props) = zone.item_props.get(item_serial) {
                let cloned = props.clone();
                zone.item_props.insert(remainder_serial, cloned);
            }

            // Emit: remove original + add remainder with new serial.
            emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
                ContainerContentChange::ItemRemoved { item_serial: serial },
                ContainerContentChange::ItemAdded {
                    item_serial: remainder_serial,
                    graphic,
                    amount: remaining,
                    x: cx,
                    y: cy,
                    color,
                },
            ]);

            return PickUpResult::Ok(PickedUpItem {
                serial,
                graphic,
                color,
                amount: take,
                source: ItemSource::Container { container_serial, x: cx, y: cy },
                remainder: Some(RemainderInfo {
                    serial: remainder_serial,
                    amount: remaining,
                }),
            });
        }
    }

    // Not found.
    PickUpResult::Rejected(PickUpReject::NotFound)
}

// -- DropItem logic -----------------------------------------------------------

/// Atomically drop a held item onto the ground or into a container.
///
/// `accessible_containers` controls the container access policy:
/// - `None` — GM bypass: all containers are accessible.
/// - `Some(set)` — only containers whose serial is in the set can be
///   dropped into.
pub(super) fn handle_drop_item<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    player_serial: u32,
    item: &HeldItemInfo,
    target: DropTarget,
    accessible_containers: Option<&HashSet<u32>>,
    equipment_index: &EquipmentIndex,
) -> DropResult {
    /// Eye-height offset for LOS checks.
    const EYE_HEIGHT: i16 = 14;

    let enforce_checks = accessible_containers.is_some();

    // Get player position for LOS checks.
    let (px, py, pz) = if enforce_checks {
        match zone.store.get(player_serial) {
        Some(DemoEntity::Mobile(m)) => (m.x, m.y, m.z as i16),
            _ => (0, 0, 0),
        }
    } else {
        (0, 0, 0)
    };

    match target {
        DropTarget::Ground { x, y, z } => {
            // LOS check for ground drops.
            if enforce_checks && !zone.has_los(
                px, py, pz + EYE_HEIGHT,
                x, y, z as i16,
            ) {
                debug!(
                    "[drop] REJECTED: no LOS from ({},{},{}) to ground ({},{},{})",
                    px, py, pz, x, y, z,
                );
                return DropResult::RejectedNoLos;
            }

            debug!(
                "[drop] Ground drop: serial=0x{:08X}, graphic=0x{:04X}, amount={}, pos=({},{},{})",
                item.serial, item.graphic, item.amount, x, y, z,
            );

            // Ground drops never auto-merge — the player explicitly chose
            // a tile, not a specific stack.  Merging only happens when the
            // client targets an existing item (DropTarget::OnEntity).

            // Create new ground entity.
            let already_exists = zone.store.get(item.serial).is_some();
            if already_exists {
                debug!(
                    "[drop] WARNING: serial 0x{:08X} already exists in zone.store!",
                    item.serial,
                );
            }
            // Preserve the item's container nature.  If the held item has a
            // `ContainerInfo` record in `zone.containers` (e.g. it was lifted
            // out of another container), it must be spawned as a container so
            // it can be opened and dropped into again on the ground.  Without
            // this, the entity would be `is_container: false` and `drop_on_entity`
            // would treat it as a plain ground item (rejecting drops).
            let is_container = zone.containers.get(item.serial).is_some();
            let entity = DemoEntity::Item {
                serial: item.serial,
                graphic: item.graphic,
                color: item.color,
                amount: item.amount,
                x, y, z,
                is_container, hidden: false,
                facing: None,
            };
            let pos = EngineEntity::pos(&entity);
            let snap = entity.snapshot();
            zone.spawn(item.serial, entity);
            let _ = event_tx.send(WorldEvent::EntitySpawned {
                map_id: zone.map_id,
                serial: item.serial,
                pos,
                entity: snap,
            });

            DropResult::DroppedOnGround { serial: item.serial }
        }

        DropTarget::OnEntity { target_serial, x, y } => {
            drop_on_entity(zone, event_tx, item, target_serial, x, y,
                           accessible_containers, enforce_checks, px, py, pz,
                           equipment_index)
        }
    }
}

/// Handle `DropTarget::OnEntity` — resolve whether the target is a
/// container, a ground item, or an item inside a container, and act
/// accordingly.
fn drop_on_entity<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    item: &HeldItemInfo,
    target_serial: u32,
    x: u16,
    y: u16,
    accessible_containers: Option<&HashSet<u32>>,
    enforce_checks: bool,
    px: u16,
    py: u16,
    pz: i16,
    equipment_index: &EquipmentIndex,
) -> DropResult {
    const EYE_HEIGHT: i16 = 14;

    // 1. Check zone store (ground entity).
    //
    // A ground item counts as a container target if either its `is_container`
    // flag is set OR it has a `ContainerInfo` record in `zone.containers`
    // (defensive fallback for items whose flag was lost on a previous
    // ground-drop — see `handle_drop_item`).
    let target_is_ground_container = matches!(
        zone.store.get(target_serial),
        Some(DemoEntity::Item { is_container: true, .. })
    ) || (zone.store.get(target_serial).map_or(false, |e| matches!(e, DemoEntity::Item { .. }))
        && zone.containers.get(target_serial).is_some());

    // Whether the held item may merge into an existing stack at all.
    let item_stackable = is_stackable(item.graphic, zone.static_data().map(|a| a.as_ref()));

    if let Some(entity) = zone.store.get(target_serial) {
        match entity {
            DemoEntity::Item { x: cx, y: cy, z: cz, .. } if target_is_ground_container => {
                let (cx, cy, cz) = (*cx, *cy, *cz);
                // Target is a ground (world) container.  These are always
                // accessible if they are within range and line of sight —
                // no need to have opened them first.
                if enforce_checks {
                    // Range check (same range as pickup).
                    const DROP_RANGE: u16 = 2;
                    if chebyshev(px, py, cx, cy) > DROP_RANGE {
                        debug!(
                            "[drop] REJECTED: ground container 0x{:08X} out of range \
                             (dist={}, max={})",
                            target_serial, chebyshev(px, py, cx, cy), DROP_RANGE,
                        );
                        return DropResult::Rejected;
                    }
                    // LOS check to the ground container.
                    if !zone.has_los(
                        px, py, pz + EYE_HEIGHT,
                        cx, cy, cz as i16,
                    ) {
                        debug!(
                            "[drop] REJECTED: no LOS to ground container 0x{:08X} at ({},{},{})",
                            target_serial, cx, cy, cz,
                        );
                        return DropResult::RejectedNoLos;
                    }
                }
                return drop_into_container(zone, event_tx, item, target_serial, x, y, equipment_index);
            }
            DemoEntity::Item {
                graphic, color, amount, x: ix, y: iy, z: iz, ..
            } => {
                let (ix, iy, iz) = (*ix, *iy, *iz);
                // Target is a ground item — try to merge stacks.
                // LOS check to the ground item.
                if enforce_checks && !zone.has_los(
                    px, py, pz + EYE_HEIGHT,
                    ix, iy, iz as i16,
                ) {
                    debug!(
                        "[drop] REJECTED: no LOS to ground item 0x{:08X} at ({},{},{})",
                        target_serial, ix, iy, iz,
                    );
                    return DropResult::RejectedNoLos;
                }
                if item_stackable && *graphic == item.graphic && *color == item.color {
                    let new_amount = amount.saturating_add(item.amount);
                    if let Some(DemoEntity::Item { amount, .. }) =
                        zone.store.get_mut(target_serial)
                    {
                        *amount = new_amount;
                    }
                    if let Some(e) = zone.store.get(target_serial) {
                        let pos = EngineEntity::pos(e);
                        let snap = e.snapshot();
                        let _ = event_tx.send(WorldEvent::EntityUpdated {
                            map_id: zone.map_id,
                            serial: target_serial,
                            pos,
                            entity: snap,
                        });
                    }
                    zone.item_props.remove(item.serial);
                    return DropResult::MergedOnGround {
                        target_serial,
                        new_amount,
                    };
                }
                // Graphic/color mismatch — can't merge.
                return DropResult::Rejected;
            }
            _ => {
                // Mobile or multi — can't drop on those.
                return DropResult::Rejected;
            }
        }
    }

    // 2. Check container store — target might be a container that is NOT
    //    a top-level ground entity (e.g. player backpack, nested container).
    //    These exist in zone.containers but not in zone.store.
    if zone.containers.get(target_serial).is_some() {
        if let Some(allowed) = accessible_containers {
            if !allowed.contains(&target_serial) {
                debug!(
                    "[drop] REJECTED: container 0x{:08X} not accessible (non-ground)",
                    target_serial,
                );
                return DropResult::Rejected;
            }
        }
        // LOS check for non-ground container — resolve world position.
        if enforce_checks {
            if let Some((cx3, cy3, cz3)) = resolve_container_position_3d(zone, target_serial, equipment_index) {
                if !zone.has_los(
                    px, py, pz + EYE_HEIGHT,
                    cx3, cy3, cz3 as i16,
                ) {
                    debug!(
                        "[drop] REJECTED: no LOS to container 0x{:08X} at ({},{},{})",
                        target_serial, cx3, cy3, cz3,
                    );
                    return DropResult::RejectedNoLos;
                }
            }
        }
        return drop_into_container(zone, event_tx, item, target_serial, x, y, equipment_index);
    }

    // 3. Check container items — target might be an item inside a container.
    let mut found: Option<(u32, u16, u16, u16, u16, u16)> = None;
    // (container_serial, graphic, color, amount, cx, cy)
    if let Some(cs) = zone.containers.find_container_of_item(target_serial) {
        if let Some(container) = zone.containers.get(cs) {
            if let Some(ci) = container.find_item(target_serial) {
                found = Some((cs, ci.graphic, ci.color, ci.amount, ci.x, ci.y));
            }
        }
    }

    if let Some((container_serial, graphic, color, existing_amount, cx, cy)) = found {
        // Access check: verify the container is accessible.
        if let Some(allowed) = accessible_containers {
            if !allowed.contains(&container_serial) {
                debug!(
                    "[drop] REJECTED: container 0x{:08X} not accessible (merge target)",
                    container_serial,
                );
                return DropResult::Rejected;
            }
        }

        // LOS check for the container holding the merge target.
        if enforce_checks {
            if let Some((cx3, cy3, cz3)) = resolve_container_position_3d(zone, container_serial, equipment_index) {
                if !zone.has_los(
                    px, py, pz + EYE_HEIGHT,
                    cx3, cy3, cz3 as i16,
                ) {
                    debug!(
                        "[drop] REJECTED: no LOS to container 0x{:08X} at ({},{},{}) (merge target)",
                        container_serial, cx3, cy3, cz3,
                    );
                    return DropResult::RejectedNoLos;
                }
            }
        }

        if item_stackable && graphic == item.graphic && color == item.color {
            let new_amount = existing_amount.saturating_add(item.amount);
            // Update amount in container content via direct lookup.
            if let Some(info) = zone.containers.get_mut(container_serial) {
                if let Some(ci) = info.find_item_mut(target_serial) {
                    ci.amount = new_amount;
                }
            }
            zone.item_props.remove(item.serial);

            emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
                ContainerContentChange::ItemUpdated {
                    item_serial: target_serial,
                    graphic,
                    amount: new_amount,
                    x: cx,
                    y: cy,
                    color,
                },
            ]);

            return DropResult::MergedInContainer {
                container_serial,
                target_serial,
                new_amount,
                x: cx,
                y: cy,
            };
        }
        // Not stackable, or graphic/color mismatch — place the item into the
        // same container alongside the target instead of merging.
        return drop_into_container(zone, event_tx, item, container_serial, x, y, equipment_index);
    }

    // 3. Target not found anywhere.
    DropResult::Rejected
}

/// Insert an item into a container, with optional auto-stacking.
///
/// When `x == 0xFFFF && y == 0xFFFF` the server first looks for an
/// existing stack of the same type to merge into.  If none is found it
/// picks random gump coordinates for placement.
fn drop_into_container<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    item: &HeldItemInfo,
    container_serial: u32,
    x: u16,
    y: u16,
    equipment_index: &EquipmentIndex,
) -> DropResult {
    let auto_place = x == 0xFFFF && y == 0xFFFF;

    if auto_place {
        // Try to find an existing stack of the same graphic+color in
        // this container and merge into it — but only for stackable items.
        let item_stackable = is_stackable(item.graphic, zone.static_data().map(|a| a.as_ref()));
        let merge_target = if item_stackable {
            zone.containers.get(container_serial)
                .and_then(|c| {
                    c.items.iter()
                        .find(|i| i.graphic == item.graphic && i.color == item.color)
                        .map(|i| (i.serial, i.amount, i.x, i.y))
                })
        } else {
            None
        };

        if let Some((ts, existing_amount, cx, cy)) = merge_target {
            let new_amount = existing_amount.saturating_add(item.amount);
            // Update amount in-place.
            if let Some(info) = zone.containers.get_mut(container_serial) {
                if let Some(ci) = info.find_item_mut(ts) {
                    ci.amount = new_amount;
                }
            }
            zone.item_props.remove(item.serial);

            emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
                ContainerContentChange::ItemUpdated {
                    item_serial: ts,
                    graphic: item.graphic,
                    amount: new_amount,
                    x: cx,
                    y: cy,
                    color: item.color,
                },
            ]);

            return DropResult::MergedInContainer {
                container_serial,
                target_serial: ts,
                new_amount,
                x: cx,
                y: cy,
            };
        }

        // No merge target — auto-place at random gump coordinates.
        // TODO: use gump_model-based bounds table for accurate placement.
        let rx = (item.serial.wrapping_mul(7) % 101 + 20) as u16; // 20..120
        let ry = (item.serial.wrapping_mul(13) % 101 + 50) as u16; // 50..150

        zone.containers.ingest_item_upsert(container_serial, ContainerItem {
            serial: item.serial,
            graphic: item.graphic,
            amount: item.amount,
            x: rx,
            y: ry,
            color: item.color,
            grid_index: None,
        });

        emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
            ContainerContentChange::ItemAdded {
                item_serial: item.serial,
                graphic: item.graphic,
                amount: item.amount,
                x: rx,
                y: ry,
                color: item.color,
            },
        ]);

        return DropResult::DroppedInContainer {
            container_serial,
            serial: item.serial,
            x: rx,
            y: ry,
        };
    }

    // Explicit coordinates — insert at the requested position.
    zone.containers.ingest_item_upsert(container_serial, ContainerItem {
        serial: item.serial,
        graphic: item.graphic,
        amount: item.amount,
        x,
        y,
        color: item.color,
        grid_index: None,
    });

    emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
        ContainerContentChange::ItemAdded {
            item_serial: item.serial,
            graphic: item.graphic,
            amount: item.amount,
            x,
            y,
            color: item.color,
        },
    ]);

    DropResult::DroppedInContainer {
        container_serial,
        serial: item.serial,
        x,
        y,
    }
}

// -- ConsumeItem logic --------------------------------------------------------

pub(super) fn handle_consume_item<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    item_serial: u32,
    amount: u16,
    expected_graphic: Option<u16>,
    equipment_index: &EquipmentIndex,
) -> Option<ConsumeResult> {
    let amount = amount.max(1);

    // 1. Try container items first.
    // Use reverse index for O(1) lookup of the item's container.
    let mut container_hit: Option<(u32, u16, u16, u16, u16, u16)> = None;
    // (container_serial, graphic, cur_amount, color, cx, cy)
    if let Some(cs) = zone.containers.find_container_of_item(item_serial) {
        if let Some(info) = zone.containers.get(cs) {
            if let Some(ci) = info.find_item(item_serial) {
                container_hit = Some((cs, ci.graphic, ci.amount, ci.color, ci.x, ci.y));
            }
        }
    }

    if let Some((container_serial, graphic, cur_amount, color, cx, cy)) = container_hit {
        // Graphic check.
        if let Some(expected) = expected_graphic {
            if graphic != expected {
                return None;
            }
        }

        let remaining = cur_amount.saturating_sub(amount);
        if remaining == 0 {
            // Remove entirely.
            zone.containers.remove_item_from(container_serial, item_serial);

            emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
                ContainerContentChange::ItemRemoved { item_serial },
            ]);
        } else {
            // Update amount.
            if let Some(info) = zone.containers.get_mut(container_serial) {
                if let Some(ci) = info.find_item_mut(item_serial) {
                    ci.amount = remaining;
                }
            }

            emit_container_event(zone, event_tx, equipment_index, container_serial, vec![
                ContainerContentChange::ItemUpdated {
                    item_serial,
                    graphic,
                    amount: remaining,
                    x: cx,
                    y: cy,
                    color,
                },
            ]);
        }

        return Some(ConsumeResult {
            remaining,
            graphic,
            was_ground_item: false,
        });
    }

    // 2. Try ground entity.
    if let Some(entity) = zone.store.get(item_serial) {
        if let DemoEntity::Item { graphic, amount: cur_amount, x, y, z, color, .. } = entity {
            let graphic = *graphic;
            let cur_amount = *cur_amount;
            let (ix, iy, iz) = (*x, *y, *z);
            let color = *color;

            if let Some(expected) = expected_graphic {
                if graphic != expected {
                    return None;
                }
            }

            let remaining = cur_amount.saturating_sub(amount);
            if remaining == 0 {
                // Remove the ground item.
                let last_pos = Pos3D::new(ix, iy, iz);
                zone.remove(item_serial);
                // Clean up item properties.
                zone.item_props.remove(item_serial);
                let _ = event_tx.send(WorldEvent::EntityRemoved {
                    map_id: zone.map_id,
                    serial: item_serial,
                    last_pos,
                });
            } else {
                // Update amount.
                let updated = DemoEntity::Item {
                    serial: item_serial,
                    graphic,
                    color,
                    amount: remaining,
                    x: ix, y: iy, z: iz,
                    is_container: false, hidden: false,
                    facing: None,
                };
                let pos = Pos3D::new(ix, iy, iz);
                let snap = updated.snapshot();
                zone.update(item_serial, updated);
                let _ = event_tx.send(WorldEvent::EntityUpdated {
                    map_id: zone.map_id,
                    serial: item_serial,
                    pos,
                    entity: snap,
                });
            }

            return Some(ConsumeResult {
                remaining,
                graphic,
                was_ground_item: true,
            });
        }
    }

    None
}

// -- EquipFromHeld logic ------------------------------------------------------

pub(super) fn handle_equip_from_held<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    equipment_index: &mut EquipmentIndex,
    mobile_serial: u32,
    item: &HeldItemInfo,
    layer: packets::layer::Layer,
) -> EquipResult {
    let entity = match zone.store.get_mut(mobile_serial) {
        Some(e) => e,
        None => return EquipResult::NotAMobile,
    };

    let items = match entity {
        DemoEntity::Mobile(m) => &mut m.items,
        _ => return EquipResult::NotAMobile,
    };

    // Remove existing item on the same layer (if any).
    let displaced = if let Some(idx) = items.iter().position(|eq| eq.layer == layer) {
        let old = items.remove(idx);
        equipment_index.remove(&old.serial);
        Some(DisplacedItem {
            serial: old.serial,
            graphic: old.graphic,
            color: old.color.unwrap_or(0),
            layer: old.layer,
        })
    } else {
        None
    };

    // Add new item.
    equipment_index.insert(item.serial, mobile_serial);
    items.push(packets::world::EquippedItem {
        serial: item.serial,
        graphic: item.graphic,
        layer,
        color: if item.color != 0 { Some(item.color) } else { None },
    });

    // Emit EntityUpdated so other players see the equipment change.
    if let Some(e) = zone.store.get(mobile_serial) {
        let pos = EngineEntity::pos(e);
        let snap = e.snapshot();
        let _ = event_tx.send(WorldEvent::EntityUpdated {
            map_id: zone.map_id,
            serial: mobile_serial,
            pos,
            entity: snap,
        });
    }

    EquipResult::Ok { displaced }
}
