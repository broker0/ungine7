//! Named constants for UO game data: sounds, effects, animations, bodies, colors.
//!
//! Using named constants instead of hex literals makes the code self-documenting
//! and keeps the Rust server aligned with the Lua `scene/constants.lua` table.
//!
//! ## Extending
//!
//! Add new constants as needed.  Keep the module structure and naming consistent
//! with the Lua `constants.lua` file so that both sides use the same vocabulary.

// ── Sound effect IDs ──────────────────────────────────────────────────────

/// Sound effect IDs sent via `BroadcastSound` / `play_sound`.
#[allow(dead_code)]
pub mod sound {
    // Spells / magic
    pub const MAGIC_ARROW: u16      = 0x01E5;
    pub const HEAL: u16             = 0x01F2;
    pub const LIGHTNING: u16        = 0x0029;
    pub const ENERGY_BOLT: u16      = 0x020A;
    pub const FLAMESTRIKE: u16      = 0x0208;
    pub const TELEPORT: u16         = 0x01FE;
    pub const MANA_DRAIN: u16       = 0x01F8;
    pub const SUMMON: u16           = 0x0217;
    pub const CURSE: u16            = 0x01FC;
    pub const BLESS: u16            = 0x0202;
    pub const POISON: u16           = 0x0205;
    pub const RESURRECT: u16        = 0x0214;
    /// Sound played when a rune is marked (Mark spell).
    pub const MARK: u16             = 0x01FA;
    /// Sound played on arrival from a Recall.
    pub const RECALL: u16           = 0x01FC;
    /// Sound played per stone block when Wall of Stone is cast (0x01F6 = 502).
    pub const WALL_OF_STONE: u16    = 0x01F6;

    // Combat / impacts
    pub const HEAVY_SWORD_1: u16          = 0x0236;
    pub const HEAVY_SWORD_4: u16          = 0x0237;
    pub const SWORD_1: u16        = 0x023B;
    pub const SWORD_7: u16        = 0x023C;
    pub const MACE_HIT: u16         = 0x0233;
    pub const ARROW_HIT: u16        = 0x0234;
    pub const SHIELD_BLOCK: u16     = 0x023C;
    pub const MISS_1: u16             = 0x0238;
    pub const MISS_2: u16             = 0x0239;
    pub const MISS_3: u16             = 0x023A;
    pub const PUNCH: u16            = 0x0135;
    pub const SWING: u16            = 0x0159;

    // Spell fizzle
    pub const FIZZLE: u16           = 0x005C;

    // Creature sounds
    pub const DRAGON_ROAR: u16      = 0x016C;
    pub const WOLF_HOWL: u16        = 0x00E5;
    pub const HORSE_WHINNY: u16     = 0x00A8;
    pub const SKELETON_RATTLE: u16  = 0x01C3;

    // Ambient / environment
    pub const THUNDER: u16          = 0x0029;
    pub const DOOR_OPEN: u16        = 0x00EA;
    pub const DOOR_CLOSE: u16       = 0x00F1;
    pub const CAMPFIRE: u16         = 0x0225;
    pub const ANVIL_STRIKE: u16     = 0x002A;

    // UI / feedback
    pub const COINS: u16            = 0x0037;
    pub const DRINK: u16            = 0x0031;
    pub const EAT: u16              = 0x003A;

    // Character hurt sounds (pain vocalisations)
    pub const MALE_HURT_1: u16      = 0x0154;
    pub const MALE_HURT_2: u16      = 0x0155;
    pub const MALE_HURT_3: u16      = 0x0156;
    pub const MALE_HURT_4: u16      = 0x0157;
    pub const MALE_HURT_5: u16      = 0x0158;
    pub const FEMALE_HURT_1: u16    = 0x014B;
    pub const FEMALE_HURT_2: u16    = 0x014C;
    pub const FEMALE_HURT_3: u16    = 0x014D;
    pub const FEMALE_HURT_4: u16    = 0x014E;
    pub const FEMALE_HURT_5: u16    = 0x014F;
}

// ── Visual effect graphic IDs ─────────────────────────────────────────────

/// Graphical effect IDs used with `BroadcastEffect` / `effect()`.
#[allow(dead_code)]
pub mod effect {
    // Spell effects (projectiles and on-target)
    pub const MAGIC_ARROW: u16      = 0x36E4;
    pub const HEAL_SPARKLE: u16     = 0x375A;
    pub const ENERGY_BOLT: u16      = 0x379F;
    pub const FLAMESTRIKE: u16      = 0x3709;
    pub const EXPLOSION: u16        = 0x36BD;
    pub const FIREBALL: u16         = 0x36D4;
    pub const TELEPORT: u16         = 0x3728;
    pub const POISON_CLOUD: u16     = 0x3400;
    pub const PARALYZE_FIELD: u16   = 0x3818;
    pub const FIRE_FIELD: u16       = 0x3996;

    // Environmental effects
    pub const SPARKLE: u16          = 0x373A;
    /// Sparkle shown when a rune is marked (Mark spell).
    pub const RECALL_SPARKLE: u16   = 0x3779;

    // Spell fizzle
    pub const FIZZLE: u16           = 0x3735;
}

// ── Character animation action IDs ────────────────────────────────────────

/// Animation action IDs for humanoid body types (0x0190 / 0x0191).
///
/// Monster bodies use different action IDs — consult UO animation tables.
#[allow(dead_code)]
pub mod anim {
    // Locomotion
    pub const WALK: u16             = 0x00;
    pub const WALK_WEAPON: u16      = 0x01;
    pub const RUN: u16              = 0x02;
    pub const RUN_WEAPON: u16       = 0x03;
    pub const STAND: u16            = 0x04;
    pub const FIDGET_1: u16         = 0x05;
    pub const FIDGET_2: u16         = 0x06;

    // Melee combat
    pub const SLASH_1H: u16         = 0x09;
    pub const PIERCE_1H: u16        = 0x0A;
    pub const SWING_2H: u16         = 0x0B;
    pub const SLASH_2H: u16         = 0x0C;
    pub const PIERCE_2H: u16        = 0x0D;

    // Ranged combat
    pub const SHOOT_BOW: u16        = 0x12;
    pub const SHOOT_XBOW: u16       = 0x13;

    // Damage / death
    pub const GET_HIT: u16          = 0x14;
    pub const DIE_FORWARD: u16      = 0x15;
    pub const DIE_BACKWARD: u16     = 0x16;

    // Magic — on foot
    pub const CAST_AREA: u16        = 0x10;
    pub const CAST_DIRECTED: u16    = 0x11;

    // Mounted
    pub const MOUNTED_WALK: u16     = 0x17;
    pub const MOUNTED_RUN: u16      = 0x18;
    pub const MOUNTED_STAND: u16    = 0x19;
    pub const MOUNTED_ATTACK: u16        = 0x1A;
    pub const MOUNTED_CAST_DIRECTED: u16 = 0x1B;
    pub const MOUNTED_CAST_AREA: u16     = 0x1C;
    pub const MOUNTED_GET_HIT: u16       = 0x1D;

    // Emotes / social
    pub const BOW: u16              = 0x20;
    pub const SALUTE: u16           = 0x21;
    pub const EAT: u16              = 0x22;
}

// ── Body graphic IDs ──────────────────────────────────────────────────────

/// Mobile body graphic IDs.
#[allow(dead_code)]
pub mod body {
    pub const MALE_HUMAN: u16       = 0x0190;
    pub const FEMALE_HUMAN: u16     = 0x0191;
}

// ── Geometry ──────────────────────────────────────────────────────────────

