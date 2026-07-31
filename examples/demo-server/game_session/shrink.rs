//! Shrink potion: turn a tamed animal into a carryable statue and back.
//!
//! Flow:
//! 1. Player double-clicks a shrink potion in their backpack
//!    (handled in [`super::potions`], which delegates to [`begin_shrink`]).
//! 2. A neutral target cursor asks which animal to shrink.
//! 3. Player targets one of their **own** tamed animals
//!    ([`handle_shrink_target`]):
//!    - the creature's body graphic / name / hue are recorded into a new
//!      statue item's [`ItemProps`] meta,
//!    - the statue is placed in the player's backpack,
//!    - the shrink potion is consumed,
//!    - the live creature is removed from the world.
//! 4. Double-clicking the statue ([`handle_statue_double_click`]) re-spawns
//!    the stored creature next to the player as their pet (follow) and
//!    consumes the statue.
//!
//! The statue is allocated a *unique* item serial so it never stacks with
//! another statue — each one carries its own creature data.

use log::info;

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};
use packets::interaction::{DoubleClick, TargetCursor};

use network::error;
use network::session::Session;

use common::uo_engine::entity::{DemoEntity, MobileData};
use common::uo_engine::handler::{DropResult, DropTarget, HeldItemInfo};
use common::uo_engine::item_props::{ItemProps, MetaValue};
use common::uo_engine::notoriety::NotorietyClass;

use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;
use u_core::Heading;

use framework::continuum::WorkerCommand;
use framework::ecumene::Entity as EngineEntity;

use crate::constants::{item, shrink as shrink_cfg};
use crate::game_util;
use crate::taming;
use crate::{DemoCommand, DemoWorkerTx};

use super::pending_cursor::{CursorKind, PendingCursor};
use super::session_state::SessionContext;

// ── Constants ──────────────────────────────────────────────────────────────

/// Cursor ID base for the "select animal to shrink" step.
const SHRINK_CURSOR_BASE: u32 = 0x9016_0000;

// ── Meta keys (stored on the statue item) ────────────────────────────────────

/// Meta key: body graphic of the shrunken creature.
const META_SHRUNK_BODY: &str = "shrunk_body";
/// Meta key: display name of the shrunken creature.
const META_SHRUNK_NAME: &str = "shrunk_name";
/// Meta key: hue/color of the shrunken creature.
const META_SHRUNK_HUE: &str = "shrunk_hue";

// ── Step 1: begin shrink (double-click potion) ───────────────────────────────

/// Begin the shrink potion: show a target cursor asking which animal to
/// shrink.  Stores a [`CursorKind::ShrinkSelectAnimal`] pending cursor.
///
/// The potion is **not** consumed here — only on a successful shrink.
pub(super) async fn begin_shrink(
    potion_serial: u32,
    ctx: &mut SessionContext,
    session: &mut Session,
) -> error::Result<()> {
    let Some(p) = &ctx.infra.player else {
        return Ok(());
    };
    let player_serial = p.serial;

    let cursor_id = SHRINK_CURSOR_BASE | (potion_serial & 0x0000_FFFF);

    let tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 0, // object target
        cursor_id,
        cursor_type: 0, // neutral
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    ctx.infra.pending_cursor = Some(PendingCursor::shrink_select_animal(
        cursor_id, player_serial, potion_serial,
    ));

    session.send(game_util::system_speech("Select the animal you wish to shrink.")).await?;
    session.send(RawPacket::s2c(encode_packet(&tc))).await?;
    Ok(())
}

// ── Step 2: select animal (target-cursor response) ───────────────────────────

