//! Magic system for the demo server.
//!
//! Provides spell definitions, mana costs, casting logic, and the target
//! resolution flow (target cursor → spell execution).
//!
//! The casting flow is split into two phases:
//! - [`begin_cast`] — consumes mana, plays spell words / cast animation / sound.
//!   Returns session-directed packets and the cast delay (from `SpellDef::cast_delay`).
//!   The caller stores the spell in an [`ActiveAction`](crate::actions::ActiveAction)
//!   and starts a timer.
//! - [`complete_cast`] — re-checks LOS, sends projectile / effect / damage / heal.
//!
//! Currently implements six spells:
//! - **Magic Arrow** (spell 5, 1st circle) — damage projectile
//! - **Heal** (spell 4, 1st circle) — self/target heal
//! - **Greater Heal** (spell 29, 4th circle) — stronger heal
//! - **Lightning** (spell 30, 4th circle) — damage + lightning bolt
//! - **Energy Bolt** (spell 42, 6th circle) — heavy damage projectile
//! - **Flamestrike** (spell 51, 7th circle) — flamestrike damage

use std::time::Duration;

use log::{debug, info};
use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};
use packets::layer::Layer;

use framework::continuum::WorkerCommand;
use common::uo_engine::entity::DemoEntity;
use packets::interaction::TargetCursor;

use crate::constants::{anim, effect, hue, item, sound};
use crate::game_util::{self, random_range};
use crate::buffs::BuffKind;
use crate::{DemoCommand, DemoWorkerTx};

// ── Spell definitions ─────────────────────────────────────────────────────

/// UO spell IDs (1-indexed, matching the client spellbook).
#[allow(dead_code)]
pub mod spell_id {
    pub const CLUMSY: u16 = 1;
    pub const CREATE_FOOD: u16 = 2;
    pub const FEEBLEMIND: u16 = 3;
    pub const HEAL: u16 = 4;
    pub const MAGIC_ARROW: u16 = 5;
    pub const NIGHT_SIGHT: u16 = 6;
    pub const REACTIVE_ARMOR: u16 = 7;
    pub const WEAKEN: u16 = 8;
    // 3rd circle
    pub const BLESS: u16 = 17;
    // 4th circle
    pub const CURSE: u16 = 27;
    pub const GREATER_HEAL: u16 = 29;
    pub const LIGHTNING: u16 = 30;
    pub const RECALL: u16 = 32;
    pub const WALL_OF_STONE: u16 = 24;
    // 6th circle
    pub const MARK: u16 = 45;
    pub const ENERGY_BOLT: u16 = 42;
    // 7th circle
    pub const FLAMESTRIKE: u16 = 51;
}

/// Information about a spell definition.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct SpellDef {
    pub id: u16,
    pub name: &'static str,
    /// Mana cost.
    pub mana: u16,
    /// Minimum damage (0 for non-damage spells).
    pub damage_min: u16,
    /// Maximum damage.
    pub damage_max: u16,
    /// Heal amount (0 for non-heal spells).
    pub heal_min: u16,
    pub heal_max: u16,
    /// Circle (1-8).
    pub circle: u8,
    /// Cast delay in milliseconds (measured from the original UO server).
    pub cast_delay_ms: u64,
    /// Does this spell need a target?
    pub needs_target: bool,
    /// Can target self?
    pub can_self: bool,
    /// Is this a harmful spell?
    pub harmful: bool,
    /// Projectile effect graphic (0 = no projectile).
    pub projectile_graphic: u16,
    /// On-target effect graphic (0 = none).
    pub target_effect: u16,
    /// Sound effect when cast (0 = silent).
    pub cast_sound: u16,
    /// Sound effect on impact (0 = silent).
    pub impact_sound: u16,
    /// Cast animation action ID (0 = no animation).
    /// See [`anim::CAST_DIRECTED`], [`anim::CAST_AREA`].
    pub cast_action: u16,
    /// Spell words shown overhead during cast. `None` = silent cast.
    pub words: Option<&'static str>,
    /// If `true`, a lightning bolt effect (direction_type=1) is played on target.
    pub lightning_bolt: bool,
    /// Speed parameter for the on-target effect (0 = default).
    pub target_effect_speed: u8,
    /// Duration parameter for the on-target effect (0 = default).
    pub target_effect_duration: u8,
    /// Scroll item graphic for this spell (0 = no scroll exists).
    pub scroll_graphic: u16,
    /// Cast delay when cast from a scroll (ms). 0 = same as `cast_delay_ms`.
    pub scroll_cast_delay_ms: u64,
    /// Reagent graphic IDs required for this spell (empty = no reagents).
    /// Each entry is consumed once per cast (from the caster's backpack).
    pub reagents: &'static [u16],
    /// Stat buff/debuff effect applied on cast completion.
    ///
    /// `None` for most spells. `Some(...)` for Bless, Curse, etc.
    pub stat_effect: Option<SpellStatEffect>,
}

/// Describes a stat modification applied by a spell (Bless, Curse, etc.).
#[derive(Debug, Clone, Copy)]
pub struct SpellStatEffect {
    /// The buff kind to register in the session's buff tracker.
    pub buff_kind: BuffKind,
    /// Delta applied to each stat (positive = boost, negative = debuff).
    pub delta: i16,
    /// Duration of the effect in milliseconds.
    pub duration_ms: u64,
}

impl SpellDef {
    /// Cast delay for reagent-based casting.
    pub fn cast_delay(&self) -> Duration {
        Duration::from_millis(self.cast_delay_ms)
    }