/// Eye height offset added to a mobile's Z for line-of-sight calculations.
///
/// Used when checking LOS from caster → target, melee attacker → defender,
/// healer → bandage target, etc.
pub const EYE_HEIGHT: i16 = 14;

// ── Colors / hues ─────────────────────────────────────────────────────────

/// Commonly used hue / color values.
#[allow(dead_code)]
pub mod hue {
    // System message colors
    pub const SYSTEM_RED: u16       = 0x0025;
    pub const SYSTEM_GRAY: u16      = 0x03B2;
    pub const SPELL_WORDS: u16      = 0x03E8;

    // Notoriety overhead name hues
    pub const NOTO_INNOCENT: u16    = 0x059;   // blue
    pub const NOTO_ALLY: u16        = 0x043;   // green
    pub const NOTO_GRAY: u16        = 0x3B2;   // gray (attackable, criminal, etc.)
    pub const NOTO_ENEMY: u16       = 0x030;   // orange
    pub const NOTO_MURDERER: u16    = 0x026;   // red

    // Special
    pub const GOLDEN_HEALTH: u16    = 0x035;
    pub const TEST_PLAYER: u16      = 0x0481;
    /// Heal feedback overhead text color (green).
    pub const HEAL_FEEDBACK: u16    = 0x0039;
}

// ── Speech types (wire values) ────────────────────────────────────────────

/// Speech type wire values used in `BroadcastSpeech`.
#[allow(dead_code)]
pub mod speech_type {
    pub const SPELL: u8             = 0x0A;
    /// Emote speech (`* growls *`), shown in a distinct style by the client.
    pub const EMOTE: u8            = 0x02;
}

// ── Item graphic IDs ──────────────────────────────────────────────────────

/// Commonly referenced item graphic IDs.
#[allow(dead_code)]
pub mod item {
    pub const HORSE_MOUNT: u16      = 0x3E9F;
    pub const BANDAGE: u16          = 0x0E21;

    /// Gold coin (stackable currency).  Canonical graphic shared by vendors,
    /// loot, treasure and weight/gold computations.
    pub const GOLD: u16             = 0x0EED;

    /// Magery spellbook (double-click to open the spell list).
    pub const SPELLBOOK: u16        = 0x0EFA;

    // Spell scroll graphics.
    // Formula: graphic = 0x1F2C + spell_id (UO spellbook order).
    pub const SCROLL_HEAL: u16          = 0x1F31;  // spell  4
    pub const SCROLL_MAGIC_ARROW: u16   = 0x1F32;  // spell  5
    pub const SCROLL_BLESS: u16         = 0x1F3D;  // spell 17
    pub const SCROLL_CURSE: u16         = 0x1F47;  // spell 27
    pub const SCROLL_GREATER_HEAL: u16  = 0x1F49;  // spell 29
    pub const SCROLL_LIGHTNING: u16     = 0x1F4A;  // spell 30
    pub const SCROLL_ENERGY_BOLT: u16   = 0x1F56;  // spell 42
    pub const SCROLL_FLAMESTRIKE: u16   = 0x1F5F;  // spell 51

    // Recall rune (used by the Mark / Recall spells).
    pub const RUNE: u16                 = 0x1F14;

    /// Stone block spawned by the Wall of Stone spell (graphic 0x0080 = 128).
    pub const WALL_OF_STONE_BLOCK: u16  = 0x0080;

    // Reagent graphics.
    pub const REAGENT_BLACK_PEARL: u16    = 0x0F7A;
    pub const REAGENT_BLOOD_MOSS: u16     = 0x0F7B;
    pub const REAGENT_GARLIC: u16         = 0x0F84;
    pub const REAGENT_GINSENG: u16        = 0x0F85;
    pub const REAGENT_MANDRAKE_ROOT: u16  = 0x0F86;
    pub const REAGENT_NIGHTSHADE: u16     = 0x0F88;
    pub const REAGENT_SULPHUROUS_ASH: u16 = 0x0F8C;
    pub const REAGENT_SPIDERS_SILK: u16   = 0x0F8D;

    // Potion graphics.
    pub const POTION_GREATER_HEAL: u16    = 0x0F0B;  // yellow potion bottle
    pub const POTION_HEAL: u16            = 0x0F0C;  // red potion bottle
    pub const POTION_REFRESH: u16         = 0x0F0B;  // reuse yellow bottle (orange hue)
    pub const POTION_MANA: u16            = 0x0F09;  // reuse blue bottle
    pub const POTION_CURE: u16            = 0x0F07;  // green potion bottle
    pub const POTION_STRENGTH: u16        = 0x0F09;  // white potion bottle
    pub const POTION_AGILITY: u16         = 0x0F06;  // blue potion bottle

    /// Poison potion bottle (green liquid).  All poison levels share this
    /// graphic; the level is stored per-instance in `ItemProps.meta`
    /// (`poison_level`) rather than being encoded in the bottle's hue.
    pub const POTION_POISON: u16          = 0x0F0A;

    /// Shrink potion bottle.  Shares the agility-bottle graphic (`0x0F06`)
    /// but is distinguished by its hue [`SHRINK_POTION_HUE`], so it resolves
    /// to a distinct `(graphic, color)` pair in `crate::potions`.
    pub const POTION_SHRINK: u16          = 0x0F06;
    /// Hue that distinguishes the shrink potion from the agility potion.
    pub const SHRINK_POTION_HUE: u16      = 0x0488;

    /// Item graphic used for a "shrunken animal" statue carried in the
    /// backpack.  Double-clicking it re-spawns the stored creature.
    pub const SHRINK_STATUE: u16          = 0x2106;

    // ── Crafting (smelting + blacksmithing) ──────────────────────────────
    /// Iron ore (mined with a pickaxe, smelted at a forge into ingots).
    pub const IRON_ORE: u16               = 0x19B9;
    /// Iron ingot (produced by smelting ore; the blacksmith's raw material).
    pub const IRON_INGOT: u16             = 0x1BF2;
    /// Smith's hammer — double-clicked near an anvil to open the craft menu.
    pub const SMITH_HAMMER: u16           = 0x13E3;
    /// Alternate smith's hammer art.
    pub const SMITH_HAMMER_ALT: u16       = 0x13E4;
    /// Anvil world-object graphic (required nearby to forge/repair).
    pub const ANVIL: u16                  = 0x0FB0;
    /// Forge world-object graphic (required nearby to smelt ore).
    pub const FORGE: u16                  = 0x0FB1;
}

// ── Crafting ────────────────────────────────────────────────────────────────

/// Blacksmithing / smelting configuration.
pub mod craft {
    /// Maximum Chebyshev distance (tiles) to a forge/anvil world object.
    pub const RANGE: u16 = 2;
    /// Delay before a smelt action completes (milliseconds).
    pub const SMELT_DELAY_MS: u64 = 2000;
    /// Delay before a blacksmithing action completes (milliseconds).
    pub const CRAFT_DELAY_MS: u64 = 3000;
    /// Sound played while smithing (hammer on anvil).
    pub const SOUND_SMITH: u16 = 0x002A;
    /// Sound played while smelting ore at a forge.
    pub const SOUND_SMELT: u16 = 0x002B;
    /// Forge world-object graphics that allow smelting nearby.
    pub static FORGE_GRAPHICS: &[u16] = &[super::item::FORGE];
    /// Anvil world-object graphics that allow smithing nearby.
    pub static ANVIL_GRAPHICS: &[u16] = &[super::item::ANVIL];
}

// ── Skill IDs ─────────────────────────────────────────────────────────────

