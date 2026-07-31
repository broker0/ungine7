//! Melee combat system for the demo server.
//!
//! ## Architecture: charge-based swing with aggro list
//!
//! Rather than swinging on a fixed cadence (which produces animations even
//! when the target is out of range), the weapon **charges** over the swing
//! delay period.  Once charged, the next check that finds a reachable target
//! will consume the charge and deliver a strike.
//!
//! Each session maintains an **aggro list** (`targets: HashSet<u32>`) — a set
//! of entities the player is fighting.  In war mode, entities that deal damage
//! to the player are automatically added.  Without war mode only the manually
//! selected target is tracked.
//!
//! The charge can be consumed from three trigger points:
//! 1. Swing timer fires (charge just became ready).
//! 2. Player moves (may now be in range of a target).
//! 3. A target moves (via `WorldEvent::EntityMoved`).
//!
//! ## Weapon-away model
//!
//! While a timed action (spell cast, bandage, skill use) is in progress the
//! weapon is conceptually "put away".  If the charge is ready and a target
//! is in range but the weapon is away, the charge is **silently wasted** and
//! a new charge cycle begins.  After the action completes there is a short
//! recovery delay ([`melee::ACTION_RECOVERY_DELAY_MS`]) before the weapon is
//! considered back in hand.

use std::collections::HashSet;
use std::time::Duration;

use log::{debug, info};
use protocol::RawPacket;
use packets::layer::Layer;
use packets::traits::{encode_packet, BasicPacket};
use packets::world::EquippedItem;
use tokio::time::Instant;

use framework::continuum::WorkerCommand;
use crate::constants::{anim, melee, weapon};
use crate::constants::armor::HitZone;
use crate::constants::weapon::WeaponDef;
use crate::game_util::{self, chebyshev, random_range};
use crate::{DemoCommand, DemoWorkerTx};

// ── Constants ─────────────────────────────────────────────────────────────

/// Maximum number of simultaneous aggro targets.
const MAX_AGGRO_TARGETS: usize = 8;

/// Polling interval when charge is held but no target is in range (ms).
/// Not used with event-driven triggers, but kept as a safety fallback.
#[allow(dead_code)]
const CHARGE_HOLD_POLL_MS: u64 = 250;

// ── CombatState ───────────────────────────────────────────────────────────

/// Per-session melee combat state.
pub struct CombatState {
    /// Whether the player is in war (combat) mode.
    pub war_mode: bool,
    /// All entities we are actively fighting (aggro list).
    pub targets: HashSet<u32>,
    /// Last manually selected target — gets priority when striking.
    /// Also used for the client-side attack indicator.
    pub primary_target: Option<u32>,
    /// `true` when the swing delay has elapsed and the weapon is ready.
    pub charged: bool,
    /// When the current charge cycle completes (only meaningful when
    /// `charged == false` and `has_targets() == true`).
    pub next_swing: Instant,
    /// Weapon is conceptually "away" (put down) during timed actions.
    pub weapon_away: bool,
    /// Earliest instant the weapon is back in hand after an action completes.
    pub recovery_until: Instant,
    /// Cached swing delay from the last weapon resolution, so we can
    /// restart the charge cycle without an engine query.
    pub cached_swing_delay: Duration,
}

impl CombatState {
    /// Far-future sentinel used when no swing is pending.
    const FAR_FUTURE: Duration = Duration::from_secs(86400);

    pub fn new() -> Self {
        Self {
            war_mode: false,
            targets: HashSet::new(),
            primary_target: None,
            charged: false,
            next_swing: Instant::now() + Self::FAR_FUTURE,
            weapon_away: false,
            recovery_until: Instant::now(),
            cached_swing_delay: Duration::from_millis(melee::FIST_SWING_DELAY_MS),
        }
    }

    // ── Target management ────────────────────────────────────────────

    /// Add a target to the aggro list and set it as primary.
    /// Starts charging if not already.
    pub fn add_target(&mut self, serial: u32) {
        if self.targets.len() >= MAX_AGGRO_TARGETS && !self.targets.contains(&serial) {
            // At capacity — don't add.
            return;
        }
        self.targets.insert(serial);
        self.primary_target = Some(serial);
        self.ensure_charging();
    }