    /// Cast delay accounting for scroll vs reagent casting.
    ///
    /// When `from_scroll` is `true` and `scroll_cast_delay_ms > 0`,
    /// uses the scroll-specific delay; otherwise falls back to `cast_delay_ms`.
    pub fn effective_cast_delay(&self, from_scroll: bool) -> Duration {
        if from_scroll && self.scroll_cast_delay_ms > 0 {
            Duration::from_millis(self.scroll_cast_delay_ms)
        } else {
            Duration::from_millis(self.cast_delay_ms)
        }
    }

    /// Resolve the cast animation action ID, accounting for mount state.
    ///
    /// Delegates to [`game_util::resolve_animation`] — the central
    /// mount-aware resolver.  Returns `None` if the animation should be
    /// skipped (not expected for cast actions, but handled for completeness).
    pub fn resolved_cast_action(&self, is_mounted: bool) -> Option<u16> {
        crate::game_util::resolve_animation(self.cast_action, is_mounted)
    }
}

/// Return all spell definitions.
pub fn all_spells() -> &'static [SpellDef] {
    SPELLS
}

/// Look up a spell definition by its ID.
pub fn get_spell(id: u16) -> Option<&'static SpellDef> {
    SPELLS.iter().find(|s| s.id == id)
}

/// Look up a spell definition by its scroll item graphic.
///
/// Returns `None` if the graphic does not correspond to any known spell scroll.
pub fn get_spell_by_scroll(graphic: u16) -> Option<&'static SpellDef> {
    SPELLS.iter().find(|s| s.scroll_graphic != 0 && s.scroll_graphic == graphic)
}