/// UO skill IDs (matching the client skill list).
///
/// These are the official 0-based UO skill IDs (`0` = Alchemy .. `57` =
/// Throwing), the same numbering used by the 0x3A skill packet and by the
/// client's `skills.mul`.
#[allow(dead_code)]
pub mod skill_id {
    pub const ALCHEMY: u16          = 0;
    pub const ANATOMY: u16          = 1;
    pub const ANIMAL_LORE: u16      = 2;
    pub const ITEM_ID: u16          = 3;
    pub const ARMS_LORE: u16        = 4;
    pub const BATTLE_DEFENSE: u16   = 5;
    pub const BEGGING: u16          = 6;
    pub const BLACKSMITHING: u16    = 7;
    pub const BOWCRAFT: u16         = 8;
    pub const PEACEMAKING: u16      = 9;
    pub const CAMPING: u16          = 10;
    pub const CARPENTRY: u16        = 11;
    pub const CARTOGRAPHY: u16      = 12;
    pub const COOKING: u16          = 13;
    pub const DETECTING_HIDDEN: u16 = 14;
    pub const ENTICEMENT: u16       = 15;
    pub const EVAL_INT: u16         = 16;
    pub const HEALING: u16          = 17;
    pub const FISHING: u16          = 18;
    pub const FORENSICS: u16        = 19;
    pub const HERDING: u16          = 20;
    pub const HIDING: u16           = 21;
    pub const PROVOCATION: u16      = 22;
    pub const INSCRIPTION: u16      = 23;
    pub const LOCKPICKING: u16      = 24;
    pub const MAGERY: u16           = 25;
    pub const RESIST_SPELLS: u16    = 26;
    pub const TACTICS: u16          = 27;
    pub const SNOOPING: u16         = 28;
    pub const MUSICIANSHIP: u16     = 29;
    pub const POISONING: u16        = 30;
    pub const ARCHERY: u16          = 31;
    pub const SPIRIT_SPEAK: u16     = 32;
    pub const STEALING: u16         = 33;
    pub const TAILORING: u16        = 34;
    pub const ANIMAL_TAMING: u16    = 35;
    pub const TASTE_ID: u16         = 36;
    pub const TINKERING: u16        = 37;
    pub const TRACKING: u16         = 38;
    pub const VETERINARY: u16       = 39;
    pub const SWORDS: u16           = 40;
    pub const MACE_FIGHTING: u16    = 41;
    pub const FENCING: u16          = 42;
    pub const WRESTLING: u16        = 43;
    pub const LUMBERJACKING: u16    = 44;
    pub const MINING: u16           = 45;
    pub const MEDITATION: u16       = 46;
    pub const STEALTH: u16          = 47;
    pub const REMOVE_TRAP: u16      = 48;
    pub const NECROMANCY: u16       = 49;
    pub const BATTLE_FOCUS: u16     = 50;
    pub const CHIVALRY: u16         = 51;
    pub const BUSHIDO: u16          = 52;
    pub const NINJITSU: u16         = 53;
    pub const SPELLWEAVING: u16     = 54;
    pub const MYSTICISM: u16        = 55;
    pub const IMBUING: u16          = 56;
    pub const THROWING: u16         = 57;
}

/// `ItemProps.meta` keys for per-instance item properties.
///
/// These are the string keys used with `ItemProps::get_meta_int` /
/// `set_meta`.  Centralised here so producers (crafting, dev items) and
/// consumers (equipment_calc) agree on the spelling.
pub mod meta_key {
    /// Per-instance armor rating override (Int, raw AR points).
    pub const ARMOR_RATING: &str = "armor_rating";
    /// Skill-bonus weapon: the UO skill id the bonus applies to (Int).
    pub const SKILL_BONUS_ID: &str = "skill_bonus_id";
    /// Skill-bonus weapon: the bonus amount in **tenths** (Int, e.g. 50 = +5.0).
    pub const SKILL_BONUS_AMOUNT: &str = "skill_bonus_amount";
}

// ── Skills ────────────────────────────────────────────────────────────────

/// Skill timing configuration.
pub mod skill_timing {
    /// Arms Lore evaluation delay (milliseconds).
    pub const ARMS_LORE_DELAY_MS: u64 = 5000;
    /// Poisoning (weapon coating) delay (milliseconds).
    pub const POISONING_DELAY_MS: u64 = 2000;
}

/// Wall of Stone spell configuration.
pub mod wall_of_stone {
    /// Number of stone blocks spawned in a row.
    pub const BLOCK_COUNT: u32 = 6;
    /// Delay (seconds) before the first block begins to decay.
    pub const FIRST_DECAY_SECS: u64 = 90;
    /// Interval (seconds) between successive block removals.
    pub const DECAY_INTERVAL_SECS: u64 = 15;
}

/// Bandage (healing) configuration.
pub mod bandage {
    /// Delay before bandage healing completes (milliseconds).
    pub const DELAY_MS: u64 = 3000;
    /// Maximum Chebyshev distance (tiles) to the target.
    pub const RANGE: u16 = 2;
    /// Minimum HP healed per bandage application.
    pub const HEAL_MIN: u16 = 15;
    /// Maximum HP healed per bandage application.
    pub const HEAL_MAX: u16 = 30;
    /// Sound played on bandage completion.
    pub const SOUND: u16 = 0x0048;
}

/// Potion system configuration.
pub mod potion {
    /// Global cooldown between potion uses (milliseconds).
    ///
    /// Classic UO uses approximately 10 seconds between potions.
    pub const COOLDOWN_MS: u64 = 10_000;

    /// Sound played when drinking a potion.
    pub const DRINK_SOUND: u16 = super::sound::DRINK;

    /// Animation played when drinking a potion (eat/drink gesture).
    pub const DRINK_ANIM: u16 = super::anim::EAT;

    /// Duration of Strength buff (milliseconds).
    pub const STRENGTH_DURATION_MS: u64 = 120_000;
    /// Strength bonus from Greater Strength Potion.
    pub const STRENGTH_BONUS: i16 = 10;

    /// Duration of Agility buff (milliseconds).
    pub const AGILITY_DURATION_MS: u64 = 120_000;
    /// Dexterity bonus from Greater Agility Potion.
    pub const AGILITY_BONUS: i16 = 10;
}

/// Shrink potion configuration.
///
/// A shrink potion turns one of the player's own tamed animals into a
/// carryable statue item; double-clicking the statue re-spawns the pet.
pub mod shrink {
    /// Maximum Chebyshev distance (tiles) to the animal being shrunk.
    pub const RANGE: u16 = 4;
}

/// Poison system configuration.
///
/// Poison has four levels (`1..=4` = Lesser / Regular / Greater / Deadly).
/// Each level defines the damage dealt per tick, how long the poison lasts,
/// the chance to infect a target on a weapon hit, and the chance a cure
/// potion succeeds.  All per-level arrays are indexed by `level - 1`.
pub mod poison {
    /// Interval between poison damage ticks (milliseconds).
    pub const TICK_INTERVAL_MS: u64 = 2_000;

    /// Number of levels (Lesser, Regular, Greater, Deadly).
    pub const LEVELS: usize = 4;

    /// Damage dealt per tick, indexed by `level - 1`.
    pub const DAMAGE_PER_TICK: [u16; LEVELS] = [2, 4, 7, 12];

    /// Total poison duration in milliseconds, indexed by `level - 1`.
    pub const DURATION_MS: [u64; LEVELS] = [10_000, 14_000, 18_000, 24_000];

