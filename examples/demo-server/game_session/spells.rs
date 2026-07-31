//! Spell and skill handling extracted from the main session loop.
//!
//! Processes `TextCommand` (0x12) and `GeneralInfo` (0xBF) spell sub-commands,
//! as well as spell/skill target-cursor responses.
//!
//! Spell casts use the **cast slot** (`active_cast`): [`begin_cast`](crate::magic::begin_cast) consumes
//! mana and plays the cast animation immediately, then an [`ActiveAction`] is
//! stored with a timer for [`SpellDef::cast_delay`](crate::magic::SpellDef::cast_delay_ms).  When the timer fires,
//! [`complete_cast`](crate::magic::complete_cast) resolves the spell effect.
//!
//! Skills use the **skill slot** (`active_skill`): a target cursor is shown
//! first, then the action timer fires after a cooldown delay.
//!
//! Cast and skill slots are independent — a skill can run in parallel with a
//! cast, but a cast is blocked by an active skill (see [`actions::can_begin_cast`]).

use std::pin::Pin;
use std::time::Duration;

use log::{debug, info};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use packets::action::TextCommand;
use packets::interaction::TargetCursor;

use framework::continuum::WorkerCommand;
use tokio::time::Sleep;

use crate::actions::{self, ActiveAction, ActionKind, ActionPayload};
use crate::constants::{anim, skill_id, skill_timing};
use crate::{DemoCommand, DemoWorkerTx};

use super::PlayerState;
use super::pending_cursor::{CursorKind, PendingCursor};
use super::session_state::SessionContext;

// ── Fizzle effect ────────────────────────────────────────────────────────

/// Send a spell-blocked fizzle: system message + sound + visual effect.
///
/// `msg` is the block reason (e.g. "You are already casting a spell.").
async fn send_spell_fizzle(
    msg: &str,
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    session.send(crate::game_util::system_speech(msg)).await?;
    crate::game_util::send_fizzle(
        worker_tx, player.world, player.serial,
        player.x, player.y, player.z,
    ).await;
    Ok(())
}

// ── Skill targeting ──────────────────────────────────────────────────────

/// Cursor ID base for skill targeting (distinct from spell cursors).
const SKILL_CURSOR_BASE: u32 = 0xBEEF_0000;

/// Create a target cursor for a skill and return the pending state + packet.
fn begin_skill_target(
    sid: u16,
    user_serial: u32,
) -> (PendingCursor, RawPacket) {
    let cursor_id = SKILL_CURSOR_BASE | (sid as u32);

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

    let pending = PendingCursor::skill(cursor_id, sid, user_serial);

    (pending, RawPacket::s2c(encode_packet(&tc)))
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Try to start a spell-cast action.  On success, stores it in
/// `ctx.active_cast` and resets `cast_timer`.  On failure, sends a fizzle
/// to the client.
///
/// The caller must have already checked [`actions::can_begin_cast`] before
/// showing a target cursor.  This function performs begin_cast (mana,
/// LOS, animation) and only stores the action if phase 1 succeeds.
///
/// Casting a spell interrupts meditation.
///
/// `scroll_item_serial`: if `Some`, the cast was initiated from a scroll;
/// mana is not consumed and the scroll will be consumed on completion.
async fn try_start_cast(
    spell_def: &'static crate::magic::SpellDef,
    caster_serial: u32,
    target_serial: u32,
    world: u8,
    ctx: &mut SessionContext,
    cast_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    scroll_item_serial: Option<u32>,
) -> error::Result<()> {
    let from_scroll = scroll_item_serial.is_some();

    // 1. Phase 1: consume mana, check LOS, play animation + spell words.
    let result = crate::magic::begin_cast(
        spell_def, caster_serial, target_serial, worker_tx, world, from_scroll,
    ).await;

    for pkt in result.packets {
        session.send(pkt).await?;
    }

    if !result.ok {
        // Mana check or LOS failed — don't start action.
        return Ok(());
    }

    // 2. Create and store the active action.
    let payload = ActionPayload::SpellCast {
        spell: spell_def,
        caster_serial,
        target_serial,
        world,
        scroll_item_serial,
    };
    let delay = spell_def.effective_cast_delay(from_scroll);
    let new_action = ActiveAction::new(ActionKind::SpellCast, delay, payload);
    cast_timer.as_mut().reset(new_action.completes_at);
    ctx.active_cast = Some(new_action);

    // Meditation interruption is handled centrally in the session loop
    // (any action packet, including 0x6C / 0x12 / 0xBF, stops meditation
    // before reaching this point).

    Ok(())
}

// ── Packet handlers ───────────────────────────────────────────────────────

/// Handle a spell target-cursor response (0x6C).
///
/// The caller must have already taken a `PendingCursor` with
/// `CursorKind::Spell` and verified the cursor ID matches.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_spell_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    cast_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::Spell { spell, caster_serial, scroll_item_serial } = pending.kind else {
        unreachable!("handle_spell_target called with non-Spell cursor kind");
    };

    if let Ok(tc) = TargetCursor::from_bytes(&packet.data) {
        if tc.cursor_type == 3 || tc.target_serial == 0 {
            // Cancelled by the player.
        } else {
            let p = ctx.infra.player.as_ref().unwrap();
            let world = p.world;
            try_start_cast(
                spell,
                caster_serial,
                tc.target_serial,
                world,
                ctx,
                cast_timer,
                session,
                worker_tx,
                scroll_item_serial,
            ).await?;
        }
        return Ok(true);
    }

    Ok(false)
}