static SPELLS: &[SpellDef] = &[
    SpellDef {
        id: spell_id::MAGIC_ARROW,
        name: "Magic Arrow",
        mana: 4,
        damage_min: 10,
        damage_max: 15,
        heal_min: 0,
        heal_max: 0,
        circle: 1,
        cast_delay_ms: 1100,
        needs_target: true,
        can_self: false,
        harmful: true,
        projectile_graphic: effect::MAGIC_ARROW,
        target_effect: 0,
        cast_sound: sound::MAGIC_ARROW,
        impact_sound: sound::MAGIC_ARROW,
        cast_action: anim::CAST_DIRECTED,
        words: Some("In Por Ylem"),
        lightning_bolt: false,
        target_effect_speed: 0,
        target_effect_duration: 0,
        scroll_graphic: item::SCROLL_MAGIC_ARROW,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_BLACK_PEARL, item::REAGENT_NIGHTSHADE, item::REAGENT_SULPHUROUS_ASH],
        stat_effect: None,
    },
    SpellDef {
        id: spell_id::HEAL,
        name: "Heal",
        mana: 4,
        damage_min: 0,
        damage_max: 0,
        heal_min: 15,
        heal_max: 25,
        circle: 1,
        cast_delay_ms: 1100,
        needs_target: true,
        can_self: true,
        harmful: false,
        projectile_graphic: 0,
        target_effect: effect::HEAL_SPARKLE,
        cast_sound: sound::BLESS,
        impact_sound: sound::BLESS,
        cast_action: anim::CAST_DIRECTED,
        words: Some("In Mani"),
        lightning_bolt: false,
        target_effect_speed: 7,
        target_effect_duration: 16,
        scroll_graphic: item::SCROLL_HEAL,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_GARLIC, item::REAGENT_GINSENG, item::REAGENT_SPIDERS_SILK],
        stat_effect: None,
    },
    SpellDef {
        id: spell_id::GREATER_HEAL,
        name: "Greater Heal",
        mana: 10,
        damage_min: 0,
        damage_max: 0,
        heal_min: 30,
        heal_max: 50,
        circle: 4,
        cast_delay_ms: 2700,
        needs_target: true,
        can_self: true,
        harmful: false,
        projectile_graphic: 0,
        target_effect: effect::HEAL_SPARKLE,
        cast_sound: sound::BLESS,
        impact_sound: sound::BLESS,
        cast_action: anim::CAST_DIRECTED,
        words: Some("In Vas Mani"),
        lightning_bolt: false,
        target_effect_speed: 7,
        target_effect_duration: 16,
        scroll_graphic: item::SCROLL_GREATER_HEAL,
        scroll_cast_delay_ms: 1100,
        reagents: &[item::REAGENT_GARLIC, item::REAGENT_GINSENG, item::REAGENT_MANDRAKE_ROOT, item::REAGENT_SPIDERS_SILK],
        stat_effect: None,
    },
    SpellDef {
        id: spell_id::LIGHTNING,
        name: "Lightning",
        mana: 11,
        damage_min: 15,
        damage_max: 25,
        heal_min: 0,
        heal_max: 0,
        circle: 4,
        cast_delay_ms: 1150,
        needs_target: true,
        can_self: false,
        harmful: true,
        projectile_graphic: 0,
        target_effect: 0,
        cast_sound: sound::LIGHTNING,
        impact_sound: sound::LIGHTNING,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Por Ort Grav"),
        lightning_bolt: true,
        target_effect_speed: 0,
        target_effect_duration: 0,
        scroll_graphic: item::SCROLL_LIGHTNING,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_MANDRAKE_ROOT, item::REAGENT_SULPHUROUS_ASH],
        stat_effect: None,
    },
    SpellDef {
        id: spell_id::ENERGY_BOLT,
        name: "Energy Bolt",
        mana: 20,
        damage_min: 25,
        damage_max: 40,
        heal_min: 0,
        heal_max: 0,
        circle: 6,
        cast_delay_ms: 2200,
        needs_target: true,
        can_self: false,
        harmful: true,
        projectile_graphic: effect::ENERGY_BOLT,
        target_effect: 0,
        cast_sound: sound::ENERGY_BOLT,
        impact_sound: sound::ENERGY_BOLT,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Corp Por"),
        lightning_bolt: false,
        target_effect_speed: 0,
        target_effect_duration: 0,
        scroll_graphic: item::SCROLL_ENERGY_BOLT,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_BLACK_PEARL, item::REAGENT_NIGHTSHADE],
        stat_effect: None,
    },
    SpellDef {
        id: spell_id::FLAMESTRIKE,
        name: "Flamestrike",
        mana: 20,
        damage_min: 30,
        damage_max: 50,
        heal_min: 0,
        heal_max: 0,
        circle: 7,
        cast_delay_ms: 2200,
        needs_target: true,
        can_self: false,
        harmful: true,
        projectile_graphic: 0,
        target_effect: effect::FLAMESTRIKE,
        cast_sound: sound::FLAMESTRIKE,
        impact_sound: sound::FLAMESTRIKE,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Kal Vas Flam"),
        lightning_bolt: false,
        target_effect_speed: 10,
        target_effect_duration: 30,
        scroll_graphic: item::SCROLL_FLAMESTRIKE,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_SPIDERS_SILK, item::REAGENT_SULPHUROUS_ASH],
        stat_effect: None,
    },
    // ── Bless (3rd circle, spell 17) ─────────────────────────────────
    //
    // Temporarily increases all three stats (STR, DEX, INT) of the target.
    // Classic UO: duration ~60–120 s depending on Magery, bonus ~1–10.
    SpellDef {
        id: spell_id::BLESS,
        name: "Bless",
        mana: 9,
        damage_min: 0,
        damage_max: 0,
        heal_min: 0,
        heal_max: 0,
        circle: 3,
        cast_delay_ms: 1500,
        needs_target: true,
        can_self: true,
        harmful: false,
        projectile_graphic: 0,
        target_effect: effect::HEAL_SPARKLE,
        cast_sound: sound::BLESS,
        impact_sound: sound::BLESS,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Rel Sanct"),
        lightning_bolt: false,
        target_effect_speed: 7,
        target_effect_duration: 16,
        scroll_graphic: item::SCROLL_BLESS,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_GARLIC, item::REAGENT_MANDRAKE_ROOT],
        stat_effect: Some(SpellStatEffect {
            buff_kind: BuffKind::Bless,
            delta: 10,
            duration_ms: 120_000,
        }),
    },
    // ── Curse (4th circle, spell 19) ─────────────────────────────────
    //
    // Temporarily decreases all three stats (STR, DEX, INT) of the target.
    SpellDef {
        id: spell_id::CURSE,
        name: "Curse",
        mana: 11,
        damage_min: 0,
        damage_max: 0,
        heal_min: 0,
        heal_max: 0,
        circle: 4,
        cast_delay_ms: 1500,
        needs_target: true,
        can_self: false,
        harmful: true,
        projectile_graphic: 0,
        target_effect: effect::FLAMESTRIKE,
        cast_sound: sound::CURSE,
        impact_sound: sound::CURSE,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Des Sanct"),
        lightning_bolt: false,
        target_effect_speed: 10,
        target_effect_duration: 20,
        scroll_graphic: item::SCROLL_CURSE,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_GARLIC, item::REAGENT_NIGHTSHADE, item::REAGENT_SULPHUROUS_ASH],
        stat_effect: Some(SpellStatEffect {
            buff_kind: BuffKind::Curse,
            delta: -10,
            duration_ms: 60_000,
        }),
    },
    // ── Recall (4th circle, spell 32) ────────────────────────────────
    //
    // Teleports the caster to the location stored on a *marked* rune.
    // Resolution is handled outside `complete_cast` (see
    // `game_session::recall`) because the target is an item in the
    // caster's backpack, not a world entity.
    SpellDef {
        id: spell_id::RECALL,
        name: "Recall",
        mana: 11,
        damage_min: 0,
        damage_max: 0,
        heal_min: 0,
        heal_max: 0,
        circle: 4,
        cast_delay_ms: 1500,
        needs_target: true,
        can_self: false,
        harmful: false,
        projectile_graphic: 0,
        target_effect: 0,
        cast_sound: sound::TELEPORT,
        impact_sound: sound::TELEPORT,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Kal Ort Por"),
        lightning_bolt: false,
        target_effect_speed: 0,
        target_effect_duration: 0,
        scroll_graphic: 0,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_BLACK_PEARL, item::REAGENT_BLOOD_MOSS, item::REAGENT_MANDRAKE_ROOT],
        stat_effect: None,
    },
    // ── Mark (6th circle, spell 45) ──────────────────────────────────
    //
    // Stores the caster's current location on a blank rune in their
    // backpack.  Resolution is handled in `game_session::recall`.
    SpellDef {
        id: spell_id::MARK,
        name: "Mark",
        mana: 20,
        damage_min: 0,
        damage_max: 0,
        heal_min: 0,
        heal_max: 0,
        circle: 6,
        cast_delay_ms: 2200,
        needs_target: true,
        can_self: false,
        harmful: false,
        projectile_graphic: 0,
        target_effect: 0,
        cast_sound: sound::TELEPORT,
        impact_sound: sound::TELEPORT,
        cast_action: anim::CAST_DIRECTED,
        words: Some("Kal Por Ylem"),
        lightning_bolt: false,
        target_effect_speed: 0,
        target_effect_duration: 0,
        scroll_graphic: 0,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_BLACK_PEARL, item::REAGENT_BLOOD_MOSS, item::REAGENT_MANDRAKE_ROOT],
        stat_effect: None,
    },
    // ── Wall of Stone (4th circle, spell 24) ─────────────────────────
    //
    // Conjures a row of 6 stone blocks (graphic 0x0080) at a chosen ground
    // tile.  The blocks are plain ground items that decay one-by-one after a
    // delay.  Resolution is handled outside `complete_cast` (see
    // `complete_wall_of_stone`) because the target is a ground tile, not a
    // world entity.
    SpellDef {
        id: spell_id::WALL_OF_STONE,
        name: "Wall of Stone",
        mana: 9,
        damage_min: 0,
        damage_max: 0,
        heal_min: 0,
        heal_max: 0,
        circle: 4,
        cast_delay_ms: 1500,
        needs_target: true,
        can_self: false,
        harmful: false,
        projectile_graphic: 0,
        target_effect: 0,
        cast_sound: 0,
        impact_sound: 0,
        cast_action: anim::CAST_AREA,
        words: Some("In Sanct Ylem"),
        lightning_bolt: false,
        target_effect_speed: 0,
        target_effect_duration: 0,
        scroll_graphic: 0,
        scroll_cast_delay_ms: 0,
        reagents: &[item::REAGENT_BLOOD_MOSS, item::REAGENT_GARLIC],
        stat_effect: None,
    },
];