    /// Add a target to the aggro list without changing primary.
    /// Only adds if in war mode.  Used for auto-retaliation.
    pub fn add_aggro(&mut self, serial: u32) {
        if !self.war_mode {
            return;
        }
        if self.targets.len() >= MAX_AGGRO_TARGETS && !self.targets.contains(&serial) {
            return;
        }
        let was_empty = self.targets.is_empty();
        self.targets.insert(serial);
        if was_empty {
            self.ensure_charging();
        }
    }

    /// Remove a target from the aggro list.
    pub fn remove_target(&mut self, serial: u32) {
        self.targets.remove(&serial);
        if self.primary_target == Some(serial) {
            // Promote another target to primary, or clear.
            self.primary_target = self.targets.iter().next().copied();
        }
        if self.targets.is_empty() {
            self.charged = false;
            self.next_swing = Instant::now() + Self::FAR_FUTURE;
        }
    }

    /// Clear all targets and reset state.
    pub fn clear_all(&mut self) {
        self.targets.clear();
        self.primary_target = None;
        self.charged = false;
        self.next_swing = Instant::now() + Self::FAR_FUTURE;
    }

    /// Returns `true` if there are any targets in the aggro list.
    pub fn has_targets(&self) -> bool {
        !self.targets.is_empty()
    }

    // ── Charge management ────────────────────────────────────────────

    /// Start a charge cycle if one isn't already in progress.
    fn ensure_charging(&mut self) {
        if !self.charged && self.next_swing > Instant::now() + Self::FAR_FUTURE / 2 {
            // Timer is in far future — start charging now.
            self.next_swing = Instant::now() + self.cached_swing_delay;
        }
    }

    /// Begin a new charge cycle with the given delay.
    pub fn start_new_charge(&mut self, delay: Duration) {
        self.charged = false;
        self.cached_swing_delay = delay;
        self.next_swing = Instant::now() + delay;
    }

    /// Whether the weapon is currently ready (not away and not in recovery).
    pub fn is_weapon_ready(&self) -> bool {
        !self.weapon_away && Instant::now() >= self.recovery_until
    }

    /// Mark weapon as "away" (timed action started).
    pub fn set_weapon_away(&mut self) {
        self.weapon_away = true;
    }

    /// Mark weapon as returning to hand and start recovery delay.
    pub fn start_weapon_recovery(&mut self, delay: Duration) {
        self.weapon_away = false;
        self.recovery_until = Instant::now() + delay;
    }

    // ── Compat shims (used during transition) ────────────────────────

    /// Legacy: returns `true` if there is at least one target.
    /// Kept for melee swing guard in `tokio::select!`.
    #[allow(dead_code)]
    pub fn has_target(&self) -> bool {
        self.has_targets()
    }
}

// ── Weapon resolution ─────────────────────────────────────────────────────

/// Resolved weapon info for a single swing.
pub struct ResolvedWeapon {
    pub name: &'static str,
    pub damage_min: u16,
    pub damage_max: u16,
    pub hit_sound: u16,
    pub attack_anim: u16,
    /// Maximum Chebyshev distance this weapon can reach.
    pub range: u16,
    /// Time between swings in milliseconds.
    pub swing_delay_ms: u64,
    /// `true` if this is a fencing (piercing) weapon — only fencing weapons
    /// can carry poison.
    pub is_fencing: bool,
    /// Serial of the equipped weapon item (`0` for fists / no weapon).  Used
    /// to look up and consume poison charges in item props.
    pub item_serial: u32,
}

impl ResolvedWeapon {
    /// Unarmed / fist fallback.
    pub fn fist() -> Self {
        Self {
            name: "Fists",
            damage_min: melee::FIST_DAMAGE_MIN,
            damage_max: melee::FIST_DAMAGE_MAX,
            hit_sound: melee::FIST_SOUND,
            attack_anim: melee::FIST_ANIM,
            range: melee::MELEE_RANGE_1H,
            swing_delay_ms: melee::FIST_SWING_DELAY_MS,
            is_fencing: false,
            item_serial: 0,
        }
    }

    fn from_def(def: &'static WeaponDef, item_serial: u32) -> Self {
        Self {
            name: def.name,
            damage_min: def.damage_min,
            damage_max: def.damage_max,
            hit_sound: def.hit_sound,
            attack_anim: def.attack_anim,
            range: if def.two_handed { melee::MELEE_RANGE_2H } else { melee::MELEE_RANGE_1H },
            swing_delay_ms: def.swing_delay_ms,
            is_fencing: def.is_fencing(),
            item_serial,
        }
    }

