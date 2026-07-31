//! Combat stat operations: damage, healing, mana, stamina.
//!
//! Each function performs the stat mutation on the entity in the zone
//! store and emits the appropriate [`WorldEvent`].

use framework::continuum::{Zone, WorldEvent};
use framework::continuum::item_props::ZoneItemProps;
use framework::continuum::container::HashContainerStore;
use framework::ecumene::Entity as EngineEntity;

use crate::uo_engine::entity::DemoEntity;
use crate::uo_engine::notoriety::{
    NotorietyClass, AGGRESSOR_FLAG_MS, CRIMINAL_FLAG_MS, MAX_AGGRESSORS, MURDERER_THRESHOLD,
};

// ── Damage ──────────────────────────────────────────────────────────────

/// Deal damage to a mobile entity.
///
/// Reduces HP by `amount`, publishes `DamageDealt` event.
/// Returns `Some((new_hp, killed))`, or `None` if entity not found / not a mobile.
pub(super) fn handle_deal_damage<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    amount: u16,
    source_serial: u32,
) -> Option<(u16, bool)> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let new_hp = entity.apply_damage(amount);
    let killed = new_hp == 0;
    let pos = EngineEntity::pos(entity);
    let max_hp = entity.hits().map(|(_, m)| m).unwrap_or(0);
    let _ = event_tx.send(WorldEvent::DamageDealt {
        map_id: zone.map_id,
        serial,
        source_serial,
        amount,
        new_hits: new_hp,
        max_hits: max_hp,
        x: pos.x,
        y: pos.y,
    });
    Some((new_hp, killed))
}

// ── Healing ─────────────────────────────────────────────────────────────

/// Heal a mobile entity.
///
/// Increases HP by `amount` (capped at max_hp), publishes `MobileHealed`
/// event if HP actually changed.  Returns the new HP value.
pub(super) fn handle_heal<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    amount: u16,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let old_hp = entity.hits().map(|(c, _)| c).unwrap_or(0);
    let new_hp = entity.apply_heal(amount);
    // Only broadcast if HP actually changed.
    if new_hp != old_hp {
        let pos = EngineEntity::pos(entity);
        let max_hp = entity.hits().map(|(_, m)| m).unwrap_or(0);
        let _ = event_tx.send(WorldEvent::MobileHealed {
            map_id: zone.map_id,
            serial,
            amount,
            new_hits: new_hp,
            max_hits: max_hp,
            x: pos.x,
            y: pos.y,
        });
    }
    Some(new_hp)
}

// ── Mana ────────────────────────────────────────────────────────────────

/// Consume mana from a mobile entity.
///
/// Returns the new mana value, or `None` if entity not found, not a
/// mobile, or insufficient mana.
pub(super) fn handle_consume_mana<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    amount: u16,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let (cur_mana, max_mana) = entity.mana().unwrap_or((0, 0));
    if cur_mana < amount {
        return None; // insufficient mana
    }

    let new_mana = entity.modify_mana(-(amount as i32));
    let pos = EngineEntity::pos(entity);
    let (stamina, max_stamina) = entity.stamina().unwrap_or((0, 0));
    let _ = event_tx.send(WorldEvent::ManaStaminaChanged {
        map_id: zone.map_id,
        serial,
        mana: new_mana,
        max_mana,
        stamina,
        max_stamina,
        x: pos.x,
        y: pos.y,
    });
    Some(new_mana)
}

/// Modify mana by a delta (can be negative).
///
/// Unlike `consume_mana`, this always succeeds (clamped to 0..max).
/// Returns the new mana value, or `None` if entity not found / not a mobile.
pub(super) fn handle_modify_mana<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    delta: i32,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let old_mana = entity.mana().map(|(c, _)| c).unwrap_or(0);
    let new_mana = entity.modify_mana(delta);
    // Only broadcast if mana actually changed.
    if new_mana != old_mana {
        let pos = EngineEntity::pos(entity);
        let (_, max_mana) = entity.mana().unwrap_or((new_mana, new_mana));
        let (stamina, max_stamina) = entity.stamina().unwrap_or((0, 0));
        let _ = event_tx.send(WorldEvent::ManaStaminaChanged {
            map_id: zone.map_id,
            serial,
            mana: new_mana,
            max_mana,
            stamina,
            max_stamina,
            x: pos.x,
            y: pos.y,
        });
    }
    Some(new_mana)
}

