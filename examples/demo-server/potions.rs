//! Potion definitions and lookup.
//!
//! Each potion is identified by its `(graphic, color)` pair.  The static
//! [`POTIONS`] table maps these to a [`PotionDef`] describing the effect,
//! cooldown, sound, and display name.
//!
//! The actual "use potion" logic (consume, apply effect, play sound/anim,
//! set cooldown) lives in `game_session::potions`.

use crate::constants::{item, potion as potion_cfg};

// ── Potion effect ────────────────────────────────────────────────────────

/// What happens when the potion is consumed.
#[derive(Debug, Clone, Copy)]
pub enum PotionEffect {
    /// Instantly restore hit points (min..=max).
    Heal { min: u16, max: u16 },
    /// Instantly restore stamina (min..=max).
    Refresh { min: u16, max: u16 },
    /// Instantly restore mana (min..=max).
    RestoreMana { min: u16, max: u16 },
    /// Cure poison (chance of success scales inversely with poison level).
    Cure,
    /// Temporary strength buff.
    Strength { bonus: i16, duration_ms: u64 },
    /// Temporary agility (dexterity) buff.
    Agility { bonus: i16, duration_ms: u64 },
    /// Poison potion — used on a fencing weapon (via target cursor) to apply
    /// poison charges, not drunk.  The `level` is `1..=4` (Lesser..Deadly).
    Poison { level: u8 },
    /// Shrink potion — used on a tamed animal (via target cursor), not drunk.
    /// Turns one of the player's own pets into a carryable statue item.
    Shrink,
}

// ── PotionDef ────────────────────────────────────────────────────────────

/// Static definition for one potion variant.
#[derive(Debug, Clone, Copy)]
pub struct PotionDef {
    /// Unique potion id (for logging / serialization).
    pub id: u8,
    /// Item graphic ID.
    pub graphic: u16,
    /// Item hue/color that distinguishes this variant (0 = default).
    pub color: u16,
    /// Human-readable name shown in messages.
    pub name: &'static str,
    /// What the potion does.
    pub effect: PotionEffect,
    /// Sound played when drinking.
    pub sound: u16,
}

// ── Lookup ───────────────────────────────────────────────────────────────

/// Find a potion definition by `(graphic, color)`.
///
/// Returns `None` if the item is not a known potion.
///
/// **Poison potions** are matched by graphic alone (any hue): all four
/// poison levels share the single [`item::POTION_POISON`] graphic, and the
/// concrete level is stored per-instance in `ItemProps.meta` (see
/// [`crate::game_session::poison::META_POISON_LEVEL`]) rather than encoded in
/// the bottle's hue.  The returned [`PotionDef`] therefore carries only the
/// *default* poison level; callers that need the real level must read it from
/// the item's meta.
pub fn lookup_potion(graphic: u16, color: u16) -> Option<&'static PotionDef> {
    // Poison: match by graphic only — the hue is cosmetic and the level lives
    // in per-instance meta.
    if graphic == item::POTION_POISON {
        return POTIONS.iter().find(|p| {
            p.graphic == item::POTION_POISON
                && matches!(p.effect, PotionEffect::Poison { .. })
        });
    }
    POTIONS.iter().find(|p| p.graphic == graphic && p.color == color)
}

/// Default hue applied to a poison bottle of the given level when it is
/// created.  Purely cosmetic — the level is carried in meta, not the hue.
///
/// Returns [`POISON_DEFAULT_HUE`] for every level today, but the per-level
/// table makes it easy to tint individual levels later without affecting any
/// game logic.
pub fn poison_level_hue(level: u8) -> u16 {
    match level {
        // Add per-level overrides here, e.g. `4 => 0x0021,` for Deadly.
        _ => POISON_DEFAULT_HUE,
    }
}

/// Default cosmetic hue for poison bottles (`0` = no tint / default art).
pub const POISON_DEFAULT_HUE: u16 = 0;

/// Display name for a poison potion of the given level (`1..=4`).
pub fn poison_name(level: u8) -> &'static str {
    match level {
        1 => "Lesser Poison Potion",
        2 => "Poison Potion",
        3 => "Greater Poison Potion",
        4 => "Deadly Poison Potion",
        _ => "Poison Potion",
    }
}

/// Whether a graphic is the poison-bottle graphic.
pub fn is_poison_graphic(graphic: u16) -> bool {
    graphic == item::POTION_POISON
}

/// Check whether a graphic could be a potion (any color).
///
/// Used for fast early-out in the double-click chain before fetching
/// the full item info from the engine.
pub fn is_potion_graphic(graphic: u16) -> bool {
    POTIONS.iter().any(|p| p.graphic == graphic)
}

// ── Potion table ─────────────────────────────────────────────────────────
//
// Graphics and colors based on classic UO (T2A era).  Each potion type
// uses a distinct `(graphic, color)` combination so that different potion
// kinds can share the same bottle shape with different hues.