    /// Swing delay as a `Duration`.
    pub fn swing_delay(&self) -> Duration {
        Duration::from_millis(self.swing_delay_ms)
    }
}

/// Find a weapon in the mobile's equipped items (RightHand or LeftHand)
/// and look it up in the weapon table.  Returns `(def, item_serial)` or
/// `None` if unarmed.
pub fn find_equipped_weapon(items: &[EquippedItem]) -> Option<(&'static WeaponDef, u32)> {
    for eq in items {
        if eq.layer == Layer::RightHand || eq.layer == Layer::LeftHand {
            if let Some(def) = weapon::lookup_weapon(eq.graphic) {
                return Some((def, eq.serial));
            }
        }
    }
    None
}

/// Resolve the weapon for a swing — either equipped or fist.
pub fn resolve_weapon(items: &[EquippedItem]) -> ResolvedWeapon {
    match find_equipped_weapon(items) {
        Some((def, serial)) => ResolvedWeapon::from_def(def, serial),
        None => ResolvedWeapon::fist(),
    }
}

// ── Damage calculation ────────────────────────────────────────────────────

/// Calculate melee damage: random(min, max) + STR bonus.
pub fn calc_melee_damage(str_: u16, weapon: &ResolvedWeapon) -> u16 {
    let base = random_range(weapon.damage_min, weapon.damage_max);
    let str_bonus = (str_ / 10).min(10);
    base + str_bonus
}

// ── Mount-aware animation helpers ─────────────────────────────────────────

fn is_mounted(items: &[EquippedItem]) -> bool {
    items.iter().any(|eq| eq.layer == Layer::Mount)
}

// ── SwingResult ───────────────────────────────────────────────────────────

/// Result of a melee swing attempt.
pub enum SwingResult {
    /// Swing hit the target.
    Hit {
        packets: Vec<RawPacket>,
        next_delay: Duration,
    },
    /// Dice-roll miss — attack animation played, miss sound scheduled.
    Miss {
        next_delay: Duration,
    },
    /// Target is within leash but out of weapon range or LOS.
    /// No animation, no sound.  Charge should be held.
    NotInRange,
    /// Target is dead, gone, or beyond leash range.
    /// The serial identifies which target to remove from the aggro list.
    Disengage {
        serial: u32,
    },
}

// ── try_swing ─────────────────────────────────────────────────────────────

