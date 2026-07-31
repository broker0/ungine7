//! Mount / dismount via double-click.
//!
//! Flow:
//! - **Mount**: player double-clicks a mountable NPC within range.
//!   The NPC is removed from the world, its full entity data is saved
//!   in `ItemProps.meta` (serialized JSON), and a synthetic `EquippedItem`
//!   on `Layer::Mount` is equipped on the player.  The mount-item serial
//!   is allocated via [`SerialAllocator`] to avoid collisions with other
//!   items.
//!
//! - **Dismount**: player double-clicks themselves while mounted.
//!   The mount equipment item is unequipped, the saved NPC entity data
//!   is restored from `ItemProps.meta`, and the NPC is spawned back
//!   beside the player.  The original NPC serial is recovered from the
//!   saved entity data (not from bitwise arithmetic).

use log::{debug, info, warn};

use std::sync::Arc;

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};

use network::error;
use network::session::Session;

use packets::interaction::{DeleteObject, DoubleClick, EquipItem};
use packets::layer::Layer;
use packets::world::EquippedItem;

use common::uo_engine::entity::{DemoEntity, MobileData};
use common::uo_engine::item_props::{ItemProps, MetaValue};
use common::uo_engine::serial_alloc::SerialAllocator;

use crate::constants::mount as mount_cfg;
use crate::game_util::system_message;
use crate::DemoWorkerTx;

use super::PlayerState;

/// Meta key used to store the serialized NPC entity in `ItemProps`.
const META_NPC_ENTITY: &str = "mount_npc_entity";

/// Meta key used to store the original NPC serial (as a string).
const META_NPC_SERIAL: &str = "mount_npc_serial";

/// Meta key used to store the NPC's own serialized [`ItemProps`] (e.g. pet
/// ownership/command meta) so the "tamed" state survives mount → dismount.
const META_NPC_PROPS: &str = "mount_npc_props";

// ── Double-click intercept ───────────────────────────────────────────────

/// Check if a double-click should trigger mount or dismount.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_mount_double_click(
    packet: &RawPacket,
    player: &Option<PlayerState>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
) -> error::Result<bool> {
    if packet.id() != DoubleClick::ID {
        return Ok(false);
    }

    let dc = match DoubleClick::from_bytes(&packet.data) {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };

    // Explicit paperdoll request — let the normal handler deal with it.
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }

    let p = match player {
        Some(p) => p,
        None => return Ok(false),
    };

    let clean_serial = dc.serial & 0x7FFF_FFFF;

    // ── Dismount: double-click self while mounted ─────────────────────
    if clean_serial == p.serial {
        return handle_dismount(p, session, worker_tx, serial_alloc).await;
    }

    // ── Mount: double-click a mountable NPC ───────────────────────────
    handle_mount(clean_serial, p, session, worker_tx, serial_alloc).await
}

// ── Dismount ─────────────────────────────────────────────────────────────