/// Handle the "select animal" target-cursor response (0x6C).
///
/// On success, records the creature data into a new statue item, places it
/// in the backpack, consumes the potion, and removes the live creature.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_shrink_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::ShrinkSelectAnimal { user_serial, potion_serial } = pending.kind else {
        unreachable!("handle_shrink_target called with non-ShrinkSelectAnimal cursor kind");
    };

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(t) => t,
        Err(_) => return Ok(true),
    };

    // Cancelled by the client.
    if common::dot_commands::is_target_cancelled(&tc) || tc.target_serial == 0 {
        session.send(game_util::system_message("Target cancelled.")).await?;
        return Ok(true);
    }

    let Some(p) = &ctx.infra.player else {
        return Ok(true);
    };
    let world = p.world;

    let target_serial = tc.target_serial & 0x7FFF_FFFF;
    let engine = game_util::engine_for(worker_tx, world);

    // The target must be a non-player mobile.
    let target = engine.get_entity(target_serial).await;
    let (graphic, color, name, tx, ty) = match target.as_ref() {
        Some(DemoEntity::Mobile(m)) if !m.is_player => {
            (m.graphic, m.color, m.name.clone(), m.x, m.y)
        }
        _ => {
            session.send(game_util::system_message("You can't shrink that.")).await?;
            return Ok(true);
        }
    };

    // Must be a known tameable creature.
    if taming::lookup_tameable(graphic).is_none() {
        session.send(game_util::system_message("You can only shrink tamed animals.")).await?;
        return Ok(true);
    }

    // Must be one of the player's *own* pets.
    let props = engine.get_item_props(target_serial).await.unwrap_or_default();
    if props.get_meta_int(taming::META_PET_OWNER) != Some(user_serial as i64) {
        session.send(game_util::system_message("You can only shrink your own pets.")).await?;
        return Ok(true);
    }

    // Range re-check (the user may have walked off during targeting).
    let (ux, uy) = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y),
        None => return Ok(true),
    };
    if game_util::chebyshev(ux, uy, tx, ty) > shrink_cfg::RANGE {
        session.send(game_util::system_message("That is too far away.")).await?;
        return Ok(true);
    }

    // Resolve the player's backpack.
    let bp_serial = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.backpack_serial()) {
        Some(s) => s,
        None => {
            session.send(game_util::system_message("You have no backpack to hold the statue.")).await?;
            return Ok(true);
        }
    };

    // Allocate a unique item serial for the statue (must not stack).
    let Some(statue_serial) = ctx.serial_alloc.alloc_item() else {
        session.send(game_util::system_message("You cannot hold any more right now.")).await?;
        return Ok(true);
    };

    // Drop the statue into the backpack.
    let held = HeldItemInfo {
        serial: statue_serial,
        graphic: item::SHRINK_STATUE,
        color,
        amount: 1,
    };
    let target = DropTarget::OnEntity {
        target_serial: bp_serial,
        x: 0xFFFF,
        y: 0xFFFF,
    };
    let result = engine.drop_item(user_serial, held, target, None).await;
    let placed_serial = match result {
        DropResult::DroppedInContainer { serial, .. } => serial,
        DropResult::FallbackGround { serial } | DropResult::DroppedOnGround { serial } => serial,
        other => {
            info!("[shrink] failed to place statue for 0x{:08X}: {:?}", user_serial, other);
            session.send(game_util::system_message("You cannot hold the statue right now.")).await?;
            return Ok(true);
        }
    };

    // Record the creature data on the statue's item props.
    let statue_name = format!("a shrunken {}", strip_article(&name));
    let mut statue_props = ItemProps::with_name(&statue_name);
    statue_props.set_meta(META_SHRUNK_BODY, MetaValue::Int(graphic as i64));
    statue_props.set_meta(META_SHRUNK_NAME, MetaValue::Str(name.clone()));
    statue_props.set_meta(META_SHRUNK_HUE, MetaValue::Int(color as i64));
    engine.set_item_props(placed_serial, Some(statue_props)).await;

    // Consume the shrink potion now that the shrink succeeded.
    let _ = engine.consume_item(potion_serial, 1, Some(item::POTION_SHRINK)).await;

    // Remove the live creature from the world (broadcasts EntityRemoved →
    // every observer, including this client, deletes it).
    engine.remove_entity(target_serial).await;

    // Feedback.
    if let Some(m) = engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        game_util::send_sound(
            worker_tx, world, crate::constants::potion::DRINK_SOUND,
            m.x, m.y, m.z as i16,
        ).await;
    }
    session.send(game_util::system_message(
        &format!("You shrink {} into a statue.", name),
    )).await?;

    info!(
        "[shrink] 0x{:08X} shrank creature 0x{:08X} (graphic={:#06X}) into statue 0x{:08X}",
        user_serial, target_serial, graphic, placed_serial,
    );

    Ok(true)
}

// ── Statue double-click: unshrink ────────────────────────────────────────────

