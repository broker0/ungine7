//! Weapon poisoning via the **Poisoning** skill.
//!
//! Flow:
//! 1. Player uses the Poisoning skill (handled in [`super::spells`]).
//! 2. A neutral target cursor asks for a poison bottle
//!    ([`begin_poison_skill`]).
//! 3. Player targets a poison potion — the level is read from its hue
//!    ([`handle_poison_bottle_target`]).
//! 4. A second target cursor asks for a fencing weapon.
//! 5. Player targets a fencing (piercing) weapon — equipped or in backpack.
//! 6. [`handle_poison_weapon_target`] verifies the weapon is fencing and starts
//!    a timed [`ActionPayload::Poisoning`] action.
//! 7. When the skill timer fires, [`apply_poison_to_weapon`] writes
//!    `poison_charges` / `poison_level` to the weapon's [`ItemProps`](common::uo_engine::item_props::ItemProps) meta,
//!    consumes the potion, and plays feedback.
//!
//! Poison charges are then consumed on each successful hit by the melee
//! combat code (see `crate::combat`), which applies poison to the victim.

use std::pin::Pin;
use std::time::Duration;

use log::info;
use tokio::time::Sleep;

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};
use packets::interaction::TargetCursor;

use network::error;
use network::session::Session;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::item_props::MetaValue;

use crate::actions::{ActionKind, ActionPayload, ActiveAction};
use crate::constants::{poison as poison_cfg, skill_timing, weapon};
use crate::game_util;
use crate::potions;
use crate::DemoWorkerTx;

use super::pending_cursor::{CursorKind, PendingCursor};
use super::session_state::SessionContext;

// ── Constants ────────────────────────────────────────────────────────────

/// Cursor ID base for the "select poison bottle" step.
const POISON_BOTTLE_CURSOR_BASE: u32 = 0x9014_0000;
/// Cursor ID base for the "select weapon" step.
const POISON_WEAPON_CURSOR_BASE: u32 = 0x9015_0000;

// ── Meta keys ──────────────────────────────────────────────────────────────

/// Meta key: number of remaining poison charges on a weapon.
pub(crate) const META_POISON_CHARGES: &str = "poison_charges";
/// Meta key: poison level (`1..=4`) the weapon applies on hit.
pub(crate) const META_POISON_LEVEL: &str = "poison_level";

/// Human-readable name for a poison level (`1..=4`).
pub(crate) fn level_name(level: u8) -> &'static str {
    match level {
        1 => "Lesser",
        2 => "Regular",
        3 => "Greater",
        4 => "Deadly",
        _ => "Unknown",
    }
}

// ── Step 1: select poison bottle ─────────────────────────────────────────

/// Begin the Poisoning skill: show a target cursor asking which poison bottle
/// to use.  Stores a [`CursorKind::PoisonSelectBottle`] pending cursor.
pub(super) async fn begin_poison_skill(
    ctx: &mut SessionContext,
    session: &mut Session,
) -> error::Result<()> {
    let Some(p) = &ctx.infra.player else {
        return Ok(());
    };

    let cursor_id = POISON_BOTTLE_CURSOR_BASE | (p.serial & 0x0000_FFFF);

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

    ctx.infra.pending_cursor = Some(PendingCursor::poison_select_bottle(cursor_id, p.serial));

    session.send(game_util::system_speech("Select the poison you wish to use.")).await?;
    session.send(RawPacket::s2c(encode_packet(&tc))).await?;
    Ok(())
}

/// Handle the "select poison bottle" target-cursor response (0x6C).
///
/// On success, shows a second cursor asking for the weapon to poison.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_poison_bottle_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::PoisonSelectBottle { user_serial } = pending.kind else {
        unreachable!("handle_poison_bottle_target called with non-PoisonSelectBottle cursor kind");
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
    let player_serial = p.serial;

    let bottle_serial = tc.target_serial & 0x7FFF_FFFF;
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Resolve the bottle's graphic + hue and check it is a poison potion.
    let level = match resolve_poison_level(&engine, player_serial, bottle_serial).await {
        Some(level) => level,
        None => {
            session.send(game_util::system_message(
                "That is not a poison potion.",
            )).await?;
            return Ok(true);
        }
    };

    // Show the second cursor — pick the weapon to coat.
    let cursor_id = POISON_WEAPON_CURSOR_BASE | (bottle_serial & 0x0000_FFFF);

    let weapon_tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 0,
        cursor_id,
        cursor_type: 0,
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    ctx.infra.pending_cursor = Some(PendingCursor::poison_select_weapon(
        cursor_id, level, bottle_serial, user_serial,
    ));

    session.send(game_util::system_speech("Which weapon do you wish to poison?")).await?;
    session.send(RawPacket::s2c(encode_packet(&weapon_tc))).await?;
    Ok(true)
}

// ── Step 2: select weapon ────────────────────────────────────────────────