/// Attempt a melee swing against a specific target.
///
/// This function is called by [`try_consume_charge`] after selecting the
/// best target.  It checks distance, LOS, resolves weapon, and either
/// lands a hit, misses, or reports the target is unreachable.
pub async fn try_swing(
    attacker_serial: u32,
    target_serial: u32,
    world: u8,
    worker_tx: &DemoWorkerTx,
) -> SwingResult {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // 1. Get attacker entity.
    let attacker = engine.get_entity(attacker_serial).await;
    let Some(m) = attacker.as_ref().and_then(|e| e.mobile()) else {
        return SwingResult::Disengage { serial: target_serial };
    };
    let (ax, ay, az) = (m.x, m.y, m.z);
    let str_ = m.str_;
    let items = m.items.clone();

    // 2. Get target entity.
    let target = engine.get_entity(target_serial).await;
    let Some(m) = target.as_ref().and_then(|e| e.mobile()) else {
        return SwingResult::Disengage { serial: target_serial };
    };
    let (tx, ty, tz) = (m.x, m.y, m.z);
    let target_mounted = is_mounted(&m.items);
    let target_graphic = m.graphic;
    let target_is_player = m.is_player;

    if m.hits == 0 {
        return SwingResult::Disengage { serial: target_serial };
    }

    // 3. Resolve weapon.
    let weapon = resolve_weapon(&items);
    let attacker_mounted = is_mounted(&items);

    // 4. Distance check.
    let dist = chebyshev(ax, ay, tx, ty);
    if dist > melee::LEASH_RANGE {
        return SwingResult::Disengage { serial: target_serial };
    }
    if dist > weapon.range {
        // In leash but out of weapon range — no animation, hold charge.
        return SwingResult::NotInRange;
    }

    // 5. LOS check.
    if !engine.check_los(
        ax, ay, az as i16 + crate::constants::EYE_HEIGHT,
        tx, ty, tz as i16 + crate::constants::EYE_HEIGHT,
    ).await {
        return SwingResult::NotInRange;
    }

    // 6. Miss chance roll.
    {
        use rand::Rng;
        let roll: u32 = rand::rng().random_range(0..100);
        if roll < melee::MISS_CHANCE_PCT {
            game_util::send_resolved_animation(
                worker_tx, world, attacker_serial, weapon.attack_anim,
                attacker_mounted, 5, 1, ax, ay,
            ).await;
            schedule_miss_sound(worker_tx, world, tx, ty, tz as i16);

            debug!(
                "[combat] 0x{:08X} missed 0x{:08X} with {}",
                attacker_serial, target_serial, weapon.name
            );
            return SwingResult::Miss {
                next_delay: weapon.swing_delay(),
            };
        }
    }

    // 7. Calculate raw damage.
    let raw_damage = calc_melee_damage(str_, &weapon);

    // 8. Armor reduction — roll a hit zone and apply AR if covered.
    let armor_profile = engine.query_equipment_armor(target_serial)
        .await
        .unwrap_or_default();

    let mut hit_zone = HitZone::roll();
    // If shield zone was rolled but target has no shield, redirect to chest.
    if hit_zone == HitZone::Shield && !armor_profile.has_shield {
        hit_zone = HitZone::Chest;
    }

    let zone_ar = hit_zone.zone_ar(&armor_profile);
    let damage = if zone_ar > 0 {
        raw_damage.saturating_sub(zone_ar).max(1)
    } else {
        raw_damage
    };

    let mut packets = Vec::new();
    packets.push(fight_occurring(attacker_serial, target_serial));

    // 9. Attack animation + get-hit animation.
    game_util::send_resolved_animation(
        worker_tx, world, attacker_serial, weapon.attack_anim,
        attacker_mounted, 5, 1, ax, ay,
    ).await;

    game_util::send_resolved_animation(
        worker_tx, world, target_serial, anim::GET_HIT,
        target_mounted, 5, 1, tx, ty,
    ).await;

    // 10. Sounds.
    game_util::send_sound(worker_tx, world, melee::SWING_SOUND, ax, ay, az as i16).await;
    game_util::send_sound(worker_tx, world, weapon.hit_sound, tx, ty, tz as i16).await;

    // 10a. Gender-aware hurt sound on the target.
    let hurt_snd = game_util::random_hurt_sound(target_graphic);
    game_util::send_sound(worker_tx, world, hurt_snd, tx, ty, tz as i16).await;

    // 10b. Shield block feedback — system message broadcast to target.
    if hit_zone == HitZone::Shield && armor_profile.has_shield && zone_ar > 0 {
        let _ = worker_tx.send(WorkerCommand::MapCommand(
            world,
            DemoCommand::BroadcastSpeech {
                serial: 0xFFFF_FFFF,
                graphic: 0xFFFF,
                speech_type: 0x06, // System
                color: crate::constants::hue::SYSTEM_GRAY,
                font: 3,
                name: String::new(),
                message: format!(
                    "Your shield absorbs some of the blow! (-{} damage)",
                    raw_damage.saturating_sub(damage),
                ),
                x: tx, y: ty,
            },
        )).await;
    }

    // 11. Deal damage.
    // Flag aggression first (only matters for player-vs-player): records the
    // aggressor relationship and may flag the attacker criminal.
    if target_is_player {
        engine.flag_aggression(attacker_serial, target_serial).await;
    }
    let mut target_killed = false;
    if let Some(result) = engine.deal_damage(target_serial, damage, attacker_serial).await {
        if result.killed {
            target_killed = true;
            // Record murder counts for player-vs-player kills.
            if target_is_player {
                engine.record_kill(attacker_serial, target_serial).await;
            }
            info!(
                "[combat] 0x{:08X} killed 0x{:08X} with {} ({} raw, {} after armor [{:?}])",
                attacker_serial, target_serial, weapon.name, raw_damage, damage, hit_zone
            );
            // Inject loot-table items into the auto-created corpse and schedule decay.
            // Player corpses are left intact (no monster loot, no decay) so the
            // player can recover their items on resurrection.
            if let Some(ref kill) = result.kill {
                if !target_is_player {
                    let loot = crate::loot::generate_loot_for_body(target_graphic);
                    if !loot.is_empty() {
                        engine.add_container_items(kill.corpse_serial, loot).await;
                    }
                    crate::game_util::schedule_corpse_decay(worker_tx, world, kill.corpse_serial);
                } else if let Some(ref mount) = kill.dropped_mount {
                    // Player died while mounted and the engine could not restore
                    // the saved mount NPC — spawn a default one from the graphic.
                    crate::game_util::spawn_mount_npc_on_death(
                        worker_tx, world, mount, kill.x, kill.y, kill.z,
                    ).await;
                }
            }
        } else {
            debug!(
                "[combat] 0x{:08X} hit 0x{:08X} with {} for {} damage ({} raw, {:?} zone, {} AR)",
                attacker_serial, target_serial, weapon.name, damage, raw_damage, hit_zone, zone_ar
            );
        }
    }

    // 11a. Poisoned weapon — consume a charge and (chance) poison the target.
    // Only fencing weapons can carry poison.  Charges are consumed on every
    // landed hit; if the target was just killed we still consume the charge
    // but skip applying poison to the corpse.
    if weapon.is_fencing && weapon.item_serial != 0 {
        apply_weapon_poison(
            attacker_serial, target_serial, weapon.item_serial,
            target_killed, world, worker_tx,
        ).await;
    }

    // 12. Consume stamina.
    let _ = engine.modify_stamina(attacker_serial, -(melee::STAMINA_COST as i32)).await;

    SwingResult::Hit {
        packets,
        next_delay: weapon.swing_delay(),
    }
}