static POTIONS: &[PotionDef] = &[
    // ── Heal ─────────────────────────────────────────────────────────
    PotionDef {
        id: 1,
        graphic: item::POTION_GREATER_HEAL,
        color: 0,
        name: "Greater Heal Potion",
        effect: PotionEffect::Heal { min: 20, max: 75 },
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Refresh (stamina) ────────────────────────────────────────────
    PotionDef {
        id: 2,
        graphic: item::POTION_REFRESH,
        color: 0x002D, // orange hue to distinguish from heal
        name: "Greater Refresh Potion",
        effect: PotionEffect::Refresh { min: 20, max: 75 },
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Mana ─────────────────────────────────────────────────────────
    PotionDef {
        id: 3,
        graphic: item::POTION_MANA,
        color: 0x0005, // blue hue
        name: "Greater Mana Potion",
        effect: PotionEffect::RestoreMana { min: 20, max: 75 },
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Cure ─────────────────────────────────────────────────────────
    PotionDef {
        id: 4,
        graphic: item::POTION_CURE,
        color: 0,
        name: "Greater Cure Potion",
        effect: PotionEffect::Cure,
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Strength ─────────────────────────────────────────────────────
    PotionDef {
        id: 5,
        graphic: item::POTION_STRENGTH,
        color: 0x0035, // white/golden hue
        name: "Greater Strength Potion",
        effect: PotionEffect::Strength {
            bonus: potion_cfg::STRENGTH_BONUS,
            duration_ms: potion_cfg::STRENGTH_DURATION_MS,
        },
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Agility ──────────────────────────────────────────────────────
    PotionDef {
        id: 6,
        graphic: item::POTION_AGILITY,
        color: 0,
        name: "Greater Agility Potion",
        effect: PotionEffect::Agility {
            bonus: potion_cfg::AGILITY_BONUS,
            duration_ms: potion_cfg::AGILITY_DURATION_MS,
        },
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Poison (used on fencing weapons, can also be drunk) ──────────
    // All poison levels (1..=4) share the single poison-bottle graphic with
    // a cosmetic hue.  The level is NOT encoded here — it lives per-instance
    // in `ItemProps.meta` (META_POISON_LEVEL).  This single entry is matched
    // by graphic alone (see `lookup_potion`); the `level` below is only a
    // fallback default for bottles that have no meta level set.
    PotionDef {
        id: 7,
        graphic: item::POTION_POISON,
        color: POISON_DEFAULT_HUE,
        name: "Poison Potion",
        effect: PotionEffect::Poison { level: 1 },
        sound: potion_cfg::DRINK_SOUND,
    },

    // ── Shrink (used on tamed animals, not drunk) ────────────────────
    // Shares the agility bottle graphic (0x0F06), distinguished by hue.
    PotionDef {
        id: 11,
        graphic: item::POTION_SHRINK,
        color: item::SHRINK_POTION_HUE,
        name: "Shrink Potion",
        effect: PotionEffect::Shrink,
        sound: potion_cfg::DRINK_SOUND,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poison_matches_any_hue_by_graphic() {
        // All four legacy poison hues — and the new default — must resolve to
        // the single poison definition.
        for hue in [0u16, 0x0042, 0x0044, 0x0021, 0x1234] {
            let def = lookup_potion(item::POTION_POISON, hue)
                .expect("poison must resolve for any hue");
            assert!(matches!(def.effect, PotionEffect::Poison { .. }));
        }
    }

    #[test]
    fn non_poison_still_matches_by_pair() {
        // Mana and Strength share graphic 0x0F09 but differ by hue.
        let mana = lookup_potion(item::POTION_MANA, 0x0005).unwrap();
        assert!(matches!(mana.effect, PotionEffect::RestoreMana { .. }));
        let strength = lookup_potion(item::POTION_STRENGTH, 0x0035).unwrap();
        assert!(matches!(strength.effect, PotionEffect::Strength { .. }));
        // A non-matching hue for a paired graphic resolves to neither.
        assert!(lookup_potion(item::POTION_MANA, 0xFFFF).is_none());
    }

    #[test]
    fn poison_names_by_level() {
        assert_eq!(poison_name(1), "Lesser Poison Potion");
        assert_eq!(poison_name(2), "Poison Potion");
        assert_eq!(poison_name(3), "Greater Poison Potion");
        assert_eq!(poison_name(4), "Deadly Poison Potion");
    }

    #[test]
    fn poison_default_hue_is_uncolored() {
        assert_eq!(POISON_DEFAULT_HUE, 0);
        for level in 1u8..=4 {
            assert_eq!(poison_level_hue(level), POISON_DEFAULT_HUE);
        }
    }

    #[test]
    fn is_poison_graphic_detects_bottle() {
        assert!(is_poison_graphic(item::POTION_POISON));
        assert!(!is_poison_graphic(item::POTION_MANA));
    }
}