// ── Spell execution ───────────────────────────────────────────────────────

/// State for a pending spell cast (waiting for target cursor response).
#[derive(Debug, Clone)]
pub struct PendingSpell {
    pub spell: &'static SpellDef,
    pub caster_serial: u32,
    pub cursor_id: u32,
    /// If `Some`, the cast was initiated from a scroll double-click.
    pub scroll_item_serial: Option<u32>,
}

/// Cursor ID base for spell targeting.
pub const SPELL_CURSOR_BASE: u32 = 0xDEAD_0000;

/// Create a target cursor for a spell and return the pending state.
pub fn begin_spell_target(
    spell: &'static SpellDef,
    caster_serial: u32,
    scroll_item_serial: Option<u32>,
) -> (PendingSpell, RawPacket) {
    let cursor_id = SPELL_CURSOR_BASE | (spell.id as u32);
    let cursor_type = if spell.harmful { 1u8 } else { 2u8 }; // 1=harmful, 2=helpful

    let tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 0, // object target
        cursor_id,
        cursor_type,
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    let pending = PendingSpell {
        spell,
        caster_serial,
        cursor_id,
        scroll_item_serial,
    };

    (pending, RawPacket::s2c(encode_packet(&tc)))
}

/// Cursor ID used for Mark / Recall rune targeting.
///
/// Matches the reference UO behaviour (`cursor_id = 2`, neutral cursor)
/// rather than the [`SPELL_CURSOR_BASE`] scheme used by combat / heal
/// spells.
pub const RUNE_CURSOR_ID: u32 = 2;

/// Create a target cursor for a rune spell (Mark / Recall).
///
/// Returns the pending state plus two packets: a "Select a rune…" prompt
/// (`SendSpeech` from System) and the neutral target cursor.
pub fn begin_rune_spell_target(
    spell: &'static SpellDef,
    caster_serial: u32,
) -> (PendingSpell, RawPacket, RawPacket) {
    let prompt = match spell.id {
        spell_id::MARK => "Select a rune to mark.",
        spell_id::RECALL => "Select a rune to recall from.",
        _ => "Select a rune.",
    };

    let tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 0, // object target
        cursor_id: RUNE_CURSOR_ID,
        cursor_type: 0, // neutral
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    let pending = PendingSpell {
        spell,
        caster_serial,
        cursor_id: RUNE_CURSOR_ID,
        scroll_item_serial: None,
    };

    (
        pending,
        game_util::system_speech(prompt),
        RawPacket::s2c(encode_packet(&tc)),
    )
}

/// Create a **ground** target cursor for the Wall of Stone spell.
///
/// Unlike [`begin_spell_target`] (which sends an object cursor that resolves
/// against a world entity), this sends a ground/tile cursor (`cursor_target =
/// 1`).  The client reports the picked tile's `x`/`y`/`z` in its 0x6C response
/// rather than a `target_serial`.
pub fn begin_wall_target(
    spell: &'static SpellDef,
    caster_serial: u32,
) -> (PendingSpell, RawPacket) {
    let cursor_id = SPELL_CURSOR_BASE | (spell.id as u32);

    let tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 1, // ground/tile target
        cursor_id,
        cursor_type: 0, // neutral
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    let pending = PendingSpell {
        spell,
        caster_serial,
        cursor_id,
        scroll_item_serial: None,
    };

    (pending, RawPacket::s2c(encode_packet(&tc)))
}


///
/// This is the main spell resolution function. It:
/// 1. Consumes mana from the caster
/// 2. Plays cast animation + sound
/// 3. Sends projectile/effect
/// 4. Deals damage or heals
///
/// Returns a list of packets to send directly to the caster session,
/// plus engine commands are sent via worker_tx.
///
/// **Deprecated** — prefer [`begin_cast`] + [`complete_cast`] for new code.
/// Kept for Lua / NPC paths that don't use the action system.
#[allow(dead_code)]
pub async fn execute_spell(
    spell: &SpellDef,
    caster_serial: u32,
    target_serial: u32,
    worker_tx: &DemoWorkerTx,
    world: u8,
) -> Vec<RawPacket> {
    let mut packets = Vec::new();

    // Phase 1 — begin (mana + LOS + animation).
    let begin_pkts = begin_cast(spell, caster_serial, target_serial, worker_tx, world, false).await;
    packets.extend(begin_pkts.packets);
    if !begin_pkts.ok {
        return packets;
    }

    // Phase 2 — complete (LOS, effects, damage/heal).
    let result = complete_cast(spell, caster_serial, target_serial, worker_tx, world, None).await;
    packets.extend(result.packets);
    // Note: stat_effect is ignored in the deprecated path (no BuffState access).

    packets
}