/// Check if a double-click packet targets a shrunken-animal statue.
///
/// If it carries the `shrunk_body` meta, re-spawn the stored creature next to
/// the player as their pet and consume the statue.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_statue_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if packet.id() != DoubleClick::ID {
        return Ok(false);
    }
    let dc = match DoubleClick::from_bytes(&packet.data) {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };
    // Paperdoll request (high bit) — not a statue.
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }
    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let (player_serial, world, px, py, pz) = match &ctx.infra.player {
        Some(p) => (p.serial, p.world, p.x, p.y, p.z),
        None => return Ok(false),
    };

    let engine = game_util::engine_for(worker_tx, world);

    // Quick check: is this item a statue and does it carry shrunk data?
    let item_info = engine.find_item_info(clean_serial).await;
    let is_statue_graphic = match &item_info {
        Some((_s, graphic, _c, _a)) => *graphic == item::SHRINK_STATUE,
        None => matches!(
            engine.get_entity(clean_serial).await,
            Some(DemoEntity::Item { graphic, .. }) if graphic == item::SHRINK_STATUE
        ),
    };
    if !is_statue_graphic {
        return Ok(false);
    }

    let props = match engine.get_item_props(clean_serial).await {
        Some(p) => p,
        None => return Ok(false),
    };
    let Some(body) = props.get_meta_int(META_SHRUNK_BODY) else {
        // A statue graphic without shrink data — not ours.
        return Ok(false);
    };
    let body = body as u16;
    let hue = props.get_meta_int(META_SHRUNK_HUE).unwrap_or(0) as u16;
    let creature_name = props
        .get_meta_str(META_SHRUNK_NAME)
        .map(|s| s.to_string())
        .or_else(|| taming::lookup_tameable(body).map(|d| d.name.to_string()))
        .unwrap_or_else(|| "a creature".to_string());

    // Pick a tile next to the player to place the creature.
    let nx = px.wrapping_add(1);
    let ny = py;
    let nz = engine.resolve_z(nx, ny, pz, Heading::South).await.unwrap_or(pz);

    let npc_serial = engine.allocate_mobile_serial().await;
    if npc_serial == 0 {
        session.send(game_util::system_message("There is no room to release the animal.")).await?;
        return Ok(true);
    }

    let npc = DemoEntity::Mobile(MobileData {
        serial: npc_serial,
        graphic: body,
        x: nx,
        y: ny,
        z: nz,
        direction: 0,
        color: hue,
        status: MobileFlags(0),
        notoriety: Notoriety::Innocent,
        items: Vec::new(),
        name: creature_name.clone(),
        hits: 100,
        hits_max: 100,
        mana: 0,
        mana_max: 0,
        stamina: 100,
        stamina_max: 100,
        str_: 80,
        dex: 60,
        int: 30,
        is_player: false,
        dead: false,
        living_graphic: 0,
        noto_class: NotorietyClass::Innocent,
        ..Default::default()
    });
    engine.spawn_entity(npc_serial, npc).await;

    // Restore pet ownership + default command (follow).
    let mut pet_props = ItemProps::with_name(&creature_name);
    pet_props.set_meta(taming::META_PET_OWNER, MetaValue::Int(player_serial as i64));
    pet_props.set_meta(taming::META_PET_COMMAND, MetaValue::Str(taming::CMD_FOLLOW.to_string()));
    engine.set_item_props(npc_serial, Some(pet_props)).await;

    // Attach the pet AI controller.
    let controller = Box::new(crate::controller_registry::PetController::new());
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        world,
        DemoCommand::AttachControllerPersist {
            serial: npc_serial,
            controller,
            controller_id: taming::PET_CONTROLLER_ID.to_string(),
        },
    )).await;

    // Consume the statue item now that the creature is back.
    let _ = engine.consume_item(clean_serial, 1, Some(item::SHRINK_STATUE)).await;

    session.send(game_util::system_speech(
        &format!("{} returns to your side.", creature_name),
    )).await?;

    info!(
        "[shrink] 0x{:08X} released creature 0x{:08X} (graphic={:#06X}) from statue 0x{:08X}",
        player_serial, npc_serial, body, clean_serial,
    );

    Ok(true)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Strip a leading indefinite article from a creature name so it can be
/// re-articled (e.g. `"a horse"` → `"horse"`).
fn strip_article(name: &str) -> &str {
    for prefix in ["a ", "an ", "A ", "An "] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}