/// Handle the **ground** target-cursor response for the Wall of Stone spell.
///
/// Unlike [`handle_spell_target`], the response carries a tile (`x`/`y`/`z`)
/// rather than a `target_serial`.  On success this performs phase 1
/// (`begin_cast`) and stores an [`ActionPayload::WallOfStone`] in the cast
/// slot; the wall is spawned when the cast timer fires.
pub(super) async fn handle_wall_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    cast_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::Spell { spell, caster_serial, .. } = pending.kind else {
        unreachable!("handle_wall_target called with non-Spell cursor kind");
    };

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(tc) => tc,
        Err(_) => return Ok(false),
    };
    if common::dot_commands::is_target_cancelled(&tc) {
        return Ok(true);
    }

    let Some(p) = ctx.infra.player.as_ref() else {
        return Ok(true);
    };
    let world = p.world;
    let (px, py, pz) = (p.x, p.y, p.z);

    // Line-of-sight check: the caster must be able to see the chosen tile.
    // The source is the caster's eyes; the destination is the ground tile
    // itself (no eye-height offset — we are aiming at the ground).
    let engine = crate::game_util::engine_for(worker_tx, world);
    const EYE_H: i16 = crate::constants::EYE_HEIGHT;
    if !engine.check_los(
        px, py, pz as i16 + EYE_H,
        tc.x, tc.y, tc.z as i16,
    ).await {
        session.send(crate::game_util::system_speech("Target cannot be seen.")).await?;
        return Ok(true);
    }

    // Phase 1: consume nothing yet — check mana / reagents, play spell words +
    // cast animation.  LOS to the tile was already verified above; we pass the
    // caster as its own target so `begin_cast` skips its entity-LOS path.
    let result = crate::magic::begin_cast(
        spell, caster_serial, caster_serial, worker_tx, world, false,
    ).await;
    for pkt in result.packets {
        session.send(pkt).await?;
    }
    if !result.ok {
        return Ok(true);
    }

    // Store the timed wall-spawn action in the cast slot.
    let payload = ActionPayload::WallOfStone {
        caster_serial,
        target_x: tc.x,
        target_y: tc.y,
        target_z: tc.z,
        world,
    };
    let delay = spell.cast_delay();
    let new_action = ActiveAction::new(ActionKind::SpellCast, delay, payload);
    cast_timer.as_mut().reset(new_action.completes_at);
    ctx.active_cast = Some(new_action);

    Ok(true)
}