// ── Two-phase casting (used by the action system) ─────────────────────────

/// Result of [`begin_cast`].
pub struct BeginCastResult {
    /// Packets to send to the caster's session (e.g. "Insufficient mana.").
    pub packets: Vec<RawPacket>,
    /// `true` if the cast began successfully (mana consumed, animation played).
    pub ok: bool,
}

/// **Phase 1**: consume mana, check LOS, play spell words + cast animation + sound.
///
/// Call this when the player initiates a spell.  If successful, store the
/// spell data in an [`ActiveAction`](crate::actions::ActiveAction) and start
/// a timer for [`SpellDef::cast_delay`].
///
/// When `from_scroll` is `true`, mana is not consumed here — scroll casts
/// do not cost mana.
pub async fn begin_cast(
    spell: &SpellDef,
    caster_serial: u32,
    target_serial: u32,
    worker_tx: &DemoWorkerTx,
    world: u8,
    from_scroll: bool,
) -> BeginCastResult {
    let mut packets = Vec::new();
    let engine = crate::game_util::engine_for(worker_tx, world);

    // 0. Check reagent availability in backpack (skipped for scroll casts).
    //    Actual consumption happens in complete_cast on success.
    if !from_scroll && !spell.reagents.is_empty() {
        if find_reagent_items(caster_serial, spell.reagents, worker_tx, world).await.is_none() {
            packets.push(game_util::system_speech("Insufficient reagents."));
            return BeginCastResult { packets, ok: false };
        }
    }

    // 1. Line-of-sight check (skip for self-target).
    if target_serial != caster_serial {
        let caster = engine.get_entity(caster_serial).await;
        let target = engine.get_entity(target_serial).await;

        if let (
            Some(m),
            Some(target_ent),
        ) = (caster.as_ref().and_then(|e| e.mobile()), &target) {
            let (tx, ty, tz) = match target_ent {
                DemoEntity::Mobile(tm) => (tm.x, tm.y, tm.z),
                DemoEntity::Item { x, y, z, .. } => (*x, *y, *z),
                _ => {
                    packets.push(game_util::system_speech("Invalid target."));
                    return BeginCastResult { packets, ok: false };
                }
            };

            const EYE_H: i16 = crate::constants::EYE_HEIGHT;
            if !engine.check_los(
                m.x, m.y, m.z as i16 + EYE_H,
                tx, ty, tz as i16 + EYE_H,
            ).await {
                packets.push(game_util::system_speech("Target cannot be seen."));
                return BeginCastResult { packets, ok: false };
            }
        }
    }

    // 2. Mana handling.
    //    - Regs cast:   only CHECK mana availability (consumed in complete_cast).
    //    - Scroll cast: CHECK full mana, consume upfront half now
    //                   (remaining half in complete_cast).
    {
        // Both paths need at least spell.mana available.
        let entity = engine.get_entity(caster_serial).await;
        let current_mana = match entity.as_ref().and_then(|e| e.mobile()) {
            Some(m) => m.mana,
            _ => 0,
        };
        if current_mana < spell.mana {
            packets.push(game_util::system_speech("Insufficient mana."));
            return BeginCastResult { packets, ok: false };
        }
    }

    if from_scroll {
        // Scroll: consume upfront portion (half, rounded down).
        let upfront = spell.mana / 2;
        if upfront > 0 {
            let _ = engine.consume_mana(caster_serial, upfront).await;
        }
    }

    // 3. Spell words + cast animation (mount-aware) + cast sound
    send_spell_words(worker_tx, world, caster_serial, spell).await;

    // TODO: skill-based fizzle check — at 100 Magery there is no fizzle.
    // Future: if magery < required { fizzle; return BeginCastResult { ok: false } }

    BeginCastResult { packets, ok: true }
}

/// Result of [`complete_cast`].
pub struct CompleteCastResult {
    /// Packets to send to the caster's session.
    pub packets: Vec<RawPacket>,
    /// Optional stat buff/debuff to apply. The caller must feed this into
    /// the session's `BuffState` and `apply_buff_stat` engine call.
    pub stat_effect: Option<PendingStatEffect>,
}

/// A stat effect that should be applied after a successful cast.
///
/// Returned by `complete_cast`; the session handler is responsible for
/// registering it in `BuffState` and calling `buffs::apply_buff_stat`.
pub struct PendingStatEffect {
    pub target_serial: u32,
    pub buff_kind: BuffKind,
    pub delta: i16,
    pub duration_ms: u64,
}

