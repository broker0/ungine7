//! Loot generation for killed mobiles.
//!
//! When a mobile dies, [`generate_loot`] resolves a loot table by the
//! mobile's body graphic and produces a list of [`LootItem`]s that are
//! placed into the corpse container alongside the mobile's equipment.
//!
//! Loot tables are static for now — they live in [`LOOT_TABLES`] and
//! are matched by body graphic.  In the future these could be loaded
//! from Lua scripts or data files.

use common::uo_engine::handler::LootItem;
use common::uo_engine::rpc::EngineProxy;

use crate::DemoCommand;
use crate::game_util::random_range;

// ── Loot table definitions ──────────────────────────────────────────────

/// A single entry in a loot table.
struct LootEntry {
    /// Item graphic ID.
    graphic: u16,
    /// Item colour / hue (0 = default).
    color: u16,
    /// Min-max stack amount (inclusive).
    amount_min: u16,
    amount_max: u16,
    /// Drop chance (0.0 – 1.0).  1.0 = always drops.
    chance: f32,
    /// Optional display name.
    name: Option<&'static str>,
}

/// A loot table for a specific mobile type.
struct LootTable {
    /// Gold range (min, max).  0,0 = no gold.
    gold_min: u16,
    gold_max: u16,
    /// Additional loot entries.
    items: &'static [LootEntry],
}

/// Gold graphic ID.
const GOLD_GRAPHIC: u16 = crate::constants::item::GOLD;

/// Reagent / consumable graphics for loot.
const ARROWS_GRAPHIC: u16 = 0x0F3F;
const POTION_HEAL_GRAPHIC: u16 = 0x0F0C;
const POTION_GREATER_HEAL_GRAPHIC: u16 = 0x0F0B;
const POTION_REFRESH_GRAPHIC: u16 = 0x0F0B;     // orange hue
const POTION_REFRESH_COLOR: u16 = 0x002D;
const POTION_MANA_GRAPHIC: u16 = 0x0F09;        // blue hue
const POTION_MANA_COLOR: u16 = 0x0005;
const POTION_STRENGTH_GRAPHIC: u16 = 0x0F09;    // white/golden hue
const POTION_STRENGTH_COLOR: u16 = 0x0035;
const POTION_AGILITY_GRAPHIC: u16 = 0x0F06;
const GEM_STAR_SAPPHIRE: u16 = 0x0F0F;
const GEM_EMERALD: u16 = 0x0F10;
const GEM_RUBY: u16 = 0x0F13;
const GEM_DIAMOND: u16 = 0x0F26;
const BONE_GRAPHIC: u16 = 0x0F7E;

/// Tattered treasure map (decodes into a treasure map on double-click).
/// Mirrors [`crate::treasure_map::TATTERED_MAP`]; defined locally so this
/// module compiles without the `rust-session` feature.
const TATTERED_MAP_GRAPHIC: u16 = 0x14ED;

// ── Static loot tables ──────────────────────────────────────────────────

// Body graphics for common monsters (from tiledata / spawn files).
const BODY_MONGBAT: u16 = 0x0027;        // 39
const BODY_SKELETON: u16 = 0x0039;       // 57
const BODY_ZOMBIE: u16 = 0x0003;         // 3
const BODY_ORC: u16 = 0x0011;            // 17
const BODY_ETTIN: u16 = 0x0002;          // 2
const BODY_TROLL: u16 = 0x0036;          // 54
const BODY_OGRE: u16 = 0x0001;           // 1
const BODY_LIZARDMAN: u16 = 0x0021;      // 33
const BODY_RATMAN: u16 = 0x002A;         // 42
const BODY_GAZER: u16 = 0x0016;          // 22
const BODY_HEADLESS: u16 = 0x001A;       // 26
const BODY_EARTH_ELEMENTAL: u16 = 0x000E; // 14
const BODY_FIRE_ELEMENTAL: u16 = 0x000F;  // 15
const BODY_WATER_ELEMENTAL: u16 = 0x0010; // 16
const BODY_AIR_ELEMENTAL: u16 = 0x000D;   // 13
const BODY_DAEMON: u16 = 0x000A;          // 10
const BODY_DRAGON: u16 = 0x003B;          // 59
const BODY_LICH: u16 = 0x0018;            // 24
const BODY_REAPER: u16 = 0x002F;          // 47