// ── Stamina ─────────────────────────────────────────────────────────────

/// Modify stamina by a delta (can be negative).
///
/// Returns the new stamina value, or `None` if entity not found / not a mobile.
pub(super) fn handle_modify_stamina<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    delta: i32,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let old_stamina = entity.stamina().map(|(c, _)| c).unwrap_or(0);
    let new_stamina = entity.modify_stamina(delta);
    // Only broadcast if stamina actually changed.
    if new_stamina != old_stamina {
        let pos = EngineEntity::pos(entity);
        let (mana, max_mana) = entity.mana().unwrap_or((0, 0));
        let (_, max_stamina) = entity.stamina().unwrap_or((new_stamina, new_stamina));
        let _ = event_tx.send(WorldEvent::ManaStaminaChanged {
            map_id: zone.map_id,
            serial,
            mana,
            max_mana,
            stamina: new_stamina,
            max_stamina,
            x: pos.x,
            y: pos.y,
        });
    }
    Some(new_stamina)
}

// ── Str / Dex ───────────────────────────────────────────────────────────

/// Modify strength by a delta (can be negative).
///
/// Returns the new str value, or `None` if entity not found / not a mobile.
pub(super) fn handle_modify_str<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    delta: i32,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let old_str = entity.str_val().unwrap_or(0);
    let new_str = entity.modify_str(delta);
    if new_str != old_str {
        emit_base_stat_changed(zone, event_tx, serial);
    }
    Some(new_str)
}

/// Modify dexterity by a delta (can be negative).
///
/// Returns the new dex value, or `None` if entity not found / not a mobile.
pub(super) fn handle_modify_dex<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    delta: i32,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let old_dex = entity.dex_val().unwrap_or(0);
    let new_dex = entity.modify_dex(delta);
    if new_dex != old_dex {
        emit_base_stat_changed(zone, event_tx, serial);
    }
    Some(new_dex)
}

/// Modify intelligence by a delta (can be negative).
///
/// Returns the new int value, or `None` if entity not found / not a mobile.
pub(super) fn handle_modify_int<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    delta: i32,
) -> Option<u16> {
    let entity = zone.store.get_mut(serial)?;
    if !entity.is_mobile() {
        return None;
    }

    let old_int = entity.int_val().unwrap_or(0);
    let new_int = entity.modify_int(delta);
    if new_int != old_int {
        emit_base_stat_changed(zone, event_tx, serial);
    }
    Some(new_int)
}

/// Emit a `BaseStatChanged` world event with the mobile's current stats.
pub(super) fn emit_base_stat_changed<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
) {
    if let Some(entity) = zone.store.get(serial) {
        if let Some(m) = entity.mobile() {
            let _ = event_tx.send(WorldEvent::BaseStatChanged {
                map_id: zone.map_id,
                serial,
                str_: m.str_,
                dex: m.dex,
                int: m.int,
                hits: m.hits,
                hits_max: m.hits_max,
                mana: m.mana,
                mana_max: m.mana_max,
                stamina: m.stamina,
                stamina_max: m.stamina_max,
                x: m.x,
                y: m.y,
            });
        }
    }
}

/// Emit an `EntityUpdated` event for a mobile so observers re-render its
/// (possibly recoloured) snapshot.
fn emit_entity_updated<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
) {
    if let Some(entity) = zone.store.get(serial) {
        let pos = EngineEntity::pos(entity);
        let snap = entity.snapshot();
        let _ = event_tx.send(WorldEvent::EntityUpdated {
            map_id: zone.map_id,
            serial,
            pos,
            entity: snap,
        });
    }
}

/// Insert/refresh an aggressor entry `(other, now+ttl)` on a mobile, pruning
/// expired entries and capping the list size.
fn push_aggressor(m: &mut crate::uo_engine::entity::MobileData, other: u32, now: u64) {
    let until = now + AGGRESSOR_FLAG_MS;
    // Drop expired entries.
    m.aggressors.retain(|(_, t)| *t > now);
    if let Some(slot) = m.aggressors.iter_mut().find(|(s, _)| *s == other) {
        slot.1 = until;
    } else {
        if m.aggressors.len() >= MAX_AGGRESSORS {
            // Evict the soonest-expiring entry.
            if let Some((idx, _)) = m
                .aggressors
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, t))| *t)
            {
                m.aggressors.remove(idx);
            }
        }
        m.aggressors.push((other, until));
    }
}

