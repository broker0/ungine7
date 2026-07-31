//! Animal taming — tameable creature table and pet ownership/command meta.
//!
//! Data-driven taming (mirrors `gathering` / `weapon` / `potions` tables).
//! A player uses the **Animal Taming** skill, targets a wild creature, and on
//! a successful roll the creature becomes a pet: its ownership and command
//! state are recorded in [`ItemProps`](common::uo_engine::item_props::ItemProps)
//! `meta` keyed by the creature's mobile serial, and a [`crate::controller_registry::PetController`]
//! is attached so it follows/stays on command.
//!
//! Pet command/ownership is stored as meta (not on `MobileData`), so it
//! survives snapshot save/load and requires no engine type changes.
//!
//! ## Extending
//!
//! Add a [`TameableDef`] entry to [`TAMEABLES`] keyed by body graphic.

#![allow(dead_code)]

use crate::game_util;

// ── Meta keys ────────────────────────────────────────────────────────────

/// `ItemProps` meta key: serial of the pet's owner (a player).
pub const META_PET_OWNER: &str = "pet_owner";
/// `ItemProps` meta key: current pet command — [`CMD_FOLLOW`] or [`CMD_STAY`].
pub const META_PET_COMMAND: &str = "pet_command";

/// Pet command value: follow the owner around.
pub const CMD_FOLLOW: &str = "follow";
/// Pet command value: stand still where it is.
pub const CMD_STAY: &str = "stay";

/// Persistent controller ID used for tamed pets (see
/// [`crate::controller_registry::create_controller`]).
pub const PET_CONTROLLER_ID: &str = "pet";

// ── Tuning ──────────────────────────────────────────────────────────────────

/// Time the taming attempt takes before the success roll, milliseconds.
pub const TAME_DELAY_MS: u64 = 3000;

/// Maximum Chebyshev distance (tiles) to attempt taming.
pub const TAME_RANGE: u16 = 4;

/// Follow behaviour: pet steps toward the owner on this interval, ms.
pub const FOLLOW_INTERVAL_MS: u64 = 600;

/// Follow behaviour: pet stops stepping once within this many tiles of owner.
pub const FOLLOW_DISTANCE: i32 = 2;

// ── TameableDef ──────────────────────────────────────────────────────────────

/// A tameable creature definition, keyed by body graphic.
#[derive(Debug, Clone, Copy)]
pub struct TameableDef {
    /// Mobile body graphic of the creature.
    pub body_graphic: u16,
    /// Human-readable creature name.
    pub name: &'static str,
    /// Taming success chance, percent `0..=100`.
    pub tame_chance: u8,
}

// ── Table ─────────────────────────────────────────────────────────────────────

/// All tameable creatures, keyed by body graphic.
///
/// Body graphics use the classic UO art numbering.
pub static TAMEABLES: &[TameableDef] = &[
    TameableDef { body_graphic: 0x00C8, name: "a horse",     tame_chance: 90 }, // horse
    TameableDef { body_graphic: 0x00E2, name: "a horse",     tame_chance: 90 }, // horse (variant)
    TameableDef { body_graphic: 0x00CC, name: "a horse",     tame_chance: 90 }, // horse (variant)
    TameableDef { body_graphic: 0x00E4, name: "a horse",     tame_chance: 90 }, // horse (variant)
    TameableDef { body_graphic: 0x00D0, name: "a llama",     tame_chance: 80 }, // llama
    TameableDef { body_graphic: 0x00D1, name: "a wolf",      tame_chance: 50 }, // grey wolf
    TameableDef { body_graphic: 0x00E7, name: "a hart",      tame_chance: 70 }, // hart
    TameableDef { body_graphic: 0x00ED, name: "a rabbit",    tame_chance: 95 }, // rabbit
    TameableDef { body_graphic: 0x00DC, name: "a hind",      tame_chance: 75 }, // hind
    TameableDef { body_graphic: 0x001D, name: "a gorilla",   tame_chance: 60 }, // gorilla
];

// ── Lookups ───────────────────────────────────────────────────────────────────

/// Look up a tameable creature by its body graphic.
pub fn lookup_tameable(body_graphic: u16) -> Option<&'static TameableDef> {
    TAMEABLES.iter().find(|t| t.body_graphic == body_graphic)
}

impl TameableDef {
    /// Roll the taming success chance for this creature.
    pub fn roll_tame(&self) -> bool {
        game_util::random_range(1, 100) as u8 <= self.tame_chance
    }
}