/// Handle `TextCommand` (0x12) — spell casting, skill use, actions.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_text_command(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    cast_timer: &mut Pin<Box<Sleep>>,
    _skill_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if packet.id() != TextCommand::ID {
        return Ok(false);
    }

    let cmd = match TextCommand::from_bytes(&packet.data) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };

    match cmd {
        TextCommand::CastSpell { spell } => {
            if let Some(p) = &ctx.infra.player {
                if let Ok(spell_id) = spell.0.trim().parse::<u16>() {
                    if let Some(spell_def) = crate::magic::get_spell(spell_id) {
                        let has_pending = ctx.has_pending_cursor();
                        let has_blocking = ctx.has_blocking_gump();
                        // Check slot-based blocking BEFORE showing target cursor.
                        if let Err(msg) = actions::can_begin_cast(
                            &ctx.active_cast, &ctx.active_skill, &ctx.active_bandage, has_pending, has_blocking,
                        ) {
                            send_spell_fizzle(msg, p, session, worker_tx).await?;
                            return Ok(true);
                        }

                        if spell_def.needs_target {
                            // Mark / Recall use a dedicated rune cursor with a
                            // "Select a rune…" prompt.
                            use crate::magic::spell_id;
                            if spell_id == spell_id::MARK || spell_id == spell_id::RECALL {
                                let (ps, prompt_pkt, cursor_pkt) =
                                    crate::magic::begin_rune_spell_target(spell_def, p.serial);
                                ctx.infra.pending_cursor = Some(PendingCursor::from_spell(&ps));
                                session.send(prompt_pkt).await?;
                                session.send(cursor_pkt).await?;
                            } else if spell_id == spell_id::WALL_OF_STONE {
                                // Wall of Stone targets a ground tile, not a
                                // world entity — send a ground cursor.
                                let (ps, target_pkt) =
                                    crate::magic::begin_wall_target(spell_def, p.serial);
                                ctx.infra.pending_cursor = Some(PendingCursor::from_spell(&ps));
                                session.send(target_pkt).await?;
                            } else {
                                let (ps, target_pkt) = crate::magic::begin_spell_target(
                                    spell_def,
                                    p.serial,
                                    None,  // not from scroll
                                );
                                ctx.infra.pending_cursor = Some(PendingCursor::from_spell(&ps));
                                session.send(target_pkt).await?;
                            }
                        } else {
                            // Self-target spell — start cast immediately.
                            let serial = p.serial;
                            let world = p.world;
                            try_start_cast(
                                spell_def,
                                serial,
                                serial,
                                world,
                                ctx,
                                cast_timer,
                                session,
                                worker_tx,
                                None,  // not from scroll
                            ).await?;
                        }
                    } else {
                        session.send(crate::game_util::system_message(
                            &format!("Unknown spell #{spell_id}."),
                        )).await?;
                    }
                }
            }
        }
        TextCommand::UseSkill { skill } => {
            if let Some(p) = &ctx.infra.player {
                let skill_str = skill.0.trim();
                let sid: Option<u16> = skill_str
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok());
                if let Some(sid) = sid {
                    debug!("[skill] use skill {} by 0x{:08X}", sid, p.serial);
                    match sid {
                        skill_id::MEDITATION => {
                            if ctx.regen_state.is_meditating() {
                                // Toggle off.
                                if let Some(msg) = ctx.regen_state.stop_meditation() {
                                    session.send(crate::game_util::system_message(msg)).await?;
                                }
                            } else {
                                let msg = ctx.regen_state.start_meditation();
                                session.send(crate::game_util::system_message(msg)).await?;
                            }
                        }
                        skill_id::ARMS_LORE => {
                            let has_pending = ctx.has_pending_cursor();
                            let has_blocking = ctx.has_blocking_gump();
                            // Check skill slot blocking BEFORE showing target cursor.
                            if let Err(msg) = actions::can_begin_skill(
                                &ctx.active_skill, has_pending, has_blocking,
                            ) {
                                session.send(crate::game_util::system_message(msg)).await?;
                                return Ok(true);
                            }

                            // Show target cursor — skill evaluation begins
                            // after the player selects a target.
                            let (pc, target_pkt) = begin_skill_target(sid, p.serial);
                            ctx.infra.pending_cursor = Some(pc);
                            session.send(target_pkt).await?;
                        }
                        skill_id::POISONING => {
                            let has_pending = ctx.has_pending_cursor();
                            let has_blocking = ctx.has_blocking_gump();
                            // Check skill slot blocking BEFORE showing target cursor.
                            if let Err(msg) = actions::can_begin_skill(
                                &ctx.active_skill, has_pending, has_blocking,
                            ) {
                                session.send(crate::game_util::system_message(msg)).await?;
                                return Ok(true);
                            }

                            // Show the bottle-selection cursor; the two-step
                            // poisoning flow lives in `super::poison`.
                            super::poison::begin_poison_skill(ctx, session).await?;
                        }
                        skill_id::ANIMAL_TAMING => {
                            let has_pending = ctx.has_pending_cursor();
                            let has_blocking = ctx.has_blocking_gump();
                            // Check skill slot blocking BEFORE showing target cursor.
                            if let Err(msg) = actions::can_begin_skill(
                                &ctx.active_skill, has_pending, has_blocking,
                            ) {
                                session.send(crate::game_util::system_message(msg)).await?;
                                return Ok(true);
                            }

                            // Show target cursor; taming completes on the
                            // skill timer (see `complete_skill_use`).
                            let (pc, target_pkt) = begin_skill_target(sid, p.serial);
                            ctx.infra.pending_cursor = Some(pc);
                            session.send(crate::game_util::system_speech(
                                "Whom do you wish to tame?",
                            )).await?;
                            session.send(target_pkt).await?;
                        }
                        _ => {
                            session.send(crate::game_util::system_message_gray(
                                &format!("You use skill #{sid}."),
                            )).await?;
                        }
                    }
                }
            }
        }
        TextCommand::Action { action } => {
            if let Some(p) = &ctx.infra.player {
                let action_id: u16 = match action.0.as_str() {
                    "bow" => anim::BOW,
                    "salute" => anim::SALUTE,
                    _ => 0,
                };
                if action_id != 0 {
                    // Check mount state via the engine so emotes with no
                    // mounted variant are silently skipped.
                    let engine = crate::game_util::engine_for(worker_tx, p.world);
                    let is_mounted = engine.get_entity(p.serial).await
                        .as_ref()
                        .and_then(|e| e.mobile())
                        .map(|m| m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount))
                        .unwrap_or(false);
                    crate::game_util::send_resolved_animation(
                        worker_tx, p.world, p.serial, action_id,
                        is_mounted, 5, 1, p.x, p.y,
                    ).await;
                }
            }
        }
        _ => {} // OpenDoor, etc.
    }

    Ok(true)
}

