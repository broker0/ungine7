//! Treasure map mechanic — definition tables.
//!
//! Data-driven treasure-hunting system (mirrors `gathering` / `loot`).
//!
//! ## Items
//!
//! - **tattered map** ([`TATTERED_MAP`]) — drops from monster corpses.
//!   Double-clicking it "decodes" it: the tattered map is consumed and a
//!   **treasure map** ([`TREASURE_MAP`]) is created in the backpack.  At
//!   decode time a random treasure [`TreasureLocation`] is chosen and stored
//!   in the new map's `ItemProps` meta ([`META_TREASURE_LOC`]) together with
//!   the map [`META_TREASURE_LEVEL`].
//! - **treasure map** ([`TREASURE_MAP`]) — double-clicking it opens the
//!   region map (a `MapMessage`), showing where to dig.
//! - **treasure digging tool** ([`DIGGING_TOOL`]) — a consumable tool.
//!   Double-click it, target a treasure map, then target the ground tile.
//!   If the tile matches the map's buried location, a chest + guardians are
//!   spawned.  The tool is consumed on a successful dig.
//!
//! ## Levels
//!
//! Each [`TreasureLevel`] defines the guardians spawned and the loot rolled
//! into the chest.  The level is a property of the map (chosen on decode);
//! the table is built to scale to multiple levels — currently only one
//! demonstration level exists.

#![allow(dead_code)]

use common::uo_engine::handler::LootItem;

use crate::game_util::random_range;

// ── Item graphics ──────────────────────────────────────────────────────────

/// A tattered (un-decoded) treasure map that drops from monsters.
pub const TATTERED_MAP: u16 = 0x14ED;
/// A decoded treasure map (double-click to view the region, target with the
/// digging tool to dig).
pub const TREASURE_MAP: u16 = 0x14EB;
/// The consumable treasure digging tool (shovel).
pub const DIGGING_TOOL: u16 = 0x0FB7;
/// The buried treasure chest graphic (spawned on a successful dig).
pub const TREASURE_CHEST: u16 = 0x0E40;
/// Container gump model used when the chest is opened.
pub const CHEST_GUMP: u16 = 66;

// ── Meta keys ────────────────────────────────────────────────────────────

/// `ItemProps` meta key: the treasure location id this map points to.
pub const META_TREASURE_LOC: &str = "treasure_loc";
/// `ItemProps` meta key: the treasure level (`1..`).
pub const META_TREASURE_LEVEL: &str = "treasure_level";

// ── Tunables ───────────────────────────────────────────────────────────────

/// Maximum Chebyshev distance (tiles) the dig tile may differ from the buried
/// location and still count as a hit.
pub const DIG_RANGE: u16 = 1;
/// Maximum Chebyshev distance the player may be from the dig tile.
pub const PLAYER_DIG_RANGE: u16 = 2;
/// Time (ms) a dig takes before completion.
pub const DIG_DELAY_MS: u64 = 3000;
/// Sound played while digging (pickaxe striking stone — reused).
pub const DIG_SOUND: u16 = 0x0125;
/// Gump art id used for the region map shown when a treasure map is opened.
pub const MAP_GUMP_ART: u16 = 5021;
/// Half-extent (tiles) of the square region drawn on the opened map.
pub const MAP_REGION_HALF: u16 = 260;
/// Pixel size of the opened map gump.
pub const MAP_GUMP_SIZE: u16 = 200;

// ── Guardian definition ──────────────────────────────────────────────────

/// A guardian monster spawned around a dug-up chest.
#[derive(Debug, Clone, Copy)]
pub struct GuardianDef {
    /// Body graphic of the guardian.
    pub graphic: u16,
    /// Display name.
    pub name: &'static str,
    /// Hit points.
    pub hits: u16,
    /// Strength / dexterity / intelligence.
    pub str_: u16,
    pub dex: u16,
    pub int_: u16,
    /// AI aggro range (Chebyshev tiles).
    pub aggro_range: u16,
    /// AI leash range.
    pub leash_range: u16,
    /// Melee damage range.
    pub damage_min: u16,
    pub damage_max: u16,
    /// Milliseconds between swings.
    pub swing_delay_ms: u64,
}

// ── Loot entry ─────────────────────────────────────────────────────────────

/// A possible loot drop placed into the chest.
#[derive(Debug, Clone, Copy)]
pub struct ChestLootEntry {
    pub graphic: u16,
    pub color: u16,
    pub amount_min: u16,
    pub amount_max: u16,
    /// Drop chance (0.0 – 1.0).
    pub chance: f32,
    pub name: Option<&'static str>,
}

// ── Level definition ─────────────────────────────────────────────────────

/// A treasure map level.
#[derive(Debug, Clone, Copy)]
pub struct TreasureLevel {
    /// Level number (`1..`).
    pub level: u8,
    /// Guardians spawned when the chest is dug up.
    pub guardians: &'static [GuardianDef],
    /// Gold placed in the chest (min, max).
    pub gold_min: u16,
    pub gold_max: u16,
    /// Additional loot entries.
    pub loot: &'static [ChestLootEntry],
    /// How long (seconds) the chest + guardians persist before despawning.
    pub decay_secs: u64,
}

// ── Location definition ────────────────────────────────────────────────────

/// A buried-treasure location.
#[derive(Debug, Clone, Copy)]
pub struct TreasureLocation {
    /// Stable id stored in the map's meta.
    pub id: u32,
    /// Map / facet id.
    pub map_id: u8,
    /// Buried tile coordinates.
    pub x: u16,
    pub y: u16,
    pub z: i8,
}