    /// Chance (percent) that a poisoned weapon infects the target on a
    /// successful hit, indexed by `level - 1`.
    pub const APPLY_CHANCE_PCT: [u32; LEVELS] = [40, 45, 50, 60];

    /// Chance (percent) that a cure potion neutralises the poison, indexed
    /// by `level - 1`.  Higher-level poisons are harder to cure.
    pub const CURE_CHANCE_PCT: [u32; LEVELS] = [100, 95, 75, 55];

    /// Number of poison charges applied to a weapon when poisoned, indexed
    /// by `level - 1`.  Each successful hit consumes one charge.
    pub const WEAPON_CHARGES: [u16; LEVELS] = [12, 10, 8, 6];

    /// Sound played when poison is applied to a target.
    pub const APPLY_SOUND: u16 = super::sound::POISON;

    /// Clamp a raw level into the valid `1..=LEVELS` range and return the
    /// zero-based index, or `None` if `level == 0`.
    pub fn level_index(level: u8) -> Option<usize> {
        if level == 0 {
            None
        } else {
            Some(((level as usize) - 1).min(LEVELS - 1))
        }
    }
}

// ── Melee combat ──────────────────────────────────────────────────────────

/// Melee combat configuration.
pub mod melee {
    /// Maximum Chebyshev distance for one-handed weapons and fists.
    pub const MELEE_RANGE_1H: u16 = 1;
    /// Maximum Chebyshev distance for two-handed weapons (halberd, staff, etc.).
    pub const MELEE_RANGE_2H: u16 = 2;
    /// Maximum Chebyshev distance before auto-disengaging from target.
    pub const LEASH_RANGE: u16 = 15;
    /// Stamina cost per melee swing.
    pub const STAMINA_COST: u16 = 5;
    /// Fist / unarmed minimum damage.
    pub const FIST_DAMAGE_MIN: u16 = 2;
    /// Fist / unarmed maximum damage.
    pub const FIST_DAMAGE_MAX: u16 = 6;
    /// Fist / unarmed hit sound.
    pub const FIST_SOUND: u16 = super::sound::PUNCH;
    /// Fist / unarmed attack animation.
    pub const FIST_ANIM: u16 = super::anim::SLASH_1H;
    /// Fist / unarmed swing delay (ms between attacks).
    pub const FIST_SWING_DELAY_MS: u64 = 1400;
    /// Rearm delay after completing a spell cast, bandage, or skill use (ms).
    /// The weapon is "put away" during the action and needs this time to be
    /// drawn again before the next melee swing can land.
    pub const ACTION_RECOVERY_DELAY_MS: u64 = 1300;
    /// Delay before playing the miss sound after a swing that misses (ms).
    pub const MISS_SOUND_DELAY_MS: u64 = 1200;
    /// Base miss chance as a percentage (0–100).
    pub const MISS_CHANCE_PCT: u32 = 25;
    /// Weapon swing sound (played at attacker position alongside the
    /// weapon-specific hit sound).
    pub const SWING_SOUND: u16 = super::sound::SWING;
}

/// Weapon definitions — mapping item graphics to combat stats.
pub mod weapon {
    use super::{anim, sound};

    /// Static weapon definition.
    #[derive(Debug, Clone, Copy)]
    #[allow(dead_code)]
    pub struct WeaponDef {
        /// Item graphic ID that identifies this weapon.
        pub graphic: u16,
        /// Human-readable name.
        pub name: &'static str,
        /// Minimum base damage.
        pub damage_min: u16,
        /// Maximum base damage.
        pub damage_max: u16,
        /// Hit sound effect.
        pub hit_sound: u16,
        /// Attack animation action ID.
        pub attack_anim: u16,
        /// `true` if this weapon occupies both hands (Layer::LeftHand).
        pub two_handed: bool,
        /// Time between swings in milliseconds.  Each weapon type has its
        /// own fixed swing cadence (e.g. halberd = 4400 ms, katana = 1750 ms).
        pub swing_delay_ms: u64,
    }

    impl WeaponDef {
        /// Whether this is a *fencing* (piercing) weapon — daggers, kryss,
        /// spears, etc.  Only fencing weapons can be poisoned.
        ///
        /// Determined by the attack animation: piercing weapons use the
        /// `PIERCE_1H` / `PIERCE_2H` thrust animations.
        pub fn is_fencing(&self) -> bool {
            self.attack_anim == anim::PIERCE_1H || self.attack_anim == anim::PIERCE_2H
        }
    }