/// **Phase 2**: re-check LOS, send visual effects, apply damage / healing.
///
/// Called when the cast-delay timer fires (or immediately for instant casts).
///
/// When `scroll_item_serial` is `Some(serial)`, one scroll is consumed from
/// the item stack on successful completion (not on fizzle).
pub async fn complete_cast(
    spell: &SpellDef,
    caster_serial: u32,
    target_serial: u32,
    worker_tx: &DemoWorkerTx,
    world: u8,
    scroll_item_serial: Option<u32>,
) -> CompleteCastResult {
    let mut packets = Vec::new();
    let mut stat_effect_result: Option<PendingStatEffect> = None;
    let engine = crate::game_util::engine_for(worker_tx, world);

    let bail = || CompleteCastResult { packets: Vec::new(), stat_effect: None };

    // 1. Get caster position (may have moved during cast).
    let caster = engine.get_entity(caster_serial).await;
    let Some(m) = caster.as_ref().and_then(|e| e.mobile()) else {
        return bail();
    };
    let (cx, cy, cz) = (m.x, m.y, m.z);

    // 2. Get target info.
    let target = engine.get_entity(target_serial).await;
    let (tx, ty, tz, target_graphic, target_name, target_is_player) = match target.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y, m.z, m.graphic, m.name.clone(), m.is_player),
        None => match &target {
            Some(DemoEntity::Item { x, y, z, .. }) => (*x, *y, *z, 0u16, String::new(), false),
            _ => {
                spell_fizzle("Invalid target.", caster_serial, worker_tx, world, &mut packets).await;
                return CompleteCastResult { packets, stat_effect: None };
            }
        }
    };

    // 3. Line-of-sight re-check — target may have moved during cast delay.
    if !engine.check_los(
        cx, cy, cz as i16 + crate::constants::EYE_HEIGHT,
        tx, ty, tz as i16 + crate::constants::EYE_HEIGHT,
    ).await {
        spell_fizzle("The spell fizzles.", caster_serial, worker_tx, world, &mut packets).await;
        return CompleteCastResult { packets, stat_effect: None };
    }

    // 3b. Consume mana on successful completion.
    //     - Regs cast:   full mana cost now.
    //     - Scroll cast: remaining half (total - upfront).
    {
        let mana_now = if scroll_item_serial.is_some() {
            spell.mana - spell.mana / 2  // remaining half
        } else {
            spell.mana                    // full cost for regs
        };
        if mana_now > 0 {
            if engine.consume_mana(caster_serial, mana_now).await.is_none() {
                // Not enough mana for completion — fizzle.
                spell_fizzle("Insufficient mana.", caster_serial, worker_tx, world, &mut packets).await;
                return CompleteCastResult { packets, stat_effect: None };
            }
        }
    }

    // 3c. Consume reagents on successful completion (regs cast only).
    if scroll_item_serial.is_none() && !spell.reagents.is_empty() {
        if let Some(reagent_serials) = find_reagent_items(
            caster_serial, spell.reagents, worker_tx, world,
        ).await {
            for &rs in &reagent_serials {
                let _ = engine.consume_item(rs, 1, None).await;
            }
        } else {
            // Reagents disappeared during cast — fizzle.
            spell_fizzle("Insufficient reagents.", caster_serial, worker_tx, world, &mut packets).await;
            return CompleteCastResult { packets, stat_effect: None };
        }
    }

    // 4. Visual effects.
    const PROJECTILE_Z_OFFSET: i8 = 15;

    if spell.projectile_graphic != 0 {
        game_util::send_effect(worker_tx, world,
            0, // direction_type 0 = moving effect (from source to target)
            caster_serial, target_serial,
            spell.projectile_graphic,
            cx, cy, cz.saturating_add(PROJECTILE_Z_OFFSET),
            tx, ty, tz.saturating_add(PROJECTILE_Z_OFFSET),
            10, 30,
            false, false,
        ).await;
    }

    // Impact sound at target position.
    if spell.impact_sound != 0 {
        game_util::send_sound(worker_tx, world, spell.impact_sound, tx, ty, tz as i16).await;
    }

    // Lightning bolt effect (direction_type=1).
    if spell.lightning_bolt {
        game_util::send_effect(worker_tx, world,
            1, // lightning bolt
            target_serial, 0,
            0,
            tx, ty, tz,
            0, 0, 0,
            0, 0,
            false, false,
        ).await;
    }

    // Target effect (sparkle, flamestrike, etc.).
    if spell.target_effect != 0 {
        game_util::send_effect(worker_tx, world,
            3, // direction_type 3 = stationary effect at target
            target_serial, 0,
            spell.target_effect,
            tx, ty, tz,
            0, 0, 0,
            spell.target_effect_speed, spell.target_effect_duration,
            false, false,
        ).await;
    }

    // 5. Apply damage or healing.
    if spell.damage_max > 0 {
        let damage = random_range(spell.damage_min, spell.damage_max);
        debug!("[magic] {} deals {} damage to 0x{:08X}", spell.name, damage, target_serial);

        // Flag aggression for player-vs-player offensive spells.
        // Skip when the caster targets themselves — no self-aggression.
        if target_is_player && target_serial != caster_serial {
            engine.flag_aggression(caster_serial, target_serial).await;
        }
        if let Some(result) = engine.deal_damage(target_serial, damage, caster_serial).await {
            if result.killed {
                if target_is_player && target_serial != caster_serial {
                    engine.record_kill(caster_serial, target_serial).await;
                }
                info!("[magic] {} killed 0x{:08X}", spell.name, target_serial);
                // Inject loot-table items into the auto-created corpse and schedule decay.
                // Player corpses are left intact so the player can recover items.
                if let Some(ref kill) = result.kill {
                    if !target_is_player {
                        let loot = crate::loot::generate_loot_for_body(target_graphic);
                        if !loot.is_empty() {
                            engine.add_container_items(kill.corpse_serial, loot).await;
                        }
                        crate::game_util::schedule_corpse_decay(worker_tx, world, kill.corpse_serial);
                    } else if let Some(ref mount) = kill.dropped_mount {
                        // Player died mounted; engine couldn't restore the saved
                        // mount NPC — spawn a default one from the graphic.
                        crate::game_util::spawn_mount_npc_on_death(
                            worker_tx, world, mount, kill.x, kill.y, kill.z,
                        ).await;
                    }
                }
            }
        }
    }

    if spell.heal_max > 0 {
        let heal_amount = random_range(spell.heal_min, spell.heal_max);
        debug!("[magic] {} heals 0x{:08X} for {}", spell.name, target_serial, heal_amount);

        let _ = engine.heal(target_serial, heal_amount).await;

        // Heal feedback: overhead "+N" as UnicodeSpeech (0xAE) to the caster.
        packets.push(heal_feedback_packet(
            target_serial, target_graphic, &target_name, heal_amount,
        ));
    }

    // 6. Consume scroll on successful cast.
    if let Some(scroll_serial) = scroll_item_serial {
        let graphic_check = if spell.scroll_graphic != 0 {
            Some(spell.scroll_graphic)
        } else {
            None
        };
        let consumed = engine.consume_item(scroll_serial, 1, graphic_check).await;
        if consumed.is_none() {
            debug!("[magic] scroll 0x{:08X} could not be consumed (already gone?)", scroll_serial);
        }
    }

    // 7. Stat buff / debuff (Bless, Curse, etc.).
    if let Some(ref eff) = spell.stat_effect {
        stat_effect_result = Some(PendingStatEffect {
            target_serial,
            buff_kind: eff.buff_kind,
            delta: eff.delta,
            duration_ms: eff.duration_ms,
        });
    }

    info!(
        "[magic] 0x{:08X} cast {} on 0x{:08X}",
        caster_serial, spell.name, target_serial
    );

    CompleteCastResult { packets, stat_effect: stat_effect_result }
}

