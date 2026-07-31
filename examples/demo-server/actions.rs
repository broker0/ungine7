//! Action system — timed player actions with independent slot-based blocking.
//!
//! The demo server uses **three independent action slots**, each with its own
//! timer, mirroring the original UO behaviour:
//!
//! | Slot          | Blocks starting…                          | Interrupted by damage? |
//! |---------------|-------------------------------------------|------------------------|
//! | `SpellCast`   | another cast                              | **Yes**                |
//! | `SkillUse`    | another skill, and spell cast              | No                     |
//! | `Bandage`     | another bandage, and spell cast            | No                     |
//!
//! Additionally, a **pending target cursor** (from any slot) blocks starting
//! any new action — the player must answer or cancel the cursor first.
//!
//! ## Slot architecture
//!
//! Each slot is stored as an `Option<ActiveAction>` with a dedicated
//! `tokio::time::Sleep` timer in the session loop.  The three check functions
//! ([`can_begin_cast`], [`can_begin_skill`], [`can_begin_bandage`]) encode
//! the blocking rules and should be called before creating an `ActiveAction`.
//!
//! ## Extending
//!
//! To add a new action type:
//! 1. Add a variant to [`ActionPayload`].
//! 2. Decide which slot it uses (or add a new slot).
//! 3. Update the corresponding `can_begin_*` function.
//! 4. Handle the completion in the session loop's timer branch.

use std::time::Duration;
use tokio::time::Instant;

use crate::magic::SpellDef;

// ── ActionKind ────────────────────────────────────────────────────────────

/// The category of action — determines which slot it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// Spell casting (channeled, interrupted by damage).
    SpellCast,
    /// Bandage healing (timed, NOT interrupted by damage, only by death).
    Bandage,
    /// Skill use (Arms Lore, etc.) — timed.
    SkillUse,
    // Future: Potion, CraftItem, Meditation, …
}

// ── Slot-based blocking checks ────────────────────────────────────────────

/// Check whether a new spell cast can begin.
///
/// Blocked by:
/// - active cast (already casting)
/// - active skill use (weapon/hands busy)
/// - active bandage (hands busy with healing)
/// - any pending target cursor (must answer/cancel first)
/// - blocking gump open (must close/answer first)
pub fn can_begin_cast(
    active_cast: &Option<ActiveAction>,
    active_skill: &Option<ActiveAction>,
    active_bandage: &Option<ActiveAction>,
    has_pending: bool,
    has_blocking_gump: bool,
) -> Result<(), &'static str> {
    if has_pending {
        return Err("You are already doing something.");
    }
    if has_blocking_gump {
        return Err("You are busy.");
    }
    if active_cast.is_some() {
        return Err("You are already casting a spell.");
    }
    if active_skill.is_some() {
        return Err("You are already doing something.");
    }
    if active_bandage.is_some() {
        return Err("You cannot cast spells while healing.");
    }
    Ok(())
}

/// Check whether a new skill use can begin.
///
/// Blocked by:
/// - active skill (already using a skill)
/// - any pending target cursor
/// - blocking gump open (must close/answer first)
///
/// **Not** blocked by active cast — skill and cast run in parallel.
/// **Not** blocked by active bandage.
pub fn can_begin_skill(
    active_skill: &Option<ActiveAction>,
    has_pending: bool,
    has_blocking_gump: bool,
) -> Result<(), &'static str> {
    if has_pending {
        return Err("You are already doing something.");
    }
    if has_blocking_gump {
        return Err("You are busy.");
    }
    if active_skill.is_some() {
        return Err("You are already doing something.");
    }
    Ok(())
}

/// Check whether a new bandage use can begin.
///
/// Blocked by:
/// - active bandage (already applying bandages)
/// - any pending target cursor (cursor is shared; must answer/cancel first)
///
/// **Not** blocked by active cast, active skill, or blocking gumps.
pub fn can_begin_bandage(
    active_bandage: &Option<ActiveAction>,
    has_pending: bool,
) -> Result<(), &'static str> {
    if has_pending {
        return Err("You are already doing something.");
    }
    if active_bandage.is_some() {
        return Err("You are already applying bandages.");
    }
    Ok(())
}