    /// Look up a weapon definition by equipped item graphic.
    pub fn lookup_weapon(graphic: u16) -> Option<&'static WeaponDef> {
        WEAPONS.iter().find(|w| w.graphic == graphic)
    }

    static WEAPONS: &[WeaponDef] = &[
        // ── One-handed swords ─────────────────────────────────────────────
        WeaponDef {
            graphic: 0x13FE, name: "Katana",
            damage_min: 8, damage_max: 20,
            hit_sound: sound::SWORD_1, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 1750,
        },
        WeaponDef {
            graphic: 0x13FF, name: "Katana", // alternate art
            damage_min: 8, damage_max: 20,
            hit_sound: sound::SWORD_1, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 1750,
        },
        WeaponDef {
            graphic: 0x0F5E, name: "Broadsword",
            damage_min: 10, damage_max: 22,
            hit_sound: sound::SWORD_1, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 2200,
        },
        WeaponDef {
            graphic: 0x0F5F, name: "Broadsword", // alternate art
            damage_min: 10, damage_max: 22,
            hit_sound: sound::SWORD_1, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 2200,
        },
        WeaponDef {
            graphic: 0x1441, name: "Cutlass",
            damage_min: 8, damage_max: 18,
            hit_sound: sound::SWORD_1, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 2000,
        },
        WeaponDef {
            graphic: 0x13B6, name: "Scimitar",
            damage_min: 8, damage_max: 18,
            hit_sound: sound::SWORD_1, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 2000,
        },

        // ── Daggers / piercing ────────────────────────────────────────────
        WeaponDef {
            graphic: 0x0F52, name: "Dagger",
            damage_min: 5, damage_max: 15,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_1H,
            two_handed: false, swing_delay_ms: 1500,
        },
        WeaponDef {
            graphic: 0x0F51, name: "Dagger", // alternate art
            damage_min: 5, damage_max: 15,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_1H,
            two_handed: false, swing_delay_ms: 1500,
        },
        WeaponDef {
            graphic: 0x1401, name: "Kryss",
            damage_min: 6, damage_max: 18,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_1H,
            two_handed: false, swing_delay_ms: 2400,
        },
        WeaponDef {
            graphic: 0x1400, name: "Kryss", // alternate art
            damage_min: 6, damage_max: 18,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_1H,
            two_handed: false, swing_delay_ms: 2400,
        },

        // ── Maces / hammers ───────────────────────────────────────────────
        WeaponDef {
            graphic: 0x0F62, name: "War Hammer",
            damage_min: 12, damage_max: 25,
            hit_sound: sound::MACE_HIT, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 2800,
        },
        WeaponDef {
            graphic: 0x13B4, name: "War Mace",
            damage_min: 10, damage_max: 20,
            hit_sound: sound::MACE_HIT, attack_anim: anim::SLASH_1H,
            two_handed: false, swing_delay_ms: 2400,
        },

        // ── Two-handed polearms ───────────────────────────────────────────
        WeaponDef {
            graphic: 0x143E, name: "Halberd",
            damage_min: 15, damage_max: 30,
            hit_sound: sound::HEAVY_SWORD_4, attack_anim: anim::SWING_2H,
            two_handed: true, swing_delay_ms: 4400,
        },
        WeaponDef {
            graphic: 0x143F, name: "Halberd", // alternate art
            damage_min: 15, damage_max: 30,
            hit_sound: sound::HEAVY_SWORD_4, attack_anim: anim::SWING_2H,
            two_handed: true, swing_delay_ms: 4400,
        },
        WeaponDef {
            graphic: 0x0F4E, name: "Bardiche",
            damage_min: 14, damage_max: 28,
            hit_sound: sound::HEAVY_SWORD_1, attack_anim: anim::SWING_2H,
            two_handed: true, swing_delay_ms: 3600,
        },
        WeaponDef {
            graphic: 0x0F4D, name: "Bardiche", // alternate art
            damage_min: 14, damage_max: 28,
            hit_sound: sound::HEAVY_SWORD_1, attack_anim: anim::SWING_2H,
            two_handed: true, swing_delay_ms: 3600,
        },

        // ── Spears ────────────────────────────────────────────────────────
        WeaponDef {
            graphic: 0x0F62, name: "Spear",
            damage_min: 10, damage_max: 24,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_2H,
            two_handed: true, swing_delay_ms: 3500,
        },
        // Note: 0x0F63 is Spear alternate art, NOT War Hammer alt.
        WeaponDef {
            graphic: 0x0F63, name: "Spear", // alternate art
            damage_min: 10, damage_max: 24,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_2H,
            two_handed: true, swing_delay_ms: 3500,
        },
        WeaponDef {
            graphic: 0x1402, name: "Short Spear",
            damage_min: 8, damage_max: 20,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_1H,
            two_handed: true, swing_delay_ms: 2650,
        },
        WeaponDef {
            graphic: 0x1403, name: "Short Spear", // alternate art
            damage_min: 8, damage_max: 20,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_1H,
            two_handed: true, swing_delay_ms: 2650,
        },
        WeaponDef {
            graphic: 0x0E87, name: "Pitchfork",
            damage_min: 9, damage_max: 22,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_2H,
            two_handed: true, swing_delay_ms: 2600,
        },
        WeaponDef {
            graphic: 0x0E88, name: "Pitchfork", // alternate art
            damage_min: 9, damage_max: 22,
            hit_sound: sound::SWORD_1, attack_anim: anim::PIERCE_2H,
            two_handed: true, swing_delay_ms: 2600,
        },

        // ── Two-handed staves ─────────────────────────────────────────────
        WeaponDef {
            graphic: 0x0DF0, name: "Black Staff",
            damage_min: 8, damage_max: 28,
            hit_sound: sound::MACE_HIT, attack_anim: anim::SLASH_2H,
            two_handed: true, swing_delay_ms: 3200,
        },
        WeaponDef {
            graphic: 0x0DF1, name: "Black Staff", // alternate art
            damage_min: 8, damage_max: 28,
            hit_sound: sound::MACE_HIT, attack_anim: anim::SLASH_2H,
            two_handed: true, swing_delay_ms: 3200,
        },

        // ── Ranged ────────────────────────────────────────────────────────
        WeaponDef {
            graphic: 0x13FC, name: "Heavy Crossbow",
            damage_min: 12, damage_max: 25,
            hit_sound: sound::ARROW_HIT, attack_anim: anim::SHOOT_XBOW,
            two_handed: true, swing_delay_ms: 2000,
        },
        WeaponDef {
            graphic: 0x13FD, name: "Heavy Crossbow", // alternate art
            damage_min: 12, damage_max: 25,
            hit_sound: sound::ARROW_HIT, attack_anim: anim::SHOOT_XBOW,
            two_handed: true, swing_delay_ms: 2000,
        },
        WeaponDef {
            graphic: 0x13B2, name: "Bow",
            damage_min: 9, damage_max: 21,
            hit_sound: sound::ARROW_HIT, attack_anim: anim::SHOOT_BOW,
            two_handed: true, swing_delay_ms: 1800,
        },
        WeaponDef {
            graphic: 0x13B1, name: "Bow", // alternate art
            damage_min: 9, damage_max: 21,
            hit_sound: sound::ARROW_HIT, attack_anim: anim::SHOOT_BOW,
            two_handed: true, swing_delay_ms: 1800,
        },
    ];
}

// ── Weight overrides ──────────────────────────────────────────────────────

/// Item weight system.
///
/// Weights are stored in **tenths of a stone** (`u16`).  For example
/// 0.1 stone = `1`, 0.5 stone = `5`, 1.0 stone = `10`.
///
/// The base weight for each item type comes from `tiledata.mul`
/// ([`StaticTileDef::weight_tenths`](files::tiledata::StaticTileDef::weight_tenths)).  Many items need sub-stone
/// precision that tiledata's `u8` cannot express, so the server
/// overrides them here.
///
/// ## Usage
///
/// Call [`weight::item_weight_tenths`] to get the authoritative weight
/// for a given graphic.  It checks the override table first, then falls
/// back to tiledata (via [`StaticDataProvider`](framework::ecumene::StaticDataProvider)) if available, and
/// finally returns the built-in default (10 = 1.0 stone).
pub mod weight {
    use framework::ecumene::StaticDataProvider;

    /// Look up the weight of one unit of an item by its graphic ID.
    ///
    /// Returns weight in 1/10ths of a stone.
    ///
    /// Priority:
    /// 1. Server override table ([`WEIGHT_OVERRIDES`]).
    /// 2. `tiledata.mul` via `StaticDataProvider` (converted to tenths).
    /// 3. Built-in fallback: 10 (= 1.0 stone).
    pub fn item_weight_tenths(
        graphic: u16,
        static_data: Option<&dyn StaticDataProvider>,
    ) -> u16 {
        // 1. Check override table.
        if let Some(&wt) = lookup_override(graphic) {
            return wt;
        }

        // 2. Fall back to tiledata.
        if let Some(sd) = static_data {
            if let Some(def) = sd.static_tile_def(graphic) {
                return def.weight_tenths();
            }
        }

        // 3. Built-in default: 1.0 stone.
        10
    }

    /// Compute total weight of a stack: `count × unit_weight`, in tenths.
    ///
    /// To convert to whole stones: `stack_weight_tenths(...) / 10`.
    pub fn stack_weight_tenths(
        graphic: u16,
        amount: u16,
        static_data: Option<&dyn StaticDataProvider>,
    ) -> u32 {
        item_weight_tenths(graphic, static_data) as u32 * amount as u32
    }

    /// Convert tenths to whole stones (rounded down).
    pub fn tenths_to_stones(tenths: u32) -> u16 {
        (tenths / 10) as u16
    }

    /// Maximum carry weight for a mobile based on STR.
    ///
    /// UO formula: `STR × 3.5 + 40` = `(STR × 7 + 80) / 2` stones.
    /// Returned in whole stones.
    pub fn max_carry_weight(str_: u16) -> u16 {
        ((str_ as u32 * 7 + 80) / 2) as u16
    }

    // ── Override table ────────────────────────────────────────────────

