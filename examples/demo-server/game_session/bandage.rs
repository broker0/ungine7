//! Bandage healing: double-click bandages → select target → timed heal.
//!
//! Flow:
//! 1. Player double-clicks a bandage item (graphic 0x0E21) in backpack or on ground nearby.
//! 2. A target cursor is shown (helpful).
//! 3. Player selects a mobile target within 2 tiles and LOS.
//! 4. A 2-second `ActionKind::Bandage` action begins.
//! 5. On completion, distance/LOS are re-checked, HP is healed, and one bandage is consumed.

use std::pin::Pin;

use log::{debug, info};

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};

use network::error;
use network::session::Session;

use packets::interaction::{DoubleClick, TargetCursor};

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::rpc::EngineProxy;

use tokio::time::Sleep;

use crate::actions::{self, ActionKind, ActionPayload, ActiveAction};
use crate::constants::{bandage as bandage_cfg, item};
use crate::game_util;
use crate::{DemoCommand, DemoWorkerTx};

use super::pending_cursor::PendingCursor;
use super::session_state::SessionContext;

// ── Constants ────────────────────────────────────────────────────────────

/// Cursor ID for bandage targeting (distinct from spell and skill cursors).
const BANDAGE_CURSOR_BASE: u32 = 0xBA9D_0000;

// ── Double-click intercept ───────────────────────────────────────────────

/// Check if a double-click packet targets a bandage item.
///
/// If it does, verify the bandage is accessible (in a container / backpack,
/// or on the ground within range), then send a target cursor and populate
/// `ctx.pending_bandage`.
///
/// Returns `true` if the packet was consumed (regardless of success).
pub(super) async fn handle_bandage_double_click(
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

    // Paperdoll request (high bit) — not a bandage.
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }

    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let p = match &ctx.infra.player {
        Some(p) => p,
        None => return Ok(false),
    };

    // First, try to find the item in container stores (backpack).
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let item_info = engine.find_item_info(clean_serial).await;

    let is_bandage = match &item_info {
        Some((_serial, graphic, _color, _amount)) => *graphic == item::BANDAGE,
        None => {
            // Not in a container — check as a ground entity.
            match engine.get_entity(clean_serial).await {
                Some(DemoEntity::Item { graphic, x, y, .. }) => {
                    if graphic != item::BANDAGE {
                        return Ok(false);
                    }
                    // Check distance to ground item.
                    let dx = (p.x as i32 - x as i32).unsigned_abs() as u16;
                    let dy = (p.y as i32 - y as i32).unsigned_abs() as u16;
                    if dx > bandage_cfg::RANGE || dy > bandage_cfg::RANGE {
                        session.send(game_util::system_message("That is too far away.")).await?;
                        return Ok(true);
                    }
                    true
                }
                _ => return Ok(false),
            }
        }
    };

    if !is_bandage {
        return Ok(false);
    }

    // Check bandage slot blocking before showing cursor.
    let has_pending = ctx.has_pending_cursor();
    if let Err(msg) = actions::can_begin_bandage(&ctx.active_bandage, has_pending) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Send target cursor (helpful / cursor_type = 0 neutral).
    let cursor_id = BANDAGE_CURSOR_BASE | (clean_serial & 0x0000_FFFF);

    let tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 0, // object target
        cursor_id,
        cursor_type: 2, // helpful
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    ctx.infra.pending_cursor = Some(PendingCursor::bandage(
        cursor_id, p.serial, clean_serial,
    ));

    session.send(game_util::system_speech("Who would you like to heal?")).await?;
    session.send(RawPacket::s2c(encode_packet(&tc))).await?;
    Ok(true)
}

// ── Target cursor response ───────────────────────────────────────────────

