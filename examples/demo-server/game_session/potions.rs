//! Potion usage: double-click potion → consume → apply effect → cooldown.
//!
//! Flow:
//! 1. Player double-clicks a potion item in their backpack.
//! 2. Graphic+color are matched against the potion table.
//! 3. Cooldown is checked — if still on cooldown, a message is shown.
//! 4. The potion is consumed (atomic `ConsumeItem` engine command).
//! 5. The effect is applied (heal, mana restore, stat buff, etc.).
//! 6. Drink sound + eat/drink animation are played.
//! 7. A global potion cooldown is set.
//!
//! Unlike spells and bandages, potions are instant and self-targeted —
//! no target cursor, no channeling delay.

use log::info;

use protocol::RawPacket;
use packets::traits::BasicPacket;

use network::error;
use network::session::Session;

use packets::interaction::DoubleClick;

use common::uo_engine::entity::DemoEntity;

use tokio::time::Instant;

use crate::buffs::{self, BuffKind};
use crate::constants::potion as potion_cfg;
use crate::game_util;
use crate::potions::{self, PotionDef, PotionEffect};
use crate::DemoWorkerTx;

use super::session_state::SessionContext;

// ── Double-click intercept ───────────────────────────────────────────────

/// Check if a double-click packet targets a potion item.
///
/// If it does, verify the potion is accessible, check cooldown, consume it,
/// apply the effect, and play feedback.
///
/// Returns `true` if the packet was consumed (regardless of success).
pub(super) async fn handle_potion_double_click(
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

    // Paperdoll request (high bit) — not a potion.
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }

    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let (player_serial, player_world, player_x, player_y) = match &ctx.infra.player {
        Some(p) => (p.serial, p.world, p.x, p.y),
        None => return Ok(false),
    };

    // Look up the item in container stores (typically the player's backpack).
    let engine = crate::game_util::engine_for(worker_tx, player_world);
    let item_info = engine.find_item_info(clean_serial).await;

    let (graphic, color) = match &item_info {
        Some((_serial, graphic, color, _amount)) => (*graphic, *color),
        None => {
            // Not in a container — check as a ground entity.
            match engine.get_entity(clean_serial).await {
                Some(DemoEntity::Item { graphic, color, x, y, .. }) => {
                    // Quick check: is this even a potion graphic?
                    if !potions::is_potion_graphic(graphic) {
                        return Ok(false);
                    }
                    // Range check for ground items.
                    let dx = (player_x as i32 - x as i32).unsigned_abs() as u16;
                    let dy = (player_y as i32 - y as i32).unsigned_abs() as u16;
                    if dx > 2 || dy > 2 {
                        session.send(game_util::system_message("That is too far away.")).await?;
                        return Ok(true);
                    }
                    (graphic, color)
                }
                _ => return Ok(false),
            }
        }
    };

    // Look up the potion definition.
    let potion = match potions::lookup_potion(graphic, color) {
        Some(p) => p,
        None => return Ok(false), // not a known potion
    };

    // ── Targeted potions (not drunk) ─────────────────────────────────
    // The shrink potion opens a target cursor and is consumed on success;
    // it does not follow the drink / cooldown path below.
    if matches!(potion.effect, PotionEffect::Shrink) {
        super::shrink::begin_shrink(clean_serial, ctx, session).await?;
        return Ok(true);
    }

    // ── Cooldown check ───────────────────────────────────────────────
    if let Some(until) = ctx.potion_cooldown_until {
        if Instant::now() < until {
            let remaining = until - Instant::now();
            session.send(game_util::system_message(
                &format!("You must wait {} seconds before using another potion.",
                    remaining.as_secs() + 1),
            )).await?;
            return Ok(true);
        }
    }

    // ── Resolve per-instance poison level (before consuming) ─────────
    // Poison bottles carry their level in meta, not in the table; read it now
    // because consuming the item may drop its ItemProps.
    let poison_level = if potions::is_poison_graphic(graphic) {
        Some(
            engine
                .get_item_props(clean_serial)
                .await
                .and_then(|p| p.get_meta_int(super::poison::META_POISON_LEVEL))
                .map(|v| v.clamp(1, 4) as u8)
                .unwrap_or(1),
        )
    } else {
        None
    };

    // ── Consume the potion ───────────────────────────────────────────
    let consumed = engine.consume_item(
        clean_serial,
        1,
        Some(graphic),
    ).await;

    if consumed.is_none() {
        session.send(game_util::system_message("The potion is no longer available.")).await?;
        return Ok(true);
    }

    // ── Apply effect ─────────────────────────────────────────────────
    apply_potion_effect(potion, poison_level, player_serial, player_world, ctx, session, worker_tx).await?;

    // ── Drink animation + sound ──────────────────────────────────────
    let entity = engine.get_entity(player_serial).await;
    if let Some(m) = entity.as_ref().and_then(|e| e.mobile()) {
        let mounted = m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount);
        game_util::send_sound(
            worker_tx, player_world, potion.sound,
            m.x, m.y, m.z as i16,
        ).await;
        game_util::send_resolved_animation(
            worker_tx, player_world, player_serial,
            potion_cfg::DRINK_ANIM, mounted,
            5, // frame_count
            1, // repeat_count
            m.x, m.y,
        ).await;
    }

    // ── Set cooldown ─────────────────────────────────────────────────
    ctx.potion_cooldown_until = Some(
        Instant::now() + std::time::Duration::from_millis(potion_cfg::COOLDOWN_MS),
    );

    // ── Feedback message ─────────────────────────────────────────────
    // Use the per-instance poison name when this is a poison bottle.
    let display_name = match poison_level {
        Some(level) => potions::poison_name(level),
        None => potion.name,
    };
    session.send(game_util::system_message(
        &format!("You drink a {}.", display_name),
    )).await?;

    info!(
        "[potion] 0x{:08X} used {} (id={})",
        player_serial, display_name, potion.id,
    );

    Ok(true)
}