// ── ActionPayload ─────────────────────────────────────────────────────────

/// Type-specific data carried by the action until completion.
pub enum ActionPayload {
    SpellCast {
        spell: &'static SpellDef,
        caster_serial: u32,
        target_serial: u32,
        world: u8,
        /// If `Some`, the cast originated from a scroll and the scroll
        /// should be consumed on successful completion.
        scroll_item_serial: Option<u32>,
    },
    Bandage {
        healer_serial: u32,
        target_serial: u32,
        bandage_item_serial: u32,
        world: u8,
    },
    /// Skill use (e.g. Arms Lore) — carries skill ID and target info.
    SkillUse {
        skill_id: u16,
        user_serial: u32,
        target_serial: u32,
        world: u8,
    },
    /// Poisoning skill — coats a fencing weapon with poison after a delay.
    Poisoning {
        user_serial: u32,
        /// The weapon being poisoned.
        weapon_serial: u32,
        /// The poison potion bottle (consumed on success).
        potion_serial: u32,
        /// Poison level (`1..=4`).
        level: u8,
        world: u8,
    },
    /// Resource gathering (e.g. mining) — produces a resource into the
    /// player's backpack after a delay, with a success roll on completion.
    Gather {
        user_serial: u32,
        /// The gathering tool's item graphic (looks up the [`crate::gathering::ToolDef`]).
        tool_graphic: u16,
        /// Targeted source location (tile or item-node position).
        target_x: u16,
        target_y: u16,
        target_z: i8,
        /// The targeted source's tile/item graphic (re-validated server-side).
        source_graphic: u16,
        /// Serial of the targeted resource-node item entity, or `0` when the
        /// source is a static map tile.
        source_serial: u32,
        world: u8,
    },
    /// Smelting — melt an ore stack into ingots at a forge after a delay.
    Smelt {
        user_serial: u32,
        /// The ore item being smelted (the whole stack is consumed).
        ore_serial: u32,
        world: u8,
    },
    /// Blacksmithing — forge a weapon/armor piece from ingots at an anvil.
    Craft {
        user_serial: u32,
        /// The recipe key (looks up the [`crate::crafting::RecipeDef`]).
        recipe_key: &'static str,
        world: u8,
    },
    /// Treasure digging — after a delay, spawn a chest + guardians at the
    /// buried-treasure tile and consume the digging tool + map.
    TreasureDig {
        user_serial: u32,
        /// The digging tool item (consumed on success).
        tool_serial: u32,
        /// The treasure map item (consumed on success).
        map_serial: u32,
        /// Treasure level (looks up the [`crate::treasure_map::TreasureLevel`]).
        level: u8,
        /// The tile the player chose to dig.
        target_x: u16,
        target_y: u16,
        target_z: i8,
        world: u8,
    },
    /// Wall of Stone — after the cast delay, spawn a row of stone blocks at
    /// the chosen ground tile and schedule them to decay one-by-one.
    WallOfStone {
        caster_serial: u32,
        /// The center tile the player chose.
        target_x: u16,
        target_y: u16,
        target_z: i8,
        world: u8,
    },
}

// ── ActiveAction ──────────────────────────────────────────────────────────

/// A timed action currently in progress.
pub struct ActiveAction {
    #[allow(dead_code)]
    pub kind: ActionKind,
    pub completes_at: Instant,
    pub payload: ActionPayload,
}

impl ActiveAction {
    /// Create a new active action that completes after `delay`.
    pub fn new(kind: ActionKind, delay: Duration, payload: ActionPayload) -> Self {
        Self {
            kind,
            completes_at: Instant::now() + delay,
            payload,
        }
    }
}