// ── Weapon poison ──────────────────────────────────────────────────────────

/// Consume one poison charge from a fencing weapon and, on a level-scaled
/// chance, poison the target.
///
/// Charges live in the weapon's `ItemProps.meta`
/// (`poison_charges` / `poison_level`).  When the last charge is used the
/// poison meta is removed so the weapon becomes "clean" again.  No effect if
/// the weapon has no charges.  Poison is not applied if the target was just
/// killed (no point poisoning a corpse), but the charge is still consumed.
async fn apply_weapon_poison(
    attacker_serial: u32,
    target_serial: u32,
    weapon_serial: u32,
    target_killed: bool,
    world: u8,
    worker_tx: &DemoWorkerTx,
) {
    use crate::constants::poison as poison_cfg;
    use crate::game_session::poison::{META_POISON_CHARGES, META_POISON_LEVEL};
    use common::uo_engine::item_props::MetaValue;

    let engine = crate::game_util::engine_for(worker_tx, world);

    // Read the weapon's poison state.
    let Some(mut props) = engine.get_item_props(weapon_serial).await else {
        return;
    };
    let charges = props.get_meta_int(META_POISON_CHARGES).unwrap_or(0);
    if charges <= 0 {
        return;
    }
    let level = props.get_meta_int(META_POISON_LEVEL).unwrap_or(0) as u8;
    let Some(idx) = poison_cfg::level_index(level) else {
        // Corrupt state — clear it.
        props.remove_meta(META_POISON_CHARGES);
        props.remove_meta(META_POISON_LEVEL);
        engine.set_item_props(weapon_serial, Some(props)).await;
        return;
    };

    // Consume one charge.
    let remaining = charges - 1;
    if remaining <= 0 {
        props.remove_meta(META_POISON_CHARGES);
        props.remove_meta(META_POISON_LEVEL);
    } else {
        props.set_meta(META_POISON_CHARGES, MetaValue::Int(remaining));
    }
    engine.set_item_props(weapon_serial, Some(props)).await;

    if target_killed {
        return;
    }

    // Roll the application chance.
    let chance = poison_cfg::APPLY_CHANCE_PCT[idx];
    let roll = random_range(1, 100) as u32;
    if roll > chance {
        return;
    }

    // Apply poison to the target via the engine (works for players and NPCs).
    engine.apply_poison(
        target_serial,
        level,
        poison_cfg::DURATION_MS[idx],
        poison_cfg::DAMAGE_PER_TICK[idx],
        poison_cfg::TICK_INTERVAL_MS,
        attacker_serial,
    ).await;

    // Feedback: poison sound + a "looks ill" emote at the target.
    let target = engine.get_entity(target_serial).await;
    if let Some(m) = target.as_ref().and_then(|e| e.mobile()) {
        game_util::send_sound(worker_tx, world, poison_cfg::APPLY_SOUND, m.x, m.y, m.z as i16).await;
        let _ = worker_tx.send(WorkerCommand::MapCommand(
            world,
            DemoCommand::BroadcastSpeech {
                serial: 0xFFFF_FFFF,
                graphic: 0xFFFF,
                speech_type: 0x06, // System
                color: crate::constants::hue::SYSTEM_GRAY,
                font: 3,
                name: String::new(),
                message: format!("{} looks ill.", if m.name.is_empty() { "The creature" } else { &m.name }),
                x: m.x, y: m.y,
            },
        )).await;
    }

    debug!(
        "[poison] 0x{:08X} poisoned 0x{:08X} (level {}) via weapon 0x{:08X}",
        attacker_serial, target_serial, level, weapon_serial,
    );
}

