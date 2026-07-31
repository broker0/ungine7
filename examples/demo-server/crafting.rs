//! Crafting — smelting and blacksmithing definition tables.
//!
//! Data-driven crafting system (mirrors `gathering` / `potions` / `vendor`
//! tables).  Two related mechanics:
//!
//! 1. **Smelting** — the player double-clicks **iron ore** while standing
//!    next to a **forge** world-object.  The whole ore stack is melted into
//!    iron ingots (see [`SMELT_TABLE`]).
//! 2. **Blacksmithing** — the player double-clicks a **smith's hammer** while
//!    standing next to an **anvil** world-object.  A gump menu opens
//!    ([`CraftCategory`] tabs → recipes); selecting a recipe consumes its
//!    ingredients (iron ingots) and, on a successful roll, produces a weapon
//!    or armor piece.
//!
//! Success is a fixed per-recipe chance ([`RecipeDef::chance`]) — there is no
//! skill-value dependency in this demo.  Result graphics are chosen from the
//! existing [`crate::constants::weapon`] / [`crate::constants::armor`] tables
//! so that crafted items are fully functional in combat.
//!
//! ## Extending
//!
//! Add a [`RecipeDef`] to [`RECIPES`].  Armor pieces should carry a
//! `armor_rating` matching an entry in `ARMOR_TEMPLATES`; weapons should use a
//! graphic known to `lookup_weapon` so the combat system resolves them.

#![allow(dead_code)]

use packets::layer::Layer;

use crate::constants::{anim, craft, item};
use crate::game_util;

// ── Smelting ─────────────────────────────────────────────────────────────

/// One smelting recipe: `(ore_graphic, ingot_graphic, ingots_per_ore)`.
///
/// The entire targeted ore stack is consumed; each ore unit yields
/// `ingots_per_ore` ingots.
pub static SMELT_TABLE: &[(u16, u16, u16)] = &[
    (item::IRON_ORE, item::IRON_INGOT, 1),
];

/// Look up a smelt recipe by the ore item graphic.
///
/// Returns `Some((ingot_graphic, ingots_per_ore))` if the graphic is a
/// smeltable ore.
pub fn smelt_result(ore_graphic: u16) -> Option<(u16, u16)> {
    SMELT_TABLE
        .iter()
        .find(|(ore, _, _)| *ore == ore_graphic)
        .map(|(_, ingot, per)| (*ingot, *per))
}

// ── Blacksmithing ──────────────────────────────────────────────────────────

/// Category tab in the crafting gump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CraftCategory {
    Weapons,
    Armor,
}

impl CraftCategory {
    /// All categories, in display order.
    pub fn all() -> &'static [CraftCategory] {
        &[CraftCategory::Weapons, CraftCategory::Armor]
    }

    /// Tab title shown in the gump.
    pub fn title(self) -> &'static str {
        match self {
            CraftCategory::Weapons => "Weapons",
            CraftCategory::Armor => "Armor",
        }
    }
}

/// One crafting ingredient requirement.
#[derive(Debug, Clone, Copy)]
pub struct Ingredient {
    /// Item graphic required (e.g. iron ingot).
    pub graphic: u16,
    /// Number of units required.
    pub amount: u16,
}

/// A blacksmithing recipe.
#[derive(Debug, Clone, Copy)]
pub struct RecipeDef {
    /// Stable identifier (used as the gump button payload mapping).
    pub key: &'static str,
    /// Display name shown in the gump and on the produced item.
    pub name: &'static str,
    /// Which gump tab this recipe lives under.
    pub category: CraftCategory,
    /// Result item graphic.
    pub result_graphic: u16,
    /// Result item hue (`0` = default).
    pub result_color: u16,
    /// Required materials.
    pub ingredients: &'static [Ingredient],
    /// Fixed success chance, percent `0..=100`.
    pub chance: u8,
    /// Armor rating to stamp into `ItemProps.meta["armor_rating"]`.
    ///
    /// `0` for weapons (their stats come from the weapon table by graphic).
    pub armor_rating: u16,
    /// Equipment layer hint (cosmetic / future auto-equip; unused for now).
    pub layer: Option<Layer>,
    /// Animation action played while smithing.
    pub anim: u16,
    /// Sound played while smithing.
    pub sound: u16,
}

impl RecipeDef {
    /// Roll the fixed success chance.
    pub fn roll_success(&self) -> bool {
        let roll = game_util::random_range(1, 100) as u8;
        roll <= self.chance
    }
}

/// Convenience: an iron-ingot ingredient of the given amount.
const fn ingots(amount: u16) -> Ingredient {
    Ingredient { graphic: item::IRON_INGOT, amount }
}

