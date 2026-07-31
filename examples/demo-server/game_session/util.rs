//! Small utility functions shared across session submodules.

use protocol::RawPacket;
use packets::movement::Notoriety;
use packets::traits::ManualPacket;

use network::error;
use network::session::Session;

use crate::constants::hue;
use crate::DemoWorkerTx;

use super::PlayerState;

/// Map a [`Notoriety`] value to the standard UO name-overhead hue.
pub(crate) fn notoriety_hue(n: Notoriety) -> u16 {
    match n {
        Notoriety::Innocent => hue::NOTO_INNOCENT,
        Notoriety::Ally => hue::NOTO_ALLY,
        Notoriety::Attackable => hue::NOTO_GRAY,
        Notoriety::Criminal => hue::NOTO_GRAY,
        Notoriety::Enemy => hue::NOTO_ENEMY,
        Notoriety::Murderer => hue::NOTO_MURDERER,
        Notoriety::Translucent => hue::NOTO_GRAY,
        Notoriety::Invalid => hue::NOTO_GRAY,
        Notoriety::Unknown(_) => hue::NOTO_GRAY,
    }
}

/// Send a `StatusBarInfo` (0x11) packet with the current weight to the
/// player's session.
///
/// This should be called after any operation that changes the player's
/// carried weight (pickup, drop, equip, consume).
///
/// `held_item` is the item on the player's cursor, if any, as
/// `(serial, graphic, amount)`.
pub(super) async fn send_weight_update(
    player: &PlayerState,
    held_item: Option<(u32, u16, u16)>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    let Some((cur_weight, _max_weight)) = engine.compute_weight(
        player.serial, held_item,
    ).await else {
        return Ok(());
    };

    // StatusBarInfo (0x11) with just weight updated.
    // We send a minimal "self" status update: the client merges it into
    // the existing status bar.
    //
    // Unfortunately UO's 0x11 packet is all-or-nothing for BaseStats —
    // we need the current stat values.  Fetch the entity to get them.
    let entity = engine.get_entity(player.serial).await;

    let Some(m) = entity.as_ref().and_then(|e| e.mobile()) else {
        return Ok(());
    };

    // Compute armor rating.
    let armor_rating = engine.query_equipment_armor(m.serial)
        .await
        .map(|p| p.total())
        .unwrap_or(0);

    // Real gold tally from the backpack (recursive, including sub-containers).
    let gold = engine.count_gold(m.serial, held_item).await;

    let label = if m.name.is_empty() {
        format!("[mob 0x{:04X}]", m.serial)
    } else {
        m.name.clone()
    };

    let sbi = packets::status::StatusBarInfo {
        serial: m.serial,
        name: packets::u_io::FixedString::new(&label),
        hit_points: m.hits,
        max_hit_points: m.hits_max,
        name_change_flag: 0,
        status_flag: 1, // full stats (self)
        is_female: Some(crate::game_util::is_female_body(m.graphic)),
        stats: Some(packets::status::BaseStats {
            strength: m.str_,
            dexterity: m.dex,
            intelligence: m.int,
            stamina: m.stamina,
            max_stamina: m.stamina_max,
            mana: m.mana,
            max_mana: m.mana_max,
            gold,
            armor_rating,
            weight: cur_weight,
        }),
        uoml: None,
        uor: None,
        aos: None,
        uokr: None,
    };

    session.send(RawPacket::s2c(sbi.to_bytes())).await?;
    Ok(())
}

/// Re-send the player's full skill list with current equipment bonuses
/// applied (0x3A, type 0x02 — with cap).
///
/// Called after any operation that changes equipped "plus" weapons (equip /
/// unequip), so the client reflects the new effective skill values.  Sending
/// the *full* list (rather than per-skill deltas) keeps the client perfectly
/// in sync regardless of what changed — including a skill whose bonus was
/// just removed (it returns to its base value).
///
/// No-op if the player entity is missing or has no skills.
pub(super) async fn send_skill_update_after_equipment_change(
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    let Some(skills) = engine.query_skills(player.serial).await else {
        return Ok(());
    };
    if skills.is_empty() {
        return Ok(());
    }

    let bonuses = engine.query_skill_bonuses(player.serial).await;
    let send = crate::skills::build_full_list_with_bonuses(&skills, &bonuses);
    session.send(RawPacket::s2c(send.to_bytes())).await?;
    Ok(())
}