/// Handle `GeneralInfo` (0xBF) spell sub-commands (CastTargetedSpell 0x002D).
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_general_info_spell(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    cast_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if packet.id() != 0xBF || packet.data.len() < 5 {
        return Ok(false);
    }

    let subcmd = u16::from_be_bytes([packet.data[3], packet.data[4]]);

    // CastTargetedSpell (0x002D) — target is embedded in the packet,
    // so there is no pending cursor.  Check can_begin_cast with
    // has_pending=false (no cursor was shown for this path).
    if subcmd == 0x002D && packet.data.len() >= 11 {
        let spell_id = u16::from_be_bytes([packet.data[5], packet.data[6]]);
        let target = u32::from_be_bytes([packet.data[7], packet.data[8], packet.data[9], packet.data[10]]);
        if let Some(p) = &ctx.infra.player {
            if let Some(spell_def) = crate::magic::get_spell(spell_id) {
                // Check cast slot blocking.
                if let Err(msg) = actions::can_begin_cast(
                    &ctx.active_cast, &ctx.active_skill, &ctx.active_bandage, false, ctx.has_blocking_gump(),
                ) {
                    send_spell_fizzle(msg, p, session, worker_tx).await?;
                    return Ok(true);
                }

                let serial = p.serial;
                let world = p.world;
                try_start_cast(
                    spell_def,
                    serial,
                    target,
                    world,
                    ctx,
                    cast_timer,
                    session,
                    worker_tx,
                    None,  // not from scroll
                ).await?;
            }
        }
        return Ok(true);
    }

    Ok(false)
}

// ── Skill target handling ─────────────────────────────────────────────────