    fn lookup_override(graphic: u16) -> Option<&'static u16> {
        WEIGHT_OVERRIDES.iter()
            .find(|(g, _)| *g == graphic)
            .map(|(_, w)| w)
    }

    /// Server-side weight overrides for items whose `tiledata.mul` weight
    /// is too coarse (stored as 1/10ths of a stone).
    ///
    /// Values based on real UO server data
    static WEIGHT_OVERRIDES: &[(u16, u16)] = &[
        // ── Gold ──────────────────────────────────────────────────────
        // Gold pieces are weightless in the traditional era.
        (0x0EED, 0),    // gold coin: 0.0 stones

        // ── Reagents (0.1 stone each) ────────────────────────────────
        (0x0F7A, 1),    // black pearl
        (0x0F7B, 1),    // blood moss
        (0x0F84, 1),    // garlic
        (0x0F85, 1),    // ginseng
        (0x0F86, 1),    // mandrake root
        (0x0F88, 1),    // nightshade
        (0x0F8C, 1),    // sulphurous ash
        (0x0F8D, 1),    // spider's silk

        // ── Arrows & bolts ───────────────────────────────────────────
        (0x0F3F, 1),    // arrow: 0.1 stones
        (0x1BFB, 1),    // bolt:  0.1 stones

        // ── Fletching materials ──────────────────────────────────────
        (0x1BD1, 1),    // feather: 0.1 stones
        (0x1BD4, 5),    // shaft:   0.5 stones

        // ── Scrolls (1st–4th circle: 0.5 stone) ─────────────────────
        (0x1F2D, 5),    // Reactive Armor scroll
        (0x1F2E, 5),    // Clumsy scroll
        (0x1F2F, 5),    // Create Food scroll
        (0x1F30, 5),    // Feeblemind scroll
        (0x1F31, 5),    // Heal scroll
        (0x1F32, 5),    // Magic Arrow scroll
        (0x1F33, 5),    // Night Sight scroll
        (0x1F34, 5),    // Weaken scroll
        (0x1F3D, 5),    // Bless scroll
        (0x1F47, 5),    // Curse scroll

        // ── Bandages ─────────────────────────────────────────────────
        (0x0E21, 1),    // bandage: 0.1 stones

        // ── Potions (1.0 stone each) ─────────────────────────────────
        (0x0F0B, 10),   // greater heal potion
        (0x0F0C, 10),   // heal potion
        (0x0F09, 10),   // strength potion
        (0x0F07, 10),   // lesser cure potion
        (0x0F06, 10),   // agility potion
    ];
}

// ── Regen ─────────────────────────────────────────────────────────────────

/// Stat regeneration configuration.
pub mod regen {
    /// Interval between regen ticks (milliseconds).
    pub const TICK_INTERVAL_MS: u64 = 2000;

    /// Base HP regen per tick.
    pub const HP_PER_TICK: u16      = 1;
    /// Base stamina regen per tick.
    pub const STAM_PER_TICK: u16    = 2;
    /// Base mana regen per tick (without meditation).
    pub const MANA_PER_TICK: u16    = 1;
    /// Bonus mana regen per tick while meditating.
    pub const MANA_MEDITATION_BONUS: u16 = 3;
}

// ── Mounts ────────────────────────────────────────────────────────────────

/// Mount body ↔ mount-item graphic mapping.
///
/// In UO, mounting a creature removes the NPC from the world and equips a
/// synthetic item on `Layer::Mount` whose graphic determines the rider's
/// visual.  Dismounting reverses the process.
///
/// The mount-item serial is allocated via `SerialAllocator` (not derived
/// from the NPC serial).  The original NPC serial is stored in
/// `ItemProps.meta` so it can be recovered on dismount.
#[allow(dead_code)]
pub mod mount {
    /// A single entry in the mount table.
    #[derive(Debug, Clone, Copy)]
    pub struct MountDef {
        /// NPC body graphic (the animal walking around in the world).
        pub body: u16,
        /// Equipped mount-item graphic (what the rider "wears" on Layer::Mount).
        pub mount_graphic: u16,
        /// Human-readable name.
        pub name: &'static str,
    }

    /// Standard UO mountable creatures.
    static MOUNTS: &[MountDef] = &[
        // ── Horses ────────────────────────────────────────────────────────
        MountDef { body: 0x00C8, mount_graphic: 0x3EA0, name: "Horse" },
        MountDef { body: 0x00E2, mount_graphic: 0x3EA1, name: "Horse" },
        MountDef { body: 0x00E4, mount_graphic: 0x3EA2, name: "Horse" },
        MountDef { body: 0x00CC, mount_graphic: 0x3EA3, name: "Horse" },

        // ── Ostards ───────────────────────────────────────────────────────
        MountDef { body: 0x00D2, mount_graphic: 0x3EA4, name: "Desert Ostard" },
        MountDef { body: 0x00DA, mount_graphic: 0x3EA5, name: "Frenzied Ostard" },
        MountDef { body: 0x00DB, mount_graphic: 0x3EA6, name: "Forest Ostard" },

        // ── Llama ─────────────────────────────────────────────────────────
        MountDef { body: 0x00DC, mount_graphic: 0x3EA7, name: "Llama" },

        // ── Nightmare / Ethereal ──────────────────────────────────────────
        MountDef { body: 0x0074, mount_graphic: 0x3EA8, name: "Nightmare" },
        MountDef { body: 0x0075, mount_graphic: 0x3EA9, name: "Nightmare" },
        MountDef { body: 0x0072, mount_graphic: 0x3EAA, name: "Silver Steed" },
        MountDef { body: 0x0073, mount_graphic: 0x3EAB, name: "Silver Steed" },

        // ── Ridgebacks ────────────────────────────────────────────────────
        MountDef { body: 0x00BB, mount_graphic: 0x3EAC, name: "Ridgeback" },
        MountDef { body: 0x0319, mount_graphic: 0x3EAD, name: "Savage Ridgeback" },

        // ── Skeletal / Swamp Dragon ───────────────────────────────────────
        MountDef { body: 0x0317, mount_graphic: 0x3EAF, name: "Skeletal Mount" },
        MountDef { body: 0x031A, mount_graphic: 0x3EB0, name: "Swamp Dragon" },
        MountDef { body: 0x031F, mount_graphic: 0x3EB4, name: "Armored Swamp Dragon" },

        // ── Beetles ───────────────────────────────────────────────────────
        MountDef { body: 0x0317, mount_graphic: 0x3EBC, name: "Giant Beetle" },

        // ── Sea Horse ─────────────────────────────────────────────────────
        MountDef { body: 0x0090, mount_graphic: 0x3EB8, name: "Sea Horse" },

        // ── Ki-Rin / Unicorn ──────────────────────────────────────────────
        MountDef { body: 0x0084, mount_graphic: 0x3E9F, name: "Ki-Rin" },
        MountDef { body: 0x007A, mount_graphic: 0x3EB4, name: "Unicorn" },
    ];

    /// Look up a mount definition by NPC body graphic.
    ///
    /// Returns `None` if the body is not a mountable creature.
    pub fn body_to_mount(body: u16) -> Option<&'static MountDef> {
        MOUNTS.iter().find(|m| m.body == body)
    }

    /// Look up a mount definition by equipped mount-item graphic.
    ///
    /// Returns `None` if the graphic is not a known mount-item graphic.
    /// Used during dismount to reconstruct the NPC body.
    pub fn mount_graphic_to_mount(mount_graphic: u16) -> Option<&'static MountDef> {
        MOUNTS.iter().find(|m| m.mount_graphic == mount_graphic)
    }

    /// Maximum Chebyshev distance to mount a creature.
    pub const MOUNT_RANGE: u16 = 2;
}

// ── Armor system ──────────────────────────────────────────────────────────

/// Armor definitions, hit zones, and lookup helpers.
///
/// Each armor piece is identified by its `(graphic, color)` pair in a static
/// template table.  Items created by the server can also store a per-instance
/// `armor_rating` in `ItemProps.meta` which takes priority over the template.
///
/// ## Hit zones
///
/// Melee damage targets a random body zone with classic UO probabilities.
/// If the zone is covered by an armor piece, its AR is subtracted from the
/// raw damage (minimum 1).  If the zone is uncovered, the full damage
/// passes through.  Magical damage ignores physical armor entirely.
pub mod armor {
    use packets::layer::Layer;