// ── Recipe table ───────────────────────────────────────────────────────────
//
// Result graphics are taken from `constants::weapon::WEAPONS` (weapons) and
// `constants::armor::ARMOR_TEMPLATES` (armor) so crafted items work in combat.

pub static RECIPES: &[RecipeDef] = &[
    // ── Weapons ──────────────────────────────────────────────────────────
    RecipeDef {
        key: "dagger",
        name: "Dagger",
        category: CraftCategory::Weapons,
        result_graphic: 0x0F52, // Dagger (weapon table)
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 3 }],
        chance: 90,
        armor_rating: 0,
        layer: Some(Layer::RightHand),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "cutlass",
        name: "Cutlass",
        category: CraftCategory::Weapons,
        result_graphic: 0x1441, // Cutlass (weapon table)
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 8 }],
        chance: 75,
        armor_rating: 0,
        layer: Some(Layer::RightHand),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "broadsword",
        name: "Broadsword",
        category: CraftCategory::Weapons,
        result_graphic: 0x0F5E, // Broadsword (weapon table)
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 10 }],
        chance: 65,
        armor_rating: 0,
        layer: Some(Layer::RightHand),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "war_hammer",
        name: "War Hammer",
        category: CraftCategory::Weapons,
        result_graphic: 0x0F62, // War Hammer (weapon table)
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 16 }],
        chance: 50,
        armor_rating: 0,
        layer: Some(Layer::LeftHand),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },

    // ── Armor: chainmail (Iron Chain templates) ───────────────────────────
    RecipeDef {
        key: "chain_tunic",
        name: "Iron Chainmail Tunic",
        category: CraftCategory::Armor,
        result_graphic: 0x13BF, // Iron Chainmail Tunic
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 12 }],
        chance: 65,
        armor_rating: 14,
        layer: Some(Layer::Tunic),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "chain_legs",
        name: "Iron Chainmail Leggings",
        category: CraftCategory::Armor,
        result_graphic: 0x13BE, // Iron Chainmail Leggings
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 10 }],
        chance: 70,
        armor_rating: 12,
        layer: Some(Layer::Legs),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },

    // ── Armor: plate (Iron Plate templates) ───────────────────────────────
    RecipeDef {
        key: "plate_chest",
        name: "Iron Plate Chest",
        category: CraftCategory::Armor,
        result_graphic: 0x1415, // Iron Plate Chest
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 18 }],
        chance: 45,
        armor_rating: 20,
        layer: Some(Layer::Torso),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "plate_arms",
        name: "Iron Plate Arms",
        category: CraftCategory::Armor,
        result_graphic: 0x1410, // Iron Plate Arms
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 14 }],
        chance: 55,
        armor_rating: 16,
        layer: Some(Layer::Arms),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "plate_legs",
        name: "Iron Plate Legs",
        category: CraftCategory::Armor,
        result_graphic: 0x1411, // Iron Plate Legs
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 14 }],
        chance: 55,
        armor_rating: 16,
        layer: Some(Layer::Legs),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "plate_gloves",
        name: "Iron Plate Gloves",
        category: CraftCategory::Armor,
        result_graphic: 0x1414, // Iron Plate Gloves
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 10 }],
        chance: 60,
        armor_rating: 12,
        layer: Some(Layer::Gloves),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
    RecipeDef {
        key: "close_helmet",
        name: "Iron Close Helmet",
        category: CraftCategory::Armor,
        result_graphic: 0x1412, // Iron Close Helmet
        result_color: 0,
        ingredients: &[Ingredient { graphic: item::IRON_INGOT, amount: 12 }],
        chance: 55,
        armor_rating: 15,
        layer: Some(Layer::Helmet),
        anim: anim::SWING_2H,
        sound: craft::SOUND_SMITH,
    },
];

// ── Lookups ──────────────────────────────────────────────────────────────

/// Look up a recipe by its stable key.
pub fn lookup_recipe(key: &str) -> Option<&'static RecipeDef> {
    RECIPES.iter().find(|r| r.key == key)
}

/// All recipes in the given category, in table order.
pub fn recipes_in_category(cat: CraftCategory) -> impl Iterator<Item = &'static RecipeDef> {
    RECIPES.iter().filter(move |r| r.category == cat)
}

/// Returns `true` if the given item graphic is a smith's hammer.
pub fn is_smith_hammer(graphic: u16) -> bool {
    graphic == item::SMITH_HAMMER || graphic == item::SMITH_HAMMER_ALT
}