/// Record an act of aggression by `attacker` against `victim`.
///
/// - Establishes a mutual aggressor relationship (each lists the other), so
///   the victim may retaliate without becoming a criminal.
/// - If the victim is an *innocent player* and was not already an aggressor
///   to the attacker (i.e. the strike is unprovoked), the attacker is flagged
///   criminal.
pub(super) fn handle_flag_aggression<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    attacker: u32,
    victim: u32,
) {
    if attacker == victim || attacker == 0 || victim == 0 {
        return;
    }
    let now = crate::uo_engine::entity::MobileData::now_epoch_ms();

    // Read victim's relevant state first (immutable borrow).
    let (victim_is_innocent_player, victim_had_attacker_as_aggressor) = {
        let Some(vm) = zone.store.get(victim).and_then(|e| e.mobile()) else {
            return;
        };
        let innocent_player = vm.is_player
            && vm.effective_notoriety_class() == NotorietyClass::Innocent;
        (innocent_player, vm.is_aggressor_to(attacker))
    };

    // The attack is "provoked" if the victim had previously aggressed the
    // attacker (the attacker is on the victim's aggressor list).
    let provoked = victim_had_attacker_as_aggressor;

    // Update the attacker: mark victim as an aggressor target, and flag
    // criminal if this was an unprovoked attack on an innocent player.
    let mut attacker_changed = false;
    if let Some(am) = zone.store.get_mut(attacker).and_then(|e| e.mobile_mut()) {
        push_aggressor(am, victim, now);
        if victim_is_innocent_player && !provoked && am.is_player {
            let already_criminal = am.criminal_until_ms > now;
            am.criminal_until_ms = now + CRIMINAL_FLAG_MS;
            if !already_criminal {
                attacker_changed = true;
            }
        }
    }

    // Update the victim: record the attacker as an aggressor too, so the
    // victim's defensive retaliation is treated as provoked.
    if let Some(vm) = zone.store.get_mut(victim).and_then(|e| e.mobile_mut()) {
        push_aggressor(vm, attacker, now);
    }

    // Re-broadcast the attacker's snapshot if its colour may have changed.
    if attacker_changed {
        emit_entity_updated(zone, event_tx, attacker);
    }
}

/// Record that `killer` killed `victim`, updating murder counts.
pub(super) fn handle_record_kill<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    killer: u32,
    victim: u32,
) {
    if killer == victim || killer == 0 || victim == 0 {
        return;
    }

    // Only innocent-player victims count as murders.
    let victim_was_innocent_player = zone
        .store
        .get(victim)
        .and_then(|e| e.mobile())
        .map(|m| m.is_player && m.effective_notoriety_class() == NotorietyClass::Innocent)
        .unwrap_or(false);
    if !victim_was_innocent_player {
        return;
    }

    let mut became_murderer = false;
    if let Some(km) = zone.store.get_mut(killer).and_then(|e| e.mobile_mut()) {
        if km.is_player {
            let was_below = km.murders < MURDERER_THRESHOLD;
            km.murders = km.murders.saturating_add(1);
            if was_below && km.murders >= MURDERER_THRESHOLD {
                became_murderer = true;
            }
        }
    }
    if became_murderer {
        emit_entity_updated(zone, event_tx, killer);
    }
}

/// Apply GM reputation overrides; any `Some` field is set.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_set_reputation<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    murders: Option<u16>,
    karma: Option<i32>,
    fame: Option<i32>,
    guild_id: Option<Option<u32>>,
    criminal: Option<bool>,
) {
    let now = crate::uo_engine::entity::MobileData::now_epoch_ms();
    let mut changed = false;
    if let Some(m) = zone.store.get_mut(serial).and_then(|e| e.mobile_mut()) {
        if let Some(v) = murders { m.murders = v; changed = true; }
        if let Some(v) = karma { m.karma = v; }
        if let Some(v) = fame { m.fame = v; }
        if let Some(v) = guild_id { m.guild_id = v; changed = true; }
        if let Some(v) = criminal {
            m.criminal_until_ms = if v { now + CRIMINAL_FLAG_MS } else { 0 };
            changed = true;
        }
    }
    if changed {
        emit_entity_updated(zone, event_tx, serial);
    }
}