/// Dismount: remove mount equipment, restore NPC in the world.
///
/// Returns `Ok(true)` if the player was mounted and dismount succeeded,
/// `Ok(false)` if not mounted (so the normal double-click handler runs).
async fn handle_dismount(
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
) -> error::Result<bool> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    // Check if the player is mounted.
    let entity = match engine.get_entity(player.serial).await {
        Some(e) => e,
        None => return Ok(false),
    };

    let mount_item = match entity.mobile() {
        Some(m) => {
            m.items.iter().find(|eq| eq.layer == Layer::Mount).cloned()
        }
        _ => None,
    };

    let mount_item = match mount_item {
        Some(m) => m,
        None => return Ok(false), // Not mounted — fall through to paperdoll.
    };

    let mount_item_serial = mount_item.serial;

    // Unequip the mount item from the player.
    let removed = engine.unequip_from_mobile(
        player.serial,
        mount_item_serial,
    )
    .await;

    if removed.is_none() {
        debug!("[mount] unequip failed for mount item 0x{:08X}", mount_item_serial);
        return Ok(true); // Consumed but failed.
    }

    // Send DeleteObject for the mount item so the client sees the dismount.
    session
        .send(RawPacket::s2c(encode_packet(&DeleteObject {
            id: DeleteObject::ID,
            serial: mount_item_serial,
        })))
        .await?;

    // Try to restore the original NPC from saved entity data.
    // If no saved data exists (e.g. mount from replay or test spawn),
    // create a default NPC from the mount-item graphic.
    let restored = restore_npc_from_props(
        worker_tx,
        player,
        mount_item_serial,
        serial_alloc,
    )
    .await;

    if restored.is_none() {
        // Fallback: create a default NPC from mount-item graphic.
        let fallback = spawn_default_mount_npc(
            worker_tx,
            player,
            &mount_item,
            serial_alloc,
        )
        .await;

        if let Some(npc_serial) = fallback {
            info!(
                "[mount] 0x{:08X} dismounted — spawned default NPC 0x{:08X} \
                 (no saved data, fallback from mount graphic 0x{:04X})",
                player.serial, npc_serial, mount_item.graphic,
            );
        } else {
            warn!(
                "[mount] 0x{:08X} dismounted — could not restore or create NPC \
                 (mount item 0x{:08X}, graphic 0x{:04X})",
                player.serial, mount_item_serial, mount_item.graphic,
            );
        }
    } else {
        info!(
            "[mount] 0x{:08X} dismounted — NPC 0x{:08X} restored from saved data",
            player.serial, restored.unwrap(),
        );
    }

    // Clean up item props for the mount item.
    engine.set_item_props(mount_item_serial, None).await;

    let msg = "You have dismounted.";
    session.send(system_message(msg)).await?;

    Ok(true)
}

/// Restore a previously saved NPC entity from `ItemProps.meta`.
///
/// Always allocates a fresh serial for the restored NPC to avoid
/// collisions with serials that may have been reused since the mount.
///
/// Returns `Some(npc_serial)` on success, `None` if no saved data was found.
async fn restore_npc_from_props(
    worker_tx: &DemoWorkerTx,
    player: &PlayerState,
    mount_item_serial: u32,
    serial_alloc: &Arc<SerialAllocator>,
) -> Option<u32> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    // Fetch the saved entity JSON from item props.
    let props = engine.get_item_props(mount_item_serial).await?;

    let json = match props.get_meta(META_NPC_ENTITY) {
        Some(MetaValue::Str(s)) => s.clone(),
        _ => return None,
    };

    let mut npc: DemoEntity = match serde_json::from_str(&json) {
        Ok(e) => e,
        Err(e) => {
            debug!("[mount] failed to deserialize saved NPC: {e}");
            return None;
        }
    };

    // Always allocate a fresh serial — the original NPC serial may have
    // been reused since the mount was created.
    let npc_serial = serial_alloc.alloc_mobile()
        .expect("mobile serial space exhausted");

    // Override the serial inside the deserialized entity.
    if let Some(m) = npc.mobile_mut() {
        m.serial = npc_serial;
    }

    // Place the NPC next to the player.
    place_npc_near_player(&mut npc, player);

    engine.spawn_entity(npc_serial, npc).await;

    // Restore the NPC's own ItemProps (pet ownership/command, name, …) under
    // the fresh serial, and re-attach the pet AI controller if it was tamed.
    // Without this, the "tamed" state would be lost across mount → dismount.
    let saved_props_json = match props.get_meta(META_NPC_PROPS) {
        Some(MetaValue::Str(s)) => Some(s.clone()),
        _ => None,
    };
    restore_npc_props(&engine, npc_serial, saved_props_json.as_deref()).await;
    reattach_pet_if_owned(worker_tx, player.world, npc_serial, saved_props_json.as_deref()).await;

    Some(npc_serial)
}

/// Restore a serialized [`ItemProps`] JSON blob onto `serial`.
///
/// Used to carry a mounted NPC's own item properties (e.g. pet ownership /
/// command meta) across the mount → dismount round-trip, where the NPC is
/// re-spawned under a fresh serial.
async fn restore_npc_props(
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
    serial: u32,
    props_json: Option<&str>,
) {
    let Some(json) = props_json else { return };
    match serde_json::from_str::<ItemProps>(json) {
        Ok(props) => engine.set_item_props(serial, Some(props)).await,
        Err(e) => debug!("[mount] failed to deserialize saved NPC props: {e}"),
    }
}