    // ── Armor tier ───────────────────────────────────────────────────

    /// Broad class of armor (cosmetic grouping + AR range hint).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ArmorTier {
        Leather,
        Chain,
        Plate,
        Shield,
    }

    // ── Armor template ───────────────────────────────────────────────

    /// Static definition for one armor variant (graphic + color → stats).
    ///
    /// Multiple templates can share the same `graphic` but differ in
    /// `color`, `name`, and `armor_rating` (e.g. Iron Plate vs Steel Plate).
    #[derive(Debug, Clone, Copy)]
    pub struct ArmorTemplate {
        /// Item graphic ID (the sprite the client renders).
        pub graphic: u16,
        /// Equipment layer (Helmet, Torso, Arms, Legs, Gloves, LeftHand …).
        pub layer: Layer,
        /// Hue / color that distinguishes this variant.
        pub color: u16,
        /// Human-readable name (e.g. "Steel Plate Chest").
        pub name: &'static str,
        /// Armor rating for this piece.
        pub armor_rating: u16,
        /// Broad armor class.
        pub tier: ArmorTier,
    }

    // ── Lookup ───────────────────────────────────────────────────────

    /// Find a template by exact `(graphic, color)` match.
    pub fn lookup_template(graphic: u16, color: u16) -> Option<&'static ArmorTemplate> {
        ARMOR_TEMPLATES.iter().find(|t| t.graphic == graphic && t.color == color)
    }

    /// Find a template by `graphic` only (ignoring color).
    ///
    /// Returns the first match — useful as a fallback for replay entities
    /// whose color may not match any specific tier.
    pub fn lookup_template_by_graphic(graphic: u16) -> Option<&'static ArmorTemplate> {
        ARMOR_TEMPLATES.iter().find(|t| t.graphic == graphic)
    }

    /// Check whether a graphic is a known shield.
    pub fn is_shield(graphic: u16) -> bool {
        ARMOR_TEMPLATES.iter().any(|t| t.graphic == graphic && t.tier == ArmorTier::Shield)
    }

    // ── Template table ───────────────────────────────────────────────
    //
    // Graphics sourced from `scripts/scene/outfit.lua` and standard UO
    // tiledata.  Colors are approximate UO hues.

    pub static ARMOR_TEMPLATES: &[ArmorTemplate] = &[
        // ── Plate armor ──────────────────────────────────────────────

        // Iron Plate (default hue — no dye)
        ArmorTemplate { graphic: 0x1415, layer: Layer::Torso,  color: 0x0000, name: "Iron Plate Chest",   armor_rating: 20, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1410, layer: Layer::Arms,   color: 0x0000, name: "Iron Plate Arms",    armor_rating: 16, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1414, layer: Layer::Gloves, color: 0x0000, name: "Iron Plate Gloves",  armor_rating: 12, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1411, layer: Layer::Legs,   color: 0x0000, name: "Iron Plate Legs",    armor_rating: 16, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1412, layer: Layer::Helmet, color: 0x0000, name: "Iron Close Helmet",  armor_rating: 15, tier: ArmorTier::Plate },

        // Steel Plate (dark grey hue)
        ArmorTemplate { graphic: 0x1415, layer: Layer::Torso,  color: 0x0835, name: "Steel Plate Chest",  armor_rating: 28, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1410, layer: Layer::Arms,   color: 0x0835, name: "Steel Plate Arms",   armor_rating: 22, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1414, layer: Layer::Gloves, color: 0x0835, name: "Steel Plate Gloves", armor_rating: 17, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1411, layer: Layer::Legs,   color: 0x0835, name: "Steel Plate Legs",   armor_rating: 22, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1412, layer: Layer::Helmet, color: 0x0835, name: "Steel Close Helmet", armor_rating: 20, tier: ArmorTier::Plate },

        // Golden Plate (gold hue)
        ArmorTemplate { graphic: 0x1415, layer: Layer::Torso,  color: 0x0501, name: "Golden Plate Chest",  armor_rating: 35, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1410, layer: Layer::Arms,   color: 0x0501, name: "Golden Plate Arms",   armor_rating: 28, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1414, layer: Layer::Gloves, color: 0x0501, name: "Golden Plate Gloves", armor_rating: 22, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1411, layer: Layer::Legs,   color: 0x0501, name: "Golden Plate Legs",   armor_rating: 28, tier: ArmorTier::Plate },
        ArmorTemplate { graphic: 0x1412, layer: Layer::Helmet, color: 0x0501, name: "Golden Close Helmet", armor_rating: 25, tier: ArmorTier::Plate },

        // ── Chainmail armor ──────────────────────────────────────────

        // Iron Chain (default hue)
        ArmorTemplate { graphic: 0x13BF, layer: Layer::Tunic, color: 0x0000, name: "Iron Chainmail Tunic",    armor_rating: 14, tier: ArmorTier::Chain },
        ArmorTemplate { graphic: 0x13BE, layer: Layer::Legs,  color: 0x0000, name: "Iron Chainmail Leggings", armor_rating: 12, tier: ArmorTier::Chain },

        // Steel Chain (dark grey hue)
        ArmorTemplate { graphic: 0x13BF, layer: Layer::Tunic, color: 0x0835, name: "Steel Chainmail Tunic",    armor_rating: 20, tier: ArmorTier::Chain },
        ArmorTemplate { graphic: 0x13BE, layer: Layer::Legs,  color: 0x0835, name: "Steel Chainmail Leggings", armor_rating: 17, tier: ArmorTier::Chain },

        // ── Leather armor ────────────────────────────────────────────

        // Leather (default hue — brown)
        ArmorTemplate { graphic: 0x13CC, layer: Layer::Torso,  color: 0x0000, name: "Leather Chest",  armor_rating: 8,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x13C6, layer: Layer::Gloves, color: 0x0000, name: "Leather Gloves", armor_rating: 5,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x13CB, layer: Layer::Arms,   color: 0x0000, name: "Leather Arms",   armor_rating: 6,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x13CD, layer: Layer::Legs,   color: 0x0000, name: "Leather Legs",   armor_rating: 6,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x1DB9, layer: Layer::Helmet, color: 0x0000, name: "Leather Cap",    armor_rating: 5,  tier: ArmorTier::Leather },

        // Hardened Leather (dark brown hue)
        ArmorTemplate { graphic: 0x13CC, layer: Layer::Torso,  color: 0x0451, name: "Hardened Leather Chest",  armor_rating: 12, tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x13C6, layer: Layer::Gloves, color: 0x0451, name: "Hardened Leather Gloves", armor_rating: 8,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x13CB, layer: Layer::Arms,   color: 0x0451, name: "Hardened Leather Arms",   armor_rating: 9,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x13CD, layer: Layer::Legs,   color: 0x0451, name: "Hardened Leather Legs",   armor_rating: 9,  tier: ArmorTier::Leather },
        ArmorTemplate { graphic: 0x1DB9, layer: Layer::Helmet, color: 0x0451, name: "Hardened Leather Cap",    armor_rating: 8,  tier: ArmorTier::Leather },

        // ── Shields ──────────────────────────────────────────────────

        // Wooden Shield
        ArmorTemplate { graphic: 0x1B7A, layer: Layer::LeftHand, color: 0x0000, name: "Wooden Shield",   armor_rating: 8,  tier: ArmorTier::Shield },
        // Metal Shield
        ArmorTemplate { graphic: 0x1B7B, layer: Layer::LeftHand, color: 0x0000, name: "Metal Shield",    armor_rating: 14, tier: ArmorTier::Shield },
        // Buckler
        ArmorTemplate { graphic: 0x1B73, layer: Layer::LeftHand, color: 0x0000, name: "Buckler",         armor_rating: 10, tier: ArmorTier::Shield },
        // Heater Shield
        ArmorTemplate { graphic: 0x1B76, layer: Layer::LeftHand, color: 0x0000, name: "Heater Shield",   armor_rating: 18, tier: ArmorTier::Shield },
        // Metal Kite Shield
        ArmorTemplate { graphic: 0x1B74, layer: Layer::LeftHand, color: 0x0000, name: "Metal Kite Shield", armor_rating: 16, tier: ArmorTier::Shield },
    ];

    // ── Hit zones ────────────────────────────────────────────────────

    /// Body zone targeted by a melee attack.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HitZone {
        /// 10% — protected by Helmet layer
        Head,
        /// 5% — protected by Necklace layer (gorget)
        Neck,
        /// 35% — protected by Torso / Tunic layer
        Chest,
        /// 20% — protected by Arms / Gloves layer
        Arms,
        /// 20% — protected by Legs / Pants layer
        Legs,
        /// 10% — protected by shield in LeftHand (if present)
        Shield,
    }

    impl HitZone {
        /// Roll a random hit zone using classic UO probabilities.
        ///
        /// Head 10%, Neck 5%, Chest 35%, Arms 20%, Legs 20%, Shield 10%.
        pub fn roll() -> Self {
            use rand::Rng;
            let roll: u32 = rand::rng().random_range(0..100);
            match roll {
                0..10   => HitZone::Head,
                10..15  => HitZone::Neck,
                15..50  => HitZone::Chest,
                50..70  => HitZone::Arms,
                70..90  => HitZone::Legs,
                _       => HitZone::Shield,
            }
        }

        /// Get the AR for this zone from an `ArmorProfile`.
        pub fn zone_ar(&self, profile: &ArmorProfile) -> u16 {
            match self {
                HitZone::Head   => profile.head,
                HitZone::Neck   => profile.neck,
                HitZone::Chest  => profile.chest,
                HitZone::Arms   => profile.arms,
                HitZone::Legs   => profile.legs,
                HitZone::Shield => profile.shield,
            }
        }

        /// Equipment layers that protect this zone.
        ///
        /// Returns one or two layers that should be checked for armor in
        /// this zone.  The first match wins (highest-priority layer first).
        pub fn protection_layers(&self) -> &'static [Layer] {
            match self {
                HitZone::Head   => &[Layer::Helmet],
                HitZone::Neck   => &[Layer::Necklace],
                HitZone::Chest  => &[Layer::Torso, Layer::Tunic],
                HitZone::Arms   => &[Layer::Arms, Layer::Gloves],
                HitZone::Legs   => &[Layer::Legs, Layer::Pants],
                HitZone::Shield => &[Layer::LeftHand],
            }
        }
    }

    // ── Armor profile ────────────────────────────────────────────────

    // `ArmorProfile` is defined in `common::uo_engine::handler::ArmorProfile`
    // so it can be used as the reply type for `EngineCommand::QueryEquipmentArmor`.
    // Re-exported here for convenience.
    pub use common::uo_engine::handler::ArmorProfile;
}