/// Handle a skill target-cursor response (0x6C).
///
/// The caller must have already taken a `PendingCursor` with
/// `CursorKind::Skill` and verified the cursor ID matches.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_skill_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    skill_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
) -> error::Result<bool> {
    let CursorKind::Skill { skill_id: sid, user_serial: _ } = pending.kind else {
        unreachable!("handle_skill_target called with non-Skill cursor kind");
    };

    if let Ok(tc) = TargetCursor::from_bytes(&packet.data) {
        if tc.cursor_type == 3 || tc.target_serial == 0 {
            // Cancelled by the player.
        } else {
            let p = ctx.infra.player.as_ref().unwrap();

            // Re-check skill slot (something may have started while
            // the target cursor was open).  has_pending=false because
            // this IS the pending cursor being resolved.
            if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, false, ctx.has_blocking_gump()) {
                session.send(crate::game_util::system_message(msg)).await?;
                return Ok(true);
            }

            // Start the skill-use action with the appropriate delay.
            let delay = match sid {
                skill_id::ARMS_LORE => Duration::from_millis(skill_timing::ARMS_LORE_DELAY_MS),
                skill_id::ANIMAL_TAMING => Duration::from_millis(crate::taming::TAME_DELAY_MS),
                _ => Duration::from_secs(1),
            };

            let payload = ActionPayload::SkillUse {
                skill_id: sid,
                user_serial: p.serial,
                target_serial: tc.target_serial,
                world: p.world,
            };

            let begin_msg = match sid {
                skill_id::ANIMAL_TAMING => "You start to tame the creature.",
                _ => "You begin evaluating...",
            };
            let new_action = ActiveAction::new(ActionKind::SkillUse, delay, payload);
            session.send(crate::game_util::system_message(begin_msg)).await?;
            skill_timer.as_mut().reset(new_action.completes_at);
            ctx.active_skill = Some(new_action);
        }
        return Ok(true);
    }

    Ok(false)
}

/// Complete a skill-use action (called when the skill timer fires).
///
/// Sends the result message to the player's session.
pub(super) async fn complete_skill_use(
    skill_id: u16,
    user_serial: u32,
    target_serial: u32,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    match skill_id {
        skill_id::ARMS_LORE => {
            let engine = crate::game_util::engine_for(worker_tx, world);
            let clean_serial = target_serial & 0x7FFF_FFFF;
            let info = engine.find_item_info(clean_serial).await;

            // Resolve graphic/color — from a container item or, failing that,
            // from an item equipped on the user (e.g. their wielded weapon).
            let resolved = match info {
                Some((serial, graphic, color, _amount)) => Some((serial, graphic, color)),
                None => resolve_equipped_item(&engine, user_serial, clean_serial).await,
            };

            let msg = match resolved {
                Some((serial, graphic, color)) => {
                    let mut line = format!(
                        "[Arms Lore] Serial: 0x{:08X}, Graphic: 0x{:04X}, Color: 0x{:04X}",
                        serial, graphic, color,
                    );
                    // Append poison info if the weapon carries charges.
                    if let Some(props) = engine.get_item_props(serial).await {
                        let charges = props
                            .get_meta_int(super::poison::META_POISON_CHARGES)
                            .unwrap_or(0);
                        if charges > 0 {
                            let level = props
                                .get_meta_int(super::poison::META_POISON_LEVEL)
                                .unwrap_or(0) as u8;
                            line.push_str(&format!(
                                " | Poison: {} ({} charges)",
                                super::poison::level_name(level),
                                charges,
                            ));
                        }
                    }
                    line
                }
                None => {
                    "That object is no longer available.".to_string()
                }
            };
            session.send(crate::game_util::system_message_gray(&msg)).await?;
        }
        skill_id::ANIMAL_TAMING => {
            complete_taming(user_serial, target_serial, world, session, worker_tx).await?;
        }
        _ => {
            // Generic fallback for unimplemented targeted skills.
            session.send(crate::game_util::system_message("Skill complete.")).await?;
        }
    }

    Ok(())
}