// ── try_consume_charge ────────────────────────────────────────────────────

/// Result of [`try_consume_charge`] — tells the caller what happened.
pub enum ChargeResult {
    /// Nothing happened (not charged, no targets, or targets out of range).
    Idle,
    /// Charge was consumed and a strike was attempted.  May contain packets
    /// to send to the session (e.g. `FightOccurring`).
    Consumed {
        packets: Vec<RawPacket>,
    },
    /// One or more targets should be disengaged (dead / out of leash).
    /// The caller must remove them and may need to send `AttackResponse(0)`.
    Disengaged {
        serials: Vec<u32>,
    },
}

/// Try to use a ready weapon charge.
///
/// Called from three places:
/// 1. Swing timer fires (charge just became ready).
/// 2. Player moved (may now be in range).
/// 3. A target moved (`WorldEvent::EntityMoved`).
pub async fn try_consume_charge(
    attacker_serial: u32,
    world: u8,
    combat_state: &mut CombatState,
    worker_tx: &DemoWorkerTx,
) -> ChargeResult {
    if !combat_state.charged || combat_state.targets.is_empty() {
        return ChargeResult::Idle;
    }

    // Try to find a target in range — primary first, then closest.
    let target_serial = match find_best_target(
        attacker_serial, world, combat_state, worker_tx,
    ).await {
        FindTargetResult::Found(serial) => serial,
        FindTargetResult::NoneInRange => {
            // Charge held — no one reachable.
            return ChargeResult::Idle;
        }
        FindTargetResult::Disengaged(serials) => {
            // Some targets should be removed but none were in range.
            return ChargeResult::Disengaged { serials };
        }
    };

    // Target is in range — can we strike?
    if !combat_state.is_weapon_ready() {
        // Weapon is away or in recovery — charge wasted.
        let away = combat_state.weapon_away;
        let recovery_left = combat_state.recovery_until.saturating_duration_since(Instant::now());
        debug!(
            "[combat] charge wasted: weapon_away={}, recovery_left={:?}, target=0x{:08X}",
            away, recovery_left, target_serial,
        );
        let msg = if away {
            format!(
                "[debug] Charge wasted: weapon away (casting/healing), target=0x{:08X}",
                target_serial,
            )
        } else {
            format!(
                "[debug] Charge wasted: rearm {:.0?} left, target=0x{:08X}",
                recovery_left, target_serial,
            )
        };
        combat_state.start_new_charge(combat_state.cached_swing_delay);
        return ChargeResult::Consumed {
            packets: vec![crate::game_util::system_message_gray(&msg)],
        };
    }

    // STRIKE!
    combat_state.charged = false;
    match try_swing(attacker_serial, target_serial, world, worker_tx).await {
        SwingResult::Hit { packets, next_delay } => {
            combat_state.start_new_charge(next_delay);
            ChargeResult::Consumed { packets }
        }
        SwingResult::Miss { next_delay } => {
            combat_state.start_new_charge(next_delay);
            // Miss sound already scheduled inside try_swing.
            ChargeResult::Consumed { packets: vec![] }
        }
        SwingResult::NotInRange => {
            // Shouldn't happen (find_best_target checked), but be safe.
            combat_state.charged = true;
            ChargeResult::Idle
        }
        SwingResult::Disengage { serial } => {
            combat_state.start_new_charge(combat_state.cached_swing_delay);
            ChargeResult::Disengaged { serials: vec![serial] }
        }
    }
}