// ── Graphics reused for loot ───────────────────────────────────────────────

const GOLD_GRAPHIC: u16 = crate::constants::item::GOLD;
const GEM_RUBY: u16 = 0x0F13;
const GEM_EMERALD: u16 = 0x0F10;
const GEM_DIAMOND: u16 = 0x0F26;
const POTION_GREATER_HEAL: u16 = 0x0F0B;

// Guardian body graphics (match `spawn_points::default_config`).
const BODY_ORC: u16 = 0x0011;
const BODY_SKELETON: u16 = 0x0032;

// ── Tables ─────────────────────────────────────────────────────────────────

/// Level-1 guardians: two skeletons and an orc.
static LEVEL1_GUARDIANS: &[GuardianDef] = &[
    GuardianDef {
        graphic: BODY_SKELETON, name: "a skeletal guardian",
        hits: 50, str_: 50, dex: 50, int_: 30,
        aggro_range: 10, leash_range: 25, damage_min: 4, damage_max: 12,
        swing_delay_ms: 3000,
    },
    GuardianDef {
        graphic: BODY_SKELETON, name: "a skeletal guardian",
        hits: 50, str_: 50, dex: 50, int_: 30,
        aggro_range: 10, leash_range: 25, damage_min: 4, damage_max: 12,
        swing_delay_ms: 3000,
    },
    GuardianDef {
        graphic: BODY_ORC, name: "an orcish guardian",
        hits: 80, str_: 60, dex: 50, int_: 30,
        aggro_range: 10, leash_range: 25, damage_min: 8, damage_max: 18,
        swing_delay_ms: 2500,
    },
];

/// Level-1 chest loot.
static LEVEL1_LOOT: &[ChestLootEntry] = &[
    ChestLootEntry { graphic: GEM_RUBY, color: 0, amount_min: 1, amount_max: 3, chance: 0.6, name: Some("a ruby") },
    ChestLootEntry { graphic: GEM_EMERALD, color: 0, amount_min: 1, amount_max: 3, chance: 0.5, name: Some("an emerald") },
    ChestLootEntry { graphic: GEM_DIAMOND, color: 0, amount_min: 1, amount_max: 1, chance: 0.25, name: Some("a diamond") },
    ChestLootEntry { graphic: POTION_GREATER_HEAL, color: 0, amount_min: 1, amount_max: 2, chance: 0.4, name: Some("a greater heal potion") },
];

/// All treasure levels.  Currently a single demonstration level.
pub static TREASURE_LEVELS: &[TreasureLevel] = &[TreasureLevel {
    level: 1,
    guardians: LEVEL1_GUARDIANS,
    gold_min: 500,
    gold_max: 1500,
    loot: LEVEL1_LOOT,
    decay_secs: 180,
}];

/// All buried-treasure locations.  A decoded map picks one of these at random.
pub static TREASURE_LOCATIONS: &[TreasureLocation] = &[
    TreasureLocation { id: 1, map_id: 0, x: 1420, y: 1690, z: 0 },
    TreasureLocation { id: 2, map_id: 0, x: 1500, y: 1620, z: 0 },
    TreasureLocation { id: 3, map_id: 0, x: 1360, y: 1740, z: 0 },
];

// ── Lookups ──────────────────────────────────────────────────────────────

/// Look up a treasure level by its level number.
pub fn lookup_level(level: u8) -> Option<&'static TreasureLevel> {
    TREASURE_LEVELS.iter().find(|l| l.level == level)
}

/// Look up a treasure location by its id.
pub fn lookup_location(id: u32) -> Option<&'static TreasureLocation> {
    TREASURE_LOCATIONS.iter().find(|l| l.id == id)
}

/// Pick a random treasure location id for a freshly decoded map.
pub fn random_location_id() -> u32 {
    use rand::Rng;
    let idx = rand::rng().random_range(0..TREASURE_LOCATIONS.len());
    TREASURE_LOCATIONS[idx].id
}

/// Distinct hues assigned to decoded treasure maps so that maps pointing to
/// different locations never auto-merge into one stack in the backpack (the
/// drop logic stacks items with the same graphic + hue).  Indexed by
/// `location id`.
const MAP_HUES: &[u16] = &[0x0021, 0x0026, 0x002B, 0x0030, 0x0035, 0x0040, 0x0044, 0x0048];

/// Hue to assign a decoded treasure map for a given buried-location id.
pub fn map_hue_for_location(loc_id: u32) -> u16 {
    MAP_HUES[(loc_id as usize) % MAP_HUES.len()]
}

/// Roll the level for a tattered map decoded from a given monster body.
///
/// Currently always level 1.  In the future this can scale with the monster
/// (weaker monsters → low-level maps, bosses → high-level maps).
pub fn roll_level_for_body(_body_graphic: u16) -> u8 {
    1
}

impl TreasureLevel {
    /// Roll the chest loot for this level (gold + items) as `LootItem`s.
    pub fn roll_loot(&self) -> Vec<LootItem> {
        let mut items = Vec::new();

        if self.gold_max > 0 {
            let amount = random_range(self.gold_min, self.gold_max);
            if amount > 0 {
                items.push(LootItem { graphic: GOLD_GRAPHIC, color: 0, amount, name: None });
            }
        }

        for entry in self.loot {
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
}

/// Roll a random chance (0.0 – 1.0).
fn roll_chance(chance: f32) -> bool {
    use rand::Rng;
    rand::rng().random::<f32>() < chance
}