// ── Item display names ──────────────────────────────────────────────────────

/// Fallback display names for item graphics that the demo server itself
/// creates (loot, crafting, spawns, vendor stock, …).
///
/// This table is the **third** tier in the name-resolution chain used by
/// `SingleClick`:
///
/// 1. `ItemProps::name` — an explicit per-instance name (crafted/loot/quest).
/// 2. `tiledata.mul` — the authentic UO tile name (only when `--data` is loaded).
/// 3. This table — a hand-curated name keyed by graphic, so common items
///    have sensible names even without UO data files.
/// 4. `[item 0x{graphic:04X}]` — the last-resort hex fallback.
///
/// Names of **stackable** graphics (gold, arrows, ingots, bones, ore,
/// reagents) are written in a form that reads naturally with a leading
/// count, e.g. `"gold coins"` → `"1543 gold coins"`.  Single (non-stackable)
/// items use the singular article form (`"a ruby"`).
pub mod item_names {
    use super::item;

    /// Reagent / consumable graphics referenced by `loot.rs` but not present
    /// in the `item` module.  Kept in sync with `loot.rs`.
    const ARROWS: u16 = 0x0F3F;
    const GEM_STAR_SAPPHIRE: u16 = 0x0F0F;
    const GEM_EMERALD: u16 = 0x0F10;
    const GEM_RUBY: u16 = 0x0F13;
    const GEM_DIAMOND: u16 = 0x0F26;
    const BONE: u16 = 0x0F7E;
    const TATTERED_MAP: u16 = 0x14ED;

    /// Resolve a hand-curated display name for an item graphic.
    ///
    /// Returns `None` when the graphic is not in the table, letting the
    /// caller fall back to the hex form.
    pub fn name_for_graphic(graphic: u16) -> Option<&'static str> {
        Some(match graphic {
            // Currency / consumables (stackable → plural-friendly form).
            item::GOLD => "gold coins",
            ARROWS => "arrows",
            item::IRON_ORE => "iron ore",
            item::IRON_INGOT => "iron ingots",
            BONE => "bones",
            item::BANDAGE => "bandages",

            // Reagents (stackable).
            item::REAGENT_BLACK_PEARL => "black pearl",
            item::REAGENT_BLOOD_MOSS => "blood moss",
            item::REAGENT_GARLIC => "garlic",
            item::REAGENT_GINSENG => "ginseng",
            item::REAGENT_MANDRAKE_ROOT => "mandrake root",
            item::REAGENT_NIGHTSHADE => "nightshade",
            item::REAGENT_SULPHUROUS_ASH => "sulphurous ash",
            item::REAGENT_SPIDERS_SILK => "spiders' silk",

            // Gems (stackable).
            GEM_STAR_SAPPHIRE => "star sapphires",
            GEM_EMERALD => "emeralds",
            GEM_RUBY => "rubies",
            GEM_DIAMOND => "diamonds",

            // Potions (single).
            item::POTION_HEAL => "a heal potion",
            item::POTION_GREATER_HEAL => "a greater heal potion",
            item::POTION_MANA => "a mana potion",
            item::POTION_CURE => "a cure potion",
            item::POTION_POISON => "a poison potion",

            // Books / scrolls / runes (single).
            item::SPELLBOOK => "a spellbook",
            item::RUNE => "a recall rune",
            item::SCROLL_HEAL => "a heal scroll",
            item::SCROLL_MAGIC_ARROW => "a magic arrow scroll",
            item::SCROLL_BLESS => "a bless scroll",
            item::SCROLL_CURSE => "a curse scroll",
            item::SCROLL_GREATER_HEAL => "a greater heal scroll",
            item::SCROLL_LIGHTNING => "a lightning scroll",
            item::SCROLL_ENERGY_BOLT => "an energy bolt scroll",
            item::SCROLL_FLAMESTRIKE => "a flamestrike scroll",

            // Tools / world objects (single).
            item::SMITH_HAMMER | item::SMITH_HAMMER_ALT => "a smith's hammer",
            item::ANVIL => "an anvil",
            item::FORGE => "a forge",

            // Maps / misc.
            TATTERED_MAP => "a tattered treasure map",
            item::SHRINK_STATUE => "a figurine",

            _ => return None,
        })
    }
}