/// Complete an Animal Taming attempt: validate the target is a tameable,
/// not-already-owned creature within range, roll for success, and on success
/// record ownership/command in the pet's item_props meta and attach a
/// [`crate::controller_registry::PetController`].
async fn complete_taming(
    user_serial: u32,
    target_serial: u32,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    use common::uo_engine::entity::DemoEntity;
    use common::uo_engine::item_props::{ItemProps, MetaValue};
    use crate::taming;

    let engine = crate::game_util::engine_for(worker_tx, world);
    let clean_serial = target_serial & 0x7FFF_FFFF;

    // Cannot tame yourself.
    if clean_serial == user_serial {
        session.send(crate::game_util::system_message("You cannot tame yourself.")).await?;
        return Ok(());
    }

    // Target must be a non-player mobile.
    let target = engine.get_entity(clean_serial).await;
    let (graphic, tx, ty, is_player) = match target.as_ref() {
        Some(DemoEntity::Mobile(m)) => (m.graphic, m.x, m.y, m.is_player),
        _ => {
            session.send(crate::game_util::system_message("You can't tame that.")).await?;
            return Ok(());
        }
    };
    if is_player {
        session.send(crate::game_util::system_message("You can't tame that.")).await?;
        return Ok(());
    }

    // Must be a known tameable creature.
    let Some(def) = taming::lookup_tameable(graphic) else {
        session.send(crate::game_util::system_message("That creature cannot be tamed.")).await?;
        return Ok(());
    };

    // Range re-check (the user may have walked off during the timer).
    let (ux, uy) = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y),
        None => return Ok(()),
    };
    if crate::game_util::chebyshev(ux, uy, tx, ty) > taming::TAME_RANGE {
        session.send(crate::game_util::system_message("That is too far away.")).await?;
        return Ok(());
    }

    // Already owned?
    let mut props = engine.get_item_props(clean_serial).await.unwrap_or_else(|| {
        ItemProps::with_name(def.name)
    });
    if props.get_meta_int(taming::META_PET_OWNER).is_some() {
        session.send(crate::game_util::system_message("That animal already has a master.")).await?;
        return Ok(());
    }

    // Roll for success.
    if !def.roll_tame() {
        session.send(crate::game_util::system_speech("You fail to tame the creature.")).await?;
        return Ok(());
    }

    // Success: record ownership + default command, then attach the pet AI.
    props.set_meta(taming::META_PET_OWNER, MetaValue::Int(user_serial as i64));
    props.set_meta(taming::META_PET_COMMAND, MetaValue::Str(taming::CMD_FOLLOW.to_string()));
    engine.set_item_props(clean_serial, Some(props)).await;

    let controller = Box::new(crate::controller_registry::PetController::new());
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        world,
        crate::DemoCommand::AttachControllerPersist {
            serial: clean_serial,
            controller,
            controller_id: taming::PET_CONTROLLER_ID.to_string(),
        },
    )).await;

    session.send(crate::game_util::system_speech("It seems to accept you as its master!")).await?;
    info!(
        "[taming] 0x{:08X} tamed creature 0x{:08X} (graphic={:#06X})",
        user_serial, clean_serial, graphic,
    );

    Ok(())
}

/// Resolve a targeted item that is *equipped* on the user (e.g. a wielded
/// weapon), returning `(serial, graphic, color)`.
///
/// `find_item_info` only covers items in container stores, so an equipped
/// weapon must be looked up separately.  Falls back to a standalone ground
/// item entity if not equipped.
async fn resolve_equipped_item(
    engine: &common::uo_engine::rpc::EngineProxy<DemoCommand>,
    user_serial: u32,
    target_serial: u32,
) -> Option<(u32, u16, u16)> {
    use common::uo_engine::entity::DemoEntity;

    // 1. Equipped on the user?
    if let Some(m) = engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        if let Some(eq) = m.items.iter().find(|eq| eq.serial == target_serial) {
            return Some((eq.serial, eq.graphic, eq.color.unwrap_or(0)));
        }
    }

    // 2. As a standalone item entity?
    if let Some(DemoEntity::Item { serial, graphic, color, .. }) =
        engine.get_entity(target_serial).await
    {
        return Some((serial, graphic, color));
    }

    None
}