/// Re-attach the pet AI controller to a restored NPC if its saved props mark
/// it as a tamed pet (`pet_owner` meta present).
async fn reattach_pet_if_owned(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serial: u32,
    props_json: Option<&str>,
) {
    let Some(json) = props_json else { return };
    let Ok(props) = serde_json::from_str::<ItemProps>(json) else { return };
    if props.get_meta_int(crate::taming::META_PET_OWNER).is_none() {
        return;
    }

    let controller = Box::new(crate::controller_registry::PetController::new());
    let _ = worker_tx.send(framework::continuum::WorkerCommand::MapCommand(
        world,
        crate::DemoCommand::AttachControllerPersist {
            serial,
            controller,
            controller_id: crate::taming::PET_CONTROLLER_ID.to_string(),
        },
    )).await;
}

// ── Mount ────────────────────────────────────────────────────────────────

/// Mount: remove NPC from world, save its entity, equip mount item.
///
/// The mount-item serial is allocated via `serial_alloc` to prevent
/// collisions with other items (scrolls, reagents, etc.).
///
/// Returns `Ok(true)` if the target was a mountable NPC and was mounted,
/// `Ok(false)` if not mountable (so normal double-click runs).
async fn handle_mount(
    target_serial: u32,
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
) -> error::Result<bool> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    // Look up the target entity first — if it's not a mountable creature,
    // fall through immediately so containers, items, and other mobiles
    // are handled by the normal double-click path.
    let target = match engine.get_entity(target_serial).await {
        Some(e) => e,
        None => return Ok(false),
    };

    // Must be a mobile with a mountable body graphic.
    let (body, target_x, target_y) = match target.mobile() {
        Some(m) => (m.graphic, m.x, m.y),
        _ => return Ok(false),
    };

    let mount_def = match mount_cfg::body_to_mount(body) {
        Some(def) => def,
        None => return Ok(false), // Not a mountable creature — fall through.
    };

    // Target IS a mountable creature — from here on we consume the packet.

    // Check if the player is already mounted.
    let player_entity = match engine.get_entity(player.serial).await {
        Some(e) => e,
        None => return Ok(false),
    };

    let already_mounted = match player_entity.mobile() {
        Some(m) => {
            m.items.iter().any(|eq| eq.layer == Layer::Mount)
        }
        _ => return Ok(false),
    };

    if already_mounted {
        session.send(system_message("You are already mounted.")).await?;
        return Ok(true); // Consumed — was a mountable creature, but already riding.
    }

    // Range check.
    let dx = (player.x as i32 - target_x as i32).unsigned_abs() as u16;
    let dy = (player.y as i32 - target_y as i32).unsigned_abs() as u16;
    if dx > mount_cfg::MOUNT_RANGE || dy > mount_cfg::MOUNT_RANGE {
        session.send(system_message("That is too far away.")).await?;
        return Ok(true); // Consumed — was a mountable creature, just too far.
    }

    // Save the NPC entity data before removing it.
    let npc_json = match serde_json::to_string(&target) {
        Ok(j) => j,
        Err(e) => {
            debug!("[mount] failed to serialize NPC entity: {e}");
            return Ok(false);
        }
    };

    // Save the NPC's own ItemProps (pet ownership/command, custom name, …)
    // so the "tamed" state survives the mount → dismount round-trip.  These
    // props are keyed by the NPC's serial in the engine; on dismount the NPC
    // gets a fresh serial, so we must carry the props along ourselves.
    let npc_props_json = engine
        .get_item_props(target_serial)
        .await
        .and_then(|props| serde_json::to_string(&props).ok());

    // Remove the NPC from the world.
    engine.remove_entity(target_serial).await;
    // Drop the NPC's now-orphaned props (keyed by the old serial).
    engine.set_item_props(target_serial, None).await;

    // Create mount equipment item.
    let mount_item_serial = match serial_alloc.alloc_item() {
        Some(s) => s,
        None => {
            warn!("[mount] item serial space exhausted — cannot create mount item");
            // Re-spawn the NPC since we can't mount.
            engine.spawn_entity(target_serial, target).await;
            restore_npc_props(&engine, target_serial, npc_props_json.as_deref()).await;
            return Ok(true);
        }
    };
    let mount_item = EquippedItem {
        serial: mount_item_serial,
        graphic: mount_def.mount_graphic,
        layer: Layer::Mount,
        color: target.mobile().and_then(|m| {
            if m.color != 0 { Some(m.color) } else { None }
        }),
    };

    // Equip the mount on the player.
    let ok = engine.equip_on_mobile(
        player.serial,
        mount_item.clone(),
    )
    .await;

    if !ok {
        debug!("[mount] equip failed — respawning NPC");
        // Re-spawn the NPC since equip failed.
        engine.spawn_entity(target_serial, target).await;
        restore_npc_props(&engine, target_serial, npc_props_json.as_deref()).await;
        return Ok(true);
    }

    // Send EquipItem to the acting client.
    let eq_pkt = EquipItem {
        id: EquipItem::ID,
        item_serial: mount_item_serial,
        graphic: mount_def.mount_graphic,
        _pad0: (),
        layer: Layer::Mount,
        player_serial: player.serial,
        color: mount_item.color.unwrap_or(0),
    };
    session.send(RawPacket::s2c(encode_packet(&eq_pkt))).await?;

    // Save the NPC entity data and serial in item props for later dismount.
    let mut props = ItemProps::with_name(mount_def.name);
    props.set_meta(META_NPC_ENTITY, MetaValue::Str(npc_json));
    props.set_meta(META_NPC_SERIAL, MetaValue::Str(target_serial.to_string()));
    if let Some(npc_props_json) = npc_props_json {
        props.set_meta(META_NPC_PROPS, MetaValue::Str(npc_props_json));
    }
    engine.set_item_props(mount_item_serial, Some(props)).await;

    info!(
        "[mount] 0x{:08X} mounted NPC 0x{:08X} (body 0x{:04X} → mount 0x{:04X})",
        player.serial, target_serial, body, mount_def.mount_graphic,
    );
    session.send(system_message("You have mounted.")).await?;

    Ok(true)
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Create and spawn a default NPC from the mount-item graphic when no
/// saved entity data is available (e.g. mount from replay or test spawn).
///
/// Returns `Some(npc_serial)` on success, `None` if the mount graphic
/// is not recognised.
async fn spawn_default_mount_npc(
    worker_tx: &DemoWorkerTx,
    player: &PlayerState,
    mount_item: &EquippedItem,
    serial_alloc: &Arc<SerialAllocator>,
) -> Option<u32> {
    use packets::mobile_flags::MobileFlags;
    use packets::movement::Notoriety;

    let def = mount_cfg::mount_graphic_to_mount(mount_item.graphic)?;

    let npc_serial = serial_alloc.alloc_mobile()
        .expect("mobile serial space exhausted");

    let mut npc = DemoEntity::Mobile(MobileData {
        serial: npc_serial,
        graphic: def.body,
        x: player.x.wrapping_add(1),
        y: player.y,
        z: player.z,
        direction: 0,
        color: mount_item.color.unwrap_or(0),
        status: MobileFlags(0),
        notoriety: Notoriety::Attackable,
        items: Vec::new(),
        name: def.name.to_string(),
        hits: 50,
        hits_max: 50,
        mana: 0,
        mana_max: 0,
        stamina: 50,
        stamina_max: 50,
        str_: 50,
        dex: 50,
        int: 10,
        is_player: false,
        dead: false,
        living_graphic: 0,
        noto_class: common::uo_engine::notoriety::NotorietyClass::Neutral,
        ..Default::default()
    });

    place_npc_near_player(&mut npc, player);
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    engine.spawn_entity(npc_serial, npc).await;
    Some(npc_serial)
}

/// Place an NPC entity at a position adjacent to the player.
///
/// Tries to offset by +1 X; if that's the same tile, uses player position.
fn place_npc_near_player(npc: &mut DemoEntity, player: &PlayerState) {
    let (nx, ny) = (player.x.wrapping_add(1), player.y);
    if let Some(m) = npc.mobile_mut() {
        m.x = nx;
        m.y = ny;
        m.z = player.z;
    }
}