/// Handle the "select weapon" target-cursor response (0x6C).
///
/// On success, starts a timed [`ActionPayload::Poisoning`] action; the poison
/// is actually applied when the skill timer fires (see [`apply_poison_to_weapon`]).
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_poison_weapon_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    skill_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::PoisonSelectWeapon { level, potion_serial, user_serial: _ } = pending.kind else {
        unreachable!("handle_poison_weapon_target called with non-PoisonSelectWeapon cursor kind");
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
    let player_serial = p.serial;

    let target_serial = tc.target_serial & 0x7FFF_FFFF;
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Resolve the targeted weapon's graphic.  It may be equipped on the
    // player or sitting in their backpack.
    let graphic = match resolve_weapon_graphic(&engine, player_serial, target_serial).await {
        Some(g) => g,
        None => {
            session.send(game_util::system_message("You can only poison a weapon.")).await?;
            return Ok(true);
        }
    };

    // Must be a fencing (piercing) weapon.
    let is_fencing = weapon::lookup_weapon(graphic)
        .map(|w| w.is_fencing())
        .unwrap_or(false);
    if !is_fencing {
        session.send(game_util::system_message(
            "Only fencing weapons (daggers, kryss, spears) can be poisoned.",
        )).await?;
        return Ok(true);
    }

    // Re-check the skill slot before committing to the timed action.
    if let Err(msg) = crate::actions::can_begin_skill(
        &ctx.active_skill, false, ctx.has_blocking_gump(),
    ) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Start the timed poisoning action.  Application happens on completion.
    let delay = Duration::from_millis(skill_timing::POISONING_DELAY_MS);
    let payload = ActionPayload::Poisoning {
        user_serial: player_serial,
        weapon_serial: target_serial,
        potion_serial,
        level,
        world,
    };
    let new_action = ActiveAction::new(ActionKind::SkillUse, delay, payload);
    skill_timer.as_mut().reset(new_action.completes_at);
    ctx.active_skill = Some(new_action);

    session.send(game_util::system_message("You carefully begin coating the weapon...")).await?;
    Ok(true)
}

// ── Completion: apply poison to the weapon ────────────────────────────────

/// Apply the poison to the weapon (called when the skill timer fires).
///
/// Writes the poison charges/level to the weapon's item props, consumes the
/// poison bottle, and plays feedback.
pub(super) async fn apply_poison_to_weapon(
    user_serial: u32,
    weapon_serial: u32,
    potion_serial: u32,
    level: u8,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Re-verify the weapon is still a valid fencing weapon.
    let graphic = resolve_weapon_graphic(&engine, user_serial, weapon_serial).await;
    let is_fencing = graphic
        .and_then(weapon::lookup_weapon)
        .map(|w| w.is_fencing())
        .unwrap_or(false);
    if !is_fencing {
        session.send(game_util::system_message("That object is no longer available.")).await?;
        return Ok(());
    }

    // Write the poison charges/level into the weapon's item props.
    let Some(idx) = poison_cfg::level_index(level) else {
        return Ok(());
    };
    let charges = poison_cfg::WEAPON_CHARGES[idx];

    let mut props = engine.get_item_props(weapon_serial).await.unwrap_or_default();
    props.set_meta(META_POISON_CHARGES, MetaValue::Int(charges as i64));
    props.set_meta(META_POISON_LEVEL, MetaValue::Int(level as i64));
    engine.set_item_props(weapon_serial, Some(props)).await;

    // Consume the poison potion now that application succeeded.
    let _ = engine.consume_item(potion_serial, 1, None).await;

    // Feedback.
    if let Some(m) = engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        game_util::send_sound(
            worker_tx, world, poison_cfg::APPLY_SOUND, m.x, m.y, m.z as i16,
        ).await;
    }
    session.send(game_util::system_message(
        &format!("You coat the weapon with poison ({} charges).", charges),
    )).await?;

    info!(
        "[poison] 0x{:08X} poisoned weapon 0x{:08X} (level {}, {} charges)",
        user_serial, weapon_serial, level, charges,
    );

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Resolve a targeted poison bottle to its poison level (`1..=4`).
///
/// Returns `None` if the target is not a poison potion.
///
/// The level is read from the bottle's per-instance `ItemProps.meta`
/// ([`META_POISON_LEVEL`]).  Bottles without a meta level (legacy / replay
/// data) fall back to level `1` (Lesser).
async fn resolve_poison_level(
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
    player_serial: u32,
    bottle_serial: u32,
) -> Option<u8> {
    // 1. In a container (backpack) — gives graphic + hue.
    let graphic = if let Some((_serial, graphic, _color, _amount)) =
        engine.find_item_info(bottle_serial).await
    {
        graphic
    } else if let Some(DemoEntity::Item { graphic, .. }) =
        engine.get_entity(bottle_serial).await
    {
        // 2. As a standalone item entity on the ground.
        graphic
    } else if let Some(eq) = engine
        .get_entity(player_serial)
        .await
        .as_ref()
        .and_then(|e| e.mobile())
        .and_then(|m| m.items.iter().find(|eq| eq.serial == bottle_serial).cloned())
    {
        // 3. Equipped on the player (unlikely for a potion, but be safe).
        eq.graphic
    } else {
        return None;
    };

    if !potions::is_poison_graphic(graphic) {
        return None;
    }

    // Level lives in per-instance meta; default to Lesser (1) if unset.
    let level = engine
        .get_item_props(bottle_serial)
        .await
        .and_then(|p| p.get_meta_int(META_POISON_LEVEL))
        .map(|v| v.clamp(1, 4) as u8)
        .unwrap_or(1);

    Some(level)
}

/// Resolve a targeted weapon's graphic, checking equipped items first, then
/// the player's backpack (container stores).  Returns `None` if the target
/// is not a weapon-bearing item.
async fn resolve_weapon_graphic(
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
    player_serial: u32,
    target_serial: u32,
) -> Option<u16> {
    // 1. Equipped on the player?
    if let Some(m) = engine.get_entity(player_serial).await.as_ref().and_then(|e| e.mobile()) {
        if let Some(eq) = m.items.iter().find(|eq| eq.serial == target_serial) {
            return Some(eq.graphic);
        }
    }

    // 2. In a container (backpack)?
    if let Some((_serial, graphic, _color, _amount)) = engine.find_item_info(target_serial).await {
        return Some(graphic);
    }

    // 3. As a standalone item entity?
    if let Some(DemoEntity::Item { graphic, .. }) = engine.get_entity(target_serial).await {
        return Some(graphic);
    }

    None
}