// ── find_best_target ──────────────────────────────────────────────────────

enum FindTargetResult {
    /// A reachable target was found.
    Found(u32),
    /// No target is in weapon range (but all are alive and within leash).
    NoneInRange,
    /// Some targets should be disengaged.  None were in range.
    Disengaged(Vec<u32>),
}

/// Find the best target to strike from the aggro list.
///
/// 1. Queries the attacker's position and weapon.
/// 2. Iterates all targets, checking distance (but not LOS — that's
///    done in `try_swing` for the selected target).
/// 3. Returns `primary_target` if in range, otherwise the closest.
async fn find_best_target(
    attacker_serial: u32,
    world: u8,
    combat_state: &CombatState,
    worker_tx: &DemoWorkerTx,
) -> FindTargetResult {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Get attacker info.
    let attacker = engine.get_entity(attacker_serial).await;
    let Some(m) = attacker.as_ref().and_then(|e| e.mobile()) else {
        return FindTargetResult::NoneInRange;
    };
    let (ax, ay) = (m.x, m.y);
    let weapon = resolve_weapon(&m.items);

    let mut best: Option<(u32, u16)> = None; // (serial, distance)
    let mut to_disengage = Vec::new();

    // Query all targets.  We do sequential queries since there are at
    // most MAX_AGGRO_TARGETS (8) and each is a cheap in-process RPC.
    for &serial in &combat_state.targets {
        let entity = engine.get_entity(serial).await;
        if let Some(m) = entity.as_ref().and_then(|e| e.mobile()) {
            if m.hits == 0 {
                to_disengage.push(serial);
                continue;
            }
            let dist = chebyshev(ax, ay, m.x, m.y);
            if dist > melee::LEASH_RANGE {
                to_disengage.push(serial);
                continue;
            }
            if dist <= weapon.range {
                // Primary target gets absolute priority.
                if combat_state.primary_target == Some(serial) {
                    return FindTargetResult::Found(serial);
                }
                // Track closest.
                if best.map_or(true, |(_, d)| dist < d) {
                    best = Some((serial, dist));
                }
            }
        } else {
            // Entity doesn't exist or isn't a mobile.
            to_disengage.push(serial);
        }
    }

    if let Some((serial, _)) = best {
        return FindTargetResult::Found(serial);
    }

    if !to_disengage.is_empty() {
        FindTargetResult::Disengaged(to_disengage)
    } else {
        FindTargetResult::NoneInRange
    }
}

// ── Delayed miss sound ────────────────────────────────────────────────────

/// Schedule a miss sound to play after [`melee::MISS_SOUND_DELAY_MS`].
pub fn schedule_miss_sound(
    worker_tx: &DemoWorkerTx,
    world: u8,
    x: u16,
    y: u16,
    z: i16,
) {
    use crate::constants::sound;

    let tx = worker_tx.clone();
    let delay = Duration::from_millis(melee::MISS_SOUND_DELAY_MS);

    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        game_util::send_sound(&tx, world, sound::MISS_1, x, y, z).await;
    });
}

// ── War mode packet helpers ───────────────────────────────────────────────

/// Build a `WarMode` S2C confirmation packet.
pub fn war_mode_response(fighting: bool) -> RawPacket {
    use packets::system::WarMode;
    RawPacket::s2c(encode_packet(&WarMode::new(fighting)))
}

/// Build an `AttackResponse` S2C packet confirming the attack target.
pub fn attack_response(target_serial: u32) -> RawPacket {
    use packets::interaction::AttackResponse;
    let pkt = if target_serial == 0 {
        AttackResponse::refused()
    } else {
        AttackResponse { id: AttackResponse::ID, serial: target_serial }
    };
    RawPacket::s2c(encode_packet(&pkt))
}

/// Build a `FightOccurring` S2C packet.
pub fn fight_occurring(attacker: u32, defender: u32) -> RawPacket {
    use packets::interaction::FightOccurring;
    RawPacket::s2c(encode_packet(&FightOccurring::new(attacker, defender)))
}