/// Handle a bandage target-cursor response (0x6C).
///
/// The caller must have already taken a `PendingCursor` with
/// `CursorKind::Bandage` and verified the cursor ID matches.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_bandage_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    bandage_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let super::pending_cursor::CursorKind::Bandage {
        healer_serial, bandage_item_serial,
    } = pending.kind else {
        unreachable!("handle_bandage_target called with non-Bandage cursor kind");
    };

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(tc) => tc,
        Err(_) => return Ok(false),
    };

    // Cancelled by the player.
    if tc.cursor_type == 3 || tc.target_serial == 0 {
        return Ok(true);
    }

    let p = match ctx.infra.player.as_ref() {
        Some(p) => p,
        None => return Ok(true),
    };

    let target_serial = tc.target_serial;

    // Target must be a mobile.
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let target = engine.get_entity(target_serial).await;
    let (tx, ty, tz, target_hp, target_max_hp) = match target.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y, m.z, m.hits, m.hits_max),
        _ => {
            session.send(game_util::system_message("You can only use bandages on a living creature.")).await?;
            return Ok(true);
        }
    };

    // If target is already at full HP, cancel immediately.
    if target_hp >= target_max_hp {
        session.send(game_util::system_speech("The patient seems to be quite all right")).await?;
        return Ok(true);
    }

    // Distance check.
    let dx = (p.x as i32 - tx as i32).unsigned_abs() as u16;
    let dy = (p.y as i32 - ty as i32).unsigned_abs() as u16;
    if dx > bandage_cfg::RANGE || dy > bandage_cfg::RANGE {
        session.send(game_util::system_message("That is too far away.")).await?;
        return Ok(true);
    }

    // LOS check.
    if !engine.check_los(
        p.x, p.y, p.z as i16 + crate::constants::EYE_HEIGHT,
        tx, ty, tz as i16 + crate::constants::EYE_HEIGHT,
    ).await {
        session.send(game_util::system_message("Target cannot be seen.")).await?;
        return Ok(true);
    }

    // Re-check bandage slot (something may have started while cursor was open).
    // has_pending=false because this IS the pending cursor being resolved.
    if let Err(msg) = actions::can_begin_bandage(&ctx.active_bandage, false) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    let delay = std::time::Duration::from_millis(bandage_cfg::DELAY_MS);
    let payload = ActionPayload::Bandage {
        healer_serial,
        target_serial,
        bandage_item_serial,
        world: p.world,
    };

    let new_action = ActiveAction::new(ActionKind::Bandage, delay, payload);
    bandage_timer.as_mut().reset(new_action.completes_at);
    ctx.active_bandage = Some(new_action);
    debug!(
        "[bandage] 0x{:08X} began bandaging 0x{:08X}",
        healer_serial, target_serial,
    );

    Ok(true)
}

// ── Action completion ────────────────────────────────────────────────────

/// Complete a bandage action: re-check distance/LOS, heal target, consume
/// one bandage from the item stack.
pub(super) async fn complete_bandage(
    healer_serial: u32,
    target_serial: u32,
    bandage_item_serial: u32,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // 1. Get healer position.
    let healer = engine.get_entity(healer_serial).await;
    let (hx, hy, hz) = match healer.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y, m.z),
        _ => {
            session.send(game_util::system_message("You are unable to apply bandages.")).await?;
            return Ok(());
        }
    };

    // 2. Get target info.
    let target = engine.get_entity(target_serial).await;
    let (tx, ty, tz, target_hp, target_max_hp) = match target.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y, m.z, m.hits, m.hits_max),
        _ => {
            session.send(game_util::system_message("Your target is no longer there.")).await?;
            return Ok(());
        }
    };

    // If target is already at full HP at completion, don't waste a bandage.
    if target_hp >= target_max_hp {
        session.send(game_util::system_speech("The patient seems to be quite all right")).await?;
        return Ok(());
    }

    // 3. Distance re-check.
    let dx = (hx as i32 - tx as i32).unsigned_abs() as u16;
    let dy = (hy as i32 - ty as i32).unsigned_abs() as u16;
    if dx > bandage_cfg::RANGE || dy > bandage_cfg::RANGE {
        session.send(game_util::system_message("You cannot reach the target.")).await?;
        return Ok(());
    }

    // 4. LOS re-check (skip for self-bandage).
    if healer_serial != target_serial {
        if !engine.check_los(
            hx, hy, hz as i16 + crate::constants::EYE_HEIGHT,
            tx, ty, tz as i16 + crate::constants::EYE_HEIGHT,
        ).await {
            session.send(game_util::system_message("Target cannot be seen.")).await?;
            return Ok(());
        }
    }

    // 5. Consume one bandage.
    let bandage_consumed = consume_one_bandage(&engine, bandage_item_serial).await;
    if !bandage_consumed {
        session.send(game_util::system_message("You have no bandages left.")).await?;
        return Ok(());
    }

    // 6. Heal.
    let heal_amount = game_util::random_range(bandage_cfg::HEAL_MIN, bandage_cfg::HEAL_MAX);
    let _new_hp = engine.heal(target_serial, heal_amount).await;

    // 7. Sound effect at target position.
    game_util::send_sound(worker_tx, world, bandage_cfg::SOUND, tx, ty, tz as i16).await;

    // 8. Completion message.
    session.send(game_util::system_speech("You place a bloody bandage in your backpack")).await?;

    info!(
        "[bandage] 0x{:08X} healed 0x{:08X} for {} HP",
        healer_serial, target_serial, heal_amount,
    );

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Try to consume one bandage from the given item (in container or on ground).
///
/// Uses the atomic `ConsumeItem` engine command — no read-then-write race.
///
/// Returns `true` if a bandage was consumed.
async fn consume_one_bandage(
    engine: &EngineProxy<DemoCommand>,
    bandage_item_serial: u32,
) -> bool {
    engine.consume_item(
        bandage_item_serial,
        1,                          // consume 1 unit
        Some(item::BANDAGE),        // must be a bandage graphic
    ).await.is_some()
}