// ── Effect application ───────────────────────────────────────────────────

async fn apply_potion_effect(
    potion: &PotionDef,
    poison_level_override: Option<u8>,
    serial: u32,
    world: u8,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = crate::game_util::engine_for(worker_tx, world);

    match potion.effect {
        PotionEffect::Heal { min, max } => {
            let amount = game_util::random_range(min, max);
            let _new_hp = engine.heal(serial, amount).await;
            info!("[potion] healed 0x{:08X} for {} HP", serial, amount);
        }

        PotionEffect::Refresh { min, max } => {
            let amount = game_util::random_range(min, max);
            let _new_stam = engine.modify_stamina(serial, amount as i32).await;
            info!("[potion] refreshed 0x{:08X} for {} stamina", serial, amount);
        }

        PotionEffect::RestoreMana { min, max } => {
            let amount = game_util::random_range(min, max);
            let _new_mana = engine.modify_mana(serial, amount as i32).await;
            info!("[potion] restored 0x{:08X} for {} mana", serial, amount);
        }

        PotionEffect::Cure => {
            // Attempt to neutralise poison.  Success chance scales inversely
            // with the current poison level.
            let entity = engine.get_entity(serial).await;
            let cur_level = entity
                .as_ref()
                .and_then(|e| e.mobile())
                .map(|m| m.poison_level)
                .unwrap_or(0);

            if cur_level == 0 {
                session.send(game_util::system_message(
                    "You are not poisoned.",
                )).await?;
            } else {
                let idx = crate::constants::poison::level_index(cur_level).unwrap_or(0);
                let chance = crate::constants::poison::CURE_CHANCE_PCT[idx];
                let roll = game_util::random_range(1, 100);
                if (roll as u32) <= chance {
                    let _ = engine.cure_poison(serial).await;
                    session.send(game_util::system_message(
                        "You feel cured of any toxins.",
                    )).await?;
                    info!("[potion] cured poison (level {}) on 0x{:08X}", cur_level, serial);
                } else {
                    session.send(game_util::system_message(
                        "You feel a little better, but the poison lingers.",
                    )).await?;
                    info!("[potion] cure failed (level {}) on 0x{:08X}", cur_level, serial);
                }
            }
        }

        PotionEffect::Strength { bonus, duration_ms } => {
            // Revert old buff if replacing.
            if let Some(old_delta) = ctx.buff_state.add_buff(
                BuffKind::Strength, bonus, duration_ms,
            ) {
                buffs::revert_buff_stat(worker_tx, world, serial, BuffKind::Strength, old_delta).await;
            }
            // Apply new buff.
            buffs::apply_buff_stat(worker_tx, world, serial, BuffKind::Strength, bonus).await;
            session.send(game_util::system_message(
                "You feel stronger!",
            )).await?;
        }

        PotionEffect::Agility { bonus, duration_ms } => {
            // Revert old buff if replacing.
            if let Some(old_delta) = ctx.buff_state.add_buff(
                BuffKind::Agility, bonus, duration_ms,
            ) {
                buffs::revert_buff_stat(worker_tx, world, serial, BuffKind::Agility, old_delta).await;
            }
            // Apply new buff.
            buffs::apply_buff_stat(worker_tx, world, serial, BuffKind::Agility, bonus).await;
            session.send(game_util::system_message(
                "You feel more agile!",
            )).await?;
        }

        PotionEffect::Poison { level } => {
            // Drinking a poison potion poisons the drinker.  Prefer the
            // per-instance level from meta; fall back to the table default.
            use crate::constants::poison as poison_cfg;
            let level = poison_level_override.unwrap_or(level);
            let idx = poison_cfg::level_index(level).unwrap_or(0);
            engine.apply_poison(
                serial,
                level,
                poison_cfg::DURATION_MS[idx],
                poison_cfg::DAMAGE_PER_TICK[idx],
                poison_cfg::TICK_INTERVAL_MS,
                serial,
            ).await;
            session.send(game_util::system_message(
                "You feel extremely ill as the poison takes hold.",
            )).await?;
            info!("[potion] 0x{:08X} drank a level {} poison potion", serial, level);
        }

        PotionEffect::Shrink => {
            // Handled earlier via a target cursor (see handle_potion_double_click);
            // a shrink potion is never drunk, so this branch is unreachable.
        }
    }

    Ok(())
}