/// Consume a spell's mana and reagents on successful completion.
///
/// Used by spells whose effect is resolved **outside** [`complete_cast`]
/// (e.g. Mark / Recall, which act on a rune in the backpack rather than a
/// world entity).  Returns `false` if mana or reagents were unavailable —
/// in that case nothing is consumed and the caller should fizzle.
pub async fn consume_spell_cost(
    spell: &SpellDef,
    caster_serial: u32,
    worker_tx: &DemoWorkerTx,
    world: u8,
) -> bool {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Verify reagents are still present before consuming anything.
    let reagent_serials = if spell.reagents.is_empty() {
        Some(Vec::new())
    } else {
        find_reagent_items(caster_serial, spell.reagents, worker_tx, world).await
    };
    let Some(reagent_serials) = reagent_serials else {
        return false;
    };

    // Consume mana.
    if spell.mana > 0 && engine.consume_mana(caster_serial, spell.mana).await.is_none() {
        return false;
    }

    // Consume reagents.
    for rs in reagent_serials {
        let _ = engine.consume_item(rs, 1, None).await;
    }

    true
}

/// **Wall of Stone resolution** — spawn a row of stone blocks at the chosen
/// ground tile and schedule them to decay one-by-one.
///
/// Called from the cast-timer branch (see `rust_handler`) when an
/// [`ActionPayload::WallOfStone`](crate::actions::ActionPayload::WallOfStone)
/// completes.  Consumes the spell's mana + reagents up front (fizzles on
/// failure), then spawns [`wall_of_stone::BLOCK_COUNT`] blocks centered on the
/// target tile.  The row orientation (north-south vs east-west) is chosen so
/// the wall faces the caster.
///
/// [`wall_of_stone::BLOCK_COUNT`]: crate::constants::wall_of_stone::BLOCK_COUNT
pub async fn complete_wall_of_stone(
    caster_serial: u32,
    target_x: u16,
    target_y: u16,
    target_z: i8,
    world: u8,
    serial_alloc: &std::sync::Arc<common::uo_engine::serial_alloc::SerialAllocator>,
    session: &mut network::session::Session,
    worker_tx: &DemoWorkerTx,
) -> network::error::Result<()> {
    use crate::constants::{item, sound, wall_of_stone};

    let Some(spell) = get_spell(spell_id::WALL_OF_STONE) else {
        return Ok(());
    };
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Resolve the caster position (used to orient the wall) and check the
    // caster still exists.
    let Some(caster) = engine.get_entity(caster_serial).await else {
        return Ok(());
    };
    let Some(m) = caster.mobile() else {
        return Ok(());
    };
    let (cx, cy, cz) = (m.x, m.y, m.z);

    // Line-of-sight re-check — the caster may have moved or turned away during
    // the cast delay.  Fizzle (without consuming reagents) if the tile can no
    // longer be seen.  Source is the caster's eyes; the target is the ground
    // tile itself (no eye-height offset).
    const EYE_H: i16 = crate::constants::EYE_HEIGHT;
    if !engine.check_los(
        cx, cy, cz as i16 + EYE_H,
        target_x, target_y, target_z as i16,
    ).await {
        let mut packets = Vec::new();
        spell_fizzle("The spell fizzles.", caster_serial, worker_tx, world, &mut packets).await;
        for pkt in packets {
            session.send(pkt).await?;
        }
        return Ok(());
    }

    // Consume mana + reagents now; fizzle if anything is missing.
    if !consume_spell_cost(spell, caster_serial, worker_tx, world).await {
        let mut packets = Vec::new();
        spell_fizzle("The spell fizzles.", caster_serial, worker_tx, world, &mut packets).await;
        for pkt in packets {
            session.send(pkt).await?;
        }
        return Ok(());
    }

    // Orient the wall perpendicular to the caster→target axis: if the target
    // is mostly north/south of the caster the wall runs east-west (varies X);
    // otherwise it runs north-south (varies Y).
    let dx = (target_x as i32 - cx as i32).abs();
    let dy = (target_y as i32 - cy as i32).abs();
    let horizontal = dy >= dx; // wall extends along X

    let count = wall_of_stone::BLOCK_COUNT as i32;
    // Center the row on the target tile: offsets -half .. -half + count.
    let half = count / 2;

    let mut block_serials: Vec<u32> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let offset = i - half;
        let (bx, by) = if horizontal {
            ((target_x as i32 + offset) as u16, target_y)
        } else {
            (target_x, (target_y as i32 + offset) as u16)
        };

        let Some(serial) = serial_alloc.alloc_item() else {
            debug!("[magic] Wall of Stone: serial space exhausted at block {}", i);
            break;
        };

        let block = DemoEntity::Item {
            serial,
            graphic: item::WALL_OF_STONE_BLOCK,
            color: 0,
            amount: 1,
            x: bx,
            y: by,
            z: target_z,
            is_container: false,
            hidden: false,
            facing: None,
        };
        engine.spawn_entity(serial, block).await;
        crate::game_util::send_sound(worker_tx, world, sound::WALL_OF_STONE, bx, by, target_z as i16).await;
        block_serials.push(serial);
    }

    // Schedule the blocks to decay one-by-one (random order, staggered).
    crate::game_util::schedule_staggered_decay(
        worker_tx,
        world,
        block_serials,
        wall_of_stone::FIRST_DECAY_SECS,
        wall_of_stone::DECAY_INTERVAL_SECS,
    );

    info!(
        "[magic] 0x{:08X} cast Wall of Stone at ({},{}) ({} orientation)",
        caster_serial, target_x, target_y,
        if horizontal { "E-W" } else { "N-S" },
    );

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Build a UnicodeSpeech (0xAE) packet showing "+N" overhead for heal feedback.
fn heal_feedback_packet(
    target_serial: u32,
    target_graphic: u16,
    target_name: &str,
    heal_amount: u16,
) -> RawPacket {
    use packets::speech::{UnicodeSpeech, SpeechType};
    use packets::u_io::{FixedString, NullUnicodeString};

    let msg = format!("+{}", heal_amount);
    let pkt = UnicodeSpeech {
        id: UnicodeSpeech::ID,
        len: 0, // filled by encode
        serial: target_serial,
        model: target_graphic,
        speech_type: SpeechType::Normal,
        color: hue::HEAL_FEEDBACK,
        font: 9,
        language: FixedString("ENU".to_string()),
        name: FixedString(target_name.to_string()),
        message: NullUnicodeString(msg),
    };
    RawPacket::s2c(encode_packet(&pkt))
}

