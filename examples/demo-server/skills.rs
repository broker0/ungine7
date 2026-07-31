//! Skill definitions and the default starting skill set for players.
//!
//! Skills in this demo are **static** — there is no gain/training.  Each
//! player is seeded with [`default_player_skills`] at character creation and
//! the values are sent to the client via packet `0x3A` (`SendSkills`).
//!
//! Values and caps are in **tenths** (e.g. `500` = 50.0, `1000` = 100.0),
//! matching the UO wire format.
//!
//! ## Skill names
//!
//! Skill *names* (e.g. "Swordsmanship") live in the client's `skills.mul`;
//! the server never sends them — only numeric skill ids.  The [`name`] table
//! below is purely for server-side logging / debugging and does not need to
//! match the client exactly.

use std::collections::BTreeMap;

use common::uo_engine::entity::{SkillLock, SkillValue};
use packets::skills::{
    SendSkills, SkillEntryWithCap, SkillLock as WireSkillLock,
};

use crate::constants::skill_id;

/// Default per-skill cap, in tenths (100.0).
pub const DEFAULT_SKILL_CAP: u16 = 1000;

/// A skill id + its human-readable name, for server-side logging.
#[allow(dead_code)]
struct SkillDef {
    id: u16,
    name: &'static str,
}

/// Known skills (id → name) for logging/debugging only.
///
/// Not exhaustive — only the skills this demo uses.  The authoritative name
/// list lives in the client's `skills.mul`.
#[allow(dead_code)]
static SKILLS: &[SkillDef] = &[
    SkillDef { id: skill_id::ANATOMY,       name: "Anatomy" },
    SkillDef { id: skill_id::ARMS_LORE,     name: "Arms Lore" },
    SkillDef { id: skill_id::BLACKSMITHING, name: "Blacksmithing" },
    SkillDef { id: skill_id::EVAL_INT,      name: "Evaluating Intelligence" },
    SkillDef { id: skill_id::HEALING,       name: "Healing" },
    SkillDef { id: skill_id::MAGERY,        name: "Magery" },
    SkillDef { id: skill_id::RESIST_SPELLS, name: "Resisting Spells" },
    SkillDef { id: skill_id::POISONING,     name: "Poisoning" },
    SkillDef { id: skill_id::ANIMAL_TAMING, name: "Animal Taming" },
    SkillDef { id: skill_id::SWORDS,        name: "Swordsmanship" },
    SkillDef { id: skill_id::WRESTLING,     name: "Wrestling" },
    SkillDef { id: skill_id::MEDITATION,    name: "Meditation" },
];

/// Look up a skill's name by id, for logging.  Returns `None` if unknown.
#[allow(dead_code)]
pub fn name(skill_id: u16) -> Option<&'static str> {
    SKILLS.iter().find(|s| s.id == skill_id).map(|s| s.name)
}

/// The starting skill set for a freshly created player.
///
/// Mirrors the values previously hardcoded in the login skill-send block.
/// All skills use [`DEFAULT_SKILL_CAP`] and start with `Up` lock.
pub fn default_player_skills() -> BTreeMap<u16, SkillValue> {
    // (skill_id, value_in_tenths)
    let entries: &[(u16, u16)] = &[
        (skill_id::ANATOMY,        500),  // 50.0
        (skill_id::EVAL_INT,       800),  // 80.0
        (skill_id::MAGERY,        1000),  // 100.0
        (skill_id::RESIST_SPELLS,  600),  // 60.0
        (skill_id::POISONING,     1000),  // 100.0
        (skill_id::ANIMAL_TAMING, 1000),  // 100.0
        (skill_id::SWORDS,         500),  // 50.0
        (skill_id::WRESTLING,      500),  // 50.0
        (skill_id::MEDITATION,     800),  // 80.0
    ];

    entries
        .iter()
        .map(|&(id, value)| (id, SkillValue::with_cap(value, DEFAULT_SKILL_CAP)))
        .collect()
}

// ── Wire conversion ────────────────────────────────────────────────────────

/// Convert the entity-layer [`SkillLock`] into the wire enum for packet 0x3A.
pub fn lock_to_wire(lock: SkillLock) -> WireSkillLock {
    match lock {
        SkillLock::Up => WireSkillLock::Up,
        SkillLock::Down => WireSkillLock::Down,
        SkillLock::Locked => WireSkillLock::Locked,
    }
}

/// Convert the wire enum from packet 0x3A into the entity-layer [`SkillLock`].
///
/// Unknown wire values fall back to [`SkillLock::Up`].
pub fn lock_from_wire(lock: WireSkillLock) -> SkillLock {
    match lock {
        WireSkillLock::Up => SkillLock::Up,
        WireSkillLock::Down => SkillLock::Down,
        WireSkillLock::Locked => SkillLock::Locked,
        WireSkillLock::Unknown(_) => SkillLock::Up,
    }
}

/// Build a single [`SkillEntryWithCap`] from a skill id + value.
///
/// `unmodified_value` is the base value (no temporary modifiers in this
/// demo, so it equals `value`).
pub fn entry_with_cap(skill_id: u16, sv: &SkillValue) -> SkillEntryWithCap {
    SkillEntryWithCap {
        skill_id,
        value: sv.value,
        unmodified_value: sv.value,
        lock: lock_to_wire(sv.lock),
        cap: sv.cap,
    }
}

/// Build a [`SkillEntryWithCap`] applying an equipment skill bonus (tenths).
///
/// - `value` = base value + bonus (the *effective* skill the client shows).
/// - `unmodified_value` = base value (what the skill would be unequipped).
///
/// Bonuses may push the effective value above `cap` (per design — "plus"
/// weapons can grant >100.0), so no clamping is applied.
pub fn entry_with_bonus(skill_id: u16, base: &SkillValue, bonus: u16) -> SkillEntryWithCap {
    SkillEntryWithCap {
        skill_id,
        value: base.value.saturating_add(bonus),
        unmodified_value: base.value,
        lock: lock_to_wire(base.lock),
        cap: base.cap,
    }
}

/// Build the full skill-list packet (0x3A, type 0x02 — with cap) from a
/// mobile's skill map, applying any equipment skill bonuses.
///
/// `bonuses` maps skill id → bonus in tenths (see
/// [`crate::equipment_calc::compute_skill_bonuses`]).  Skills without a
/// bonus are sent at their base value.
pub fn build_full_list_with_bonuses(
    skills: &BTreeMap<u16, SkillValue>,
    bonuses: &BTreeMap<u16, u16>,
) -> SendSkills {
    let entries = skills
        .iter()
        .map(|(&id, sv)| {
            let bonus = bonuses.get(&id).copied().unwrap_or(0);
            entry_with_bonus(id, sv, bonus)
        })
        .collect();
    SendSkills::FullListWithCap { skills: entries }
}

/// Build a single-skill update packet (0x3A, type 0xDF — with cap).
pub fn build_single_update(skill_id: u16, sv: &SkillValue) -> SendSkills {
    SendSkills::SingleUpdateWithCap(entry_with_cap(skill_id, sv))
}