static LOOT_TABLES: &[(u16, LootTable)] = &[
    // ── Weak monsters ───────────────────────────────────────────────
    (BODY_MONGBAT, LootTable {
        gold_min: 10, gold_max: 50,
        items: &[],
    }),
    (BODY_HEADLESS, LootTable {
        gold_min: 15, gold_max: 60,
        items: &[],
    }),
    (BODY_ZOMBIE, LootTable {
        gold_min: 20, gold_max: 80,
        items: &[
            LootEntry { graphic: BONE_GRAPHIC, color: 0, amount_min: 1, amount_max: 3, chance: 0.5, name: Some("bone") },
        ],
    }),
    (BODY_SKELETON, LootTable {
        gold_min: 25, gold_max: 100,
        items: &[
            LootEntry { graphic: BONE_GRAPHIC, color: 0, amount_min: 1, amount_max: 5, chance: 0.6, name: Some("bone") },
            LootEntry { graphic: ARROWS_GRAPHIC, color: 0, amount_min: 5, amount_max: 15, chance: 0.3, name: None },
            LootEntry { graphic: TATTERED_MAP_GRAPHIC, color: 0, amount_min: 1, amount_max: 1, chance: 0.15, name: Some("a tattered treasure map") },
        ],
    }),

    // ── Medium monsters ─────────────────────────────────────────────
    (BODY_ORC, LootTable {
        gold_min: 50, gold_max: 150,
        items: &[
            LootEntry { graphic: ARROWS_GRAPHIC, color: 0, amount_min: 5, amount_max: 20, chance: 0.3, name: None },
            LootEntry { graphic: TATTERED_MAP_GRAPHIC, color: 0, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a tattered treasure map") },
        ],
    }),
    (BODY_RATMAN, LootTable {
        gold_min: 40, gold_max: 120,
        items: &[],
    }),
    (BODY_LIZARDMAN, LootTable {
        gold_min: 50, gold_max: 150,
        items: &[],
    }),
    (BODY_ETTIN, LootTable {
        gold_min: 80, gold_max: 200,
        items: &[
            LootEntry { graphic: POTION_HEAL_GRAPHIC, color: 0, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a heal potion") },
            LootEntry { graphic: POTION_REFRESH_GRAPHIC, color: POTION_REFRESH_COLOR, amount_min: 1, amount_max: 1, chance: 0.15, name: Some("a greater refresh potion") },
        ],
    }),
    (BODY_TROLL, LootTable {
        gold_min: 80, gold_max: 250,
        items: &[
            LootEntry { graphic: POTION_HEAL_GRAPHIC, color: 0, amount_min: 1, amount_max: 1, chance: 0.25, name: Some("a heal potion") },
            LootEntry { graphic: POTION_STRENGTH_GRAPHIC, color: POTION_STRENGTH_COLOR, amount_min: 1, amount_max: 1, chance: 0.15, name: Some("a greater strength potion") },
        ],
    }),
    (BODY_GAZER, LootTable {
        gold_min: 100, gold_max: 300,
        items: &[
            LootEntry { graphic: GEM_STAR_SAPPHIRE, color: 0, amount_min: 1, amount_max: 1, chance: 0.15, name: Some("a star sapphire") },
            LootEntry { graphic: POTION_MANA_GRAPHIC, color: POTION_MANA_COLOR, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a greater mana potion") },
        ],
    }),
    (BODY_OGRE, LootTable {
        gold_min: 100, gold_max: 350,
        items: &[
            LootEntry { graphic: GEM_RUBY, color: 0, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a ruby") },
            LootEntry { graphic: POTION_STRENGTH_GRAPHIC, color: POTION_STRENGTH_COLOR, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a greater strength potion") },
        ],
    }),

    // ── Strong monsters ─────────────────────────────────────────────
    (BODY_EARTH_ELEMENTAL, LootTable {
        gold_min: 150, gold_max: 400,
        items: &[
            LootEntry { graphic: GEM_EMERALD, color: 0, amount_min: 1, amount_max: 2, chance: 0.3, name: Some("an emerald") },
        ],
    }),
    (BODY_FIRE_ELEMENTAL, LootTable {
        gold_min: 150, gold_max: 400,
        items: &[
            LootEntry { graphic: GEM_RUBY, color: 0, amount_min: 1, amount_max: 2, chance: 0.3, name: Some("a ruby") },
        ],
    }),
    (BODY_WATER_ELEMENTAL, LootTable {
        gold_min: 150, gold_max: 400,
        items: &[
            LootEntry { graphic: GEM_STAR_SAPPHIRE, color: 0, amount_min: 1, amount_max: 2, chance: 0.3, name: Some("a star sapphire") },
        ],
    }),
    (BODY_AIR_ELEMENTAL, LootTable {
        gold_min: 150, gold_max: 400,
        items: &[
            LootEntry { graphic: GEM_DIAMOND, color: 0, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a diamond") },
        ],
    }),
    (BODY_LICH, LootTable {
        gold_min: 200, gold_max: 500,
        items: &[
            LootEntry { graphic: GEM_DIAMOND, color: 0, amount_min: 1, amount_max: 1, chance: 0.25, name: Some("a diamond") },
            LootEntry { graphic: BONE_GRAPHIC, color: 0, amount_min: 2, amount_max: 5, chance: 0.5, name: Some("bone") },
            LootEntry { graphic: POTION_MANA_GRAPHIC, color: POTION_MANA_COLOR, amount_min: 1, amount_max: 2, chance: 0.3, name: Some("a greater mana potion") },
        ],
    }),
    (BODY_REAPER, LootTable {
        gold_min: 200, gold_max: 500,
        items: &[
            LootEntry { graphic: GEM_EMERALD, color: 0, amount_min: 1, amount_max: 2, chance: 0.3, name: Some("an emerald") },
        ],
    }),

    // ── Boss-tier ────────────────────────────────────────────────────
    (BODY_DAEMON, LootTable {
        gold_min: 300, gold_max: 800,
        items: &[
            LootEntry { graphic: GEM_DIAMOND, color: 0, amount_min: 1, amount_max: 2, chance: 0.4, name: Some("a diamond") },
            LootEntry { graphic: GEM_RUBY, color: 0, amount_min: 1, amount_max: 3, chance: 0.3, name: Some("a ruby") },
            LootEntry { graphic: POTION_GREATER_HEAL_GRAPHIC, color: 0, amount_min: 1, amount_max: 2, chance: 0.25, name: Some("a greater heal potion") },
            LootEntry { graphic: POTION_AGILITY_GRAPHIC, color: 0, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a greater agility potion") },
        ],
    }),
    (BODY_DRAGON, LootTable {
        gold_min: 500, gold_max: 1500,
        items: &[
            LootEntry { graphic: GEM_DIAMOND, color: 0, amount_min: 2, amount_max: 5, chance: 0.5, name: Some("a diamond") },
            LootEntry { graphic: GEM_STAR_SAPPHIRE, color: 0, amount_min: 1, amount_max: 3, chance: 0.4, name: Some("a star sapphire") },
            LootEntry { graphic: GEM_EMERALD, color: 0, amount_min: 1, amount_max: 3, chance: 0.4, name: Some("an emerald") },
            LootEntry { graphic: POTION_GREATER_HEAL_GRAPHIC, color: 0, amount_min: 1, amount_max: 3, chance: 0.35, name: Some("a greater heal potion") },
            LootEntry { graphic: POTION_STRENGTH_GRAPHIC, color: POTION_STRENGTH_COLOR, amount_min: 1, amount_max: 1, chance: 0.2, name: Some("a greater strength potion") },
        ],
    }),
];

/// Default loot table for humanoid NPCs (human body graphic).
/// Humanoids drop only equipment (transferred automatically) + small gold.
static DEFAULT_HUMANOID: LootTable = LootTable {
    gold_min: 30, gold_max: 200,
    items: &[],
};

/// Fallback loot table for unknown monsters.
static DEFAULT_MONSTER: LootTable = LootTable {
    gold_min: 10, gold_max: 50,
    items: &[],
};

// ── Public API ──────────────────────────────────────────────────────────

/// Generate loot items for a killed mobile.
///
/// Queries the mobile's body graphic from the engine, looks up the
/// corresponding loot table, and produces a list of [`LootItem`]s.
///
/// The returned items do NOT include equipment — those are transferred
/// automatically by the `KillMobile` engine command.
pub async fn generate_loot(
    target_serial: u32,
    engine: &EngineProxy<DemoCommand>,
) -> Vec<LootItem> {
    let entity = engine.get_entity(target_serial).await;
    let Some(m) = entity.as_ref().and_then(|e| e.mobile()) else {
        return Vec::new();
    };

    let body = m.graphic;
    generate_loot_for_body(body)
}

/// Generate loot items from a loot table based on body graphic.
///
/// Separated from [`generate_loot`] for testability and reuse from
/// Lua scripts.
pub fn generate_loot_for_body(body_graphic: u16) -> Vec<LootItem> {
    // Find the matching loot table.
    let table = LOOT_TABLES
        .iter()
        .find(|(bg, _)| *bg == body_graphic)
        .map(|(_, t)| t)
        .unwrap_or_else(|| {
            // Humanoid bodies (male/female human) get the humanoid table.
            if body_graphic == 0x0190 || body_graphic == 0x0191 {
                &DEFAULT_HUMANOID
            } else {
                &DEFAULT_MONSTER
            }
        });

    let mut items = Vec::new();

    // Gold.
    if table.gold_max > 0 {
        let amount = random_range(table.gold_min, table.gold_max);
        if amount > 0 {
            items.push(LootItem {
                graphic: GOLD_GRAPHIC,
                color: 0,
                amount,
                name: None,
            });
        }
    }

    // Additional items.
    for entry in table.items {
        if roll_chance(entry.chance) {
            let amount = random_range(entry.amount_min, entry.amount_max);
            if amount > 0 {
                items.push(LootItem {
                    graphic: entry.graphic,
                    color: entry.color,
                    amount,
                    name: entry.name.map(|s| s.to_string()),
                });
            }
        }
    }

    items
}

/// Roll a random chance (0.0 – 1.0).
fn roll_chance(chance: f32) -> bool {
    use rand::Rng;
    rand::rng().random::<f32>() < chance
}