/// Find item serials for each required reagent in the caster's backpack.
///
/// Returns `Some(vec_of_serials)` if all reagents are present (one serial
/// per entry in `reagents`), or `None` if any reagent is missing.
///
/// Duplicate graphic entries (e.g. two different spells needing the same
/// reagent) are handled correctly — each entry consumes one stack slot,
/// but since UO reagents stack, a single stack with amount >= N suffices
/// for N copies of the same graphic.
async fn find_reagent_items(
    caster_serial: u32,
    reagents: &[u16],
    worker_tx: &DemoWorkerTx,
    world: u8,
) -> Option<Vec<u32>> {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // 1. Get caster entity to find backpack serial.
    let entity = engine.get_entity(caster_serial).await?;
    let bp_serial = entity.mobile().and_then(|m| {
        m.items.iter()
            .find(|eq| eq.layer == Layer::Backpack)
            .map(|eq| eq.serial)
    })?;

    // 2. Get backpack contents.
    let container = engine.get_container(bp_serial).await?;

    // Build a mutable list of (graphic, serial, remaining_amount) from
    // the container so we can track consumption of multiple units from
    // the same stack.
    let mut available: Vec<(u16, u32, u16)> = container.items.iter()
        .map(|i| (i.graphic, i.serial, i.amount.max(1)))
        .collect();

    let mut result = Vec::with_capacity(reagents.len());

    for &reagent_graphic in reagents {
        // Find a stack with matching graphic and remaining amount > 0.
        let found = available.iter_mut()
            .find(|(g, _, amt)| *g == reagent_graphic && *amt > 0);
        match found {
            Some(entry) => {
                result.push(entry.1); // serial
                entry.2 -= 1;         // reserve one unit
            }
            None => return None, // reagent not found
        }
    }

    Some(result)
}

/// Send a spell fizzle: broadcast sound + effect at the given position,
/// and push a system speech message into `packets`.
async fn spell_fizzle(
    msg: &str,
    caster_serial: u32,
    worker_tx: &DemoWorkerTx,
    world: u8,
    packets: &mut Vec<RawPacket>,
) {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Resolve caster position for effect placement.
    let entity = engine.get_entity(caster_serial).await;
    let Some(m) = entity.as_ref().and_then(|e| e.mobile()) else {
        return; // no entity, nothing to show
    };
    let (x, y, z) = (m.x, m.y, m.z);

    packets.push(game_util::system_speech(msg));
    game_util::send_fizzle(worker_tx, world, caster_serial, x, y, z).await;
}

async fn send_spell_words(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serial: u32,
    spell: &SpellDef,
) {
    let engine = crate::game_util::engine_for(worker_tx, world);

    // Get caster entity for position and mount state.
    let entity = engine.get_entity(serial).await;

    if let Some(m) = entity.as_ref().and_then(|e| e.mobile()) {
        // Send spell words overhead (speech_type = Normal, color = SPELL_WORDS).
        if let Some(words) = spell.words {
            let _ = worker_tx.send(WorkerCommand::MapCommand(
                world,
                DemoCommand::BroadcastSpeech {
                    serial,
                    graphic: m.graphic,
                    speech_type: 0x00, // Normal
                    color: hue::SPELL_WORDS,
                    font: 3,
                    name: m.name.clone(),
                    message: words.to_string(),
                    x: m.x,
                    y: m.y,
                },
            )).await;
        }

        // Cast animation — use mounted variant if riding. Skip if 0 or no
        // mounted variant.
        if spell.cast_action != 0 {
            let is_mounted = m.items.iter().any(|eq| eq.layer == Layer::Mount);
            if let Some(action) = spell.resolved_cast_action(is_mounted) {
                game_util::send_animation(worker_tx, world, serial, action, 5, 1, false, false, 1, m.x, m.y).await;
            }
        }
    }
}
