//! Unified pending target-cursor state.
//!
//! Replaces the four separate `Option<Pending*>` fields (`pending_spell`,
//! `pending_skill`, `pending_bandage`, `pending_target`) with a single
//! `Option<PendingCursor>` that carries a `CursorKind` discriminant.
//!
//! Dispatch on `TargetCursor` (0x6C) response is now a single `match`
//! on `CursorKind` instead of a chain of try/restore calls.

// Some variants/methods are only used under `rust-session`, not `lua-session`.
#![allow(dead_code)]

use crate::magic::{PendingSpell, SpellDef};

use super::dot_commands::PendingTarget;

// ── CursorKind ───────────────────────────────────────────────────────────

/// What kind of target cursor is pending.
#[derive(Debug)]
pub(super) enum CursorKind {
    /// Spell targeting (from spellbook cast or TextCommand).
    Spell {
        spell: &'static SpellDef,
        caster_serial: u32,
        scroll_item_serial: Option<u32>,
    },
    /// Skill targeting (Arms Lore, etc.).
    Skill {
        skill_id: u16,
        user_serial: u32,
    },
    /// Bandage targeting.
    Bandage {
        healer_serial: u32,
        bandage_item_serial: u32,
    },
    /// GM dot-command targeting (.remove, .tele, .mtele).
    DotCommand(PendingTarget),
    /// `.spawner <template>` — place an admin spawner object at the targeted
    /// tile, producing the given monster template.
    SpawnerPlacement {
        /// The monster template name the spawner will produce.
        template: String,
    },
    /// House placement targeting — the player double-clicked a house deed and
    /// must now pick a spot on the ground.
    HousePlacement {
        /// The house deed's multi id to place.
        multi_id: u16,
        /// The deed item serial (consumed on success, returned on demolish).
        deed_serial: u32,
    },
    /// Ship placement targeting — the player double-clicked a ship deed and
    /// must now pick a spot on the water.
    ShipPlacement {
        /// The ship deed's North-facing multi id (used to look up the def).
        multi_id: u16,
        /// The deed item serial (consumed on success, returned on re-deed).
        deed_serial: u32,
    },
    /// Poisoning skill — step 1: the player used the Poisoning skill and must
    /// now pick a poison potion bottle (the poison level is read from it).
    PoisonSelectBottle {
        /// The player applying the poison.
        user_serial: u32,
    },
    /// Poisoning skill — step 2: the player has chosen a poison bottle and must
    /// now pick a fencing weapon to apply the poison to.
    PoisonSelectWeapon {
        /// Poison level (`1..=4` = Lesser..Deadly).
        level: u8,
        /// The poison potion item serial (consumed on success).
        potion_serial: u32,
        /// The player applying the poison.
        user_serial: u32,
    },
    /// Target cursor sent by a controller (via WorldEvent::TargetedTargetCursor).
    ///
    /// The cursor response is forwarded to the controller as
    /// `GameCommand::TargetResponse`.  The controller itself tracks
    /// spell/skill state — the session only needs to hold the cursor ID.
    Controller,
    /// Resource gathering — the player double-clicked a gathering tool and
    /// must now target a resource source (static tile or item node).
    GatherTarget {
        /// The player gathering.
        user_serial: u32,
        /// The tool's item graphic (looks up the tool definition).
        tool_graphic: u16,
    },
    /// Shrink potion — the player double-clicked a shrink potion and must now
    /// target one of their own tamed animals to turn into a statue item.
    ShrinkSelectAnimal {
        /// The player using the potion.
        user_serial: u32,
        /// The shrink potion item serial (consumed on success).
        potion_serial: u32,
    },
    /// Treasure digging — step 1: the player double-clicked a digging tool and
    /// must now target a treasure map in their backpack.
    TreasureSelectMap {
        /// The player digging.
        user_serial: u32,
        /// The digging tool item serial (consumed on a successful dig).
        tool_serial: u32,
    },
    /// Treasure digging — step 2: the player has chosen a treasure map and must
    /// now target the ground tile to dig.
    TreasureDigTile {
        /// The player digging.
        user_serial: u32,
        /// The digging tool item serial.
        tool_serial: u32,
        /// The chosen treasure map item serial.
        map_serial: u32,
        /// The treasure location id stored in the map.
        loc_id: u32,
        /// The treasure level stored in the map.
        level: u8,
    },
}

// ── PendingCursor ────────────────────────────────────────────────────────

/// A target cursor that has been sent to the client and is awaiting a
/// response (0x6C).
#[derive(Debug)]
pub(super) struct PendingCursor {
    /// The cursor ID sent to the client — used to validate the response.
    pub cursor_id: u32,
    /// What this cursor is for.
    pub kind: CursorKind,
}

impl PendingCursor {
    /// Create a `PendingCursor` from a `PendingSpell` (magic.rs).
    pub fn from_spell(ps: &PendingSpell) -> Self {
        Self {
            cursor_id: ps.cursor_id,
            kind: CursorKind::Spell {
                spell: ps.spell,
                caster_serial: ps.caster_serial,
                scroll_item_serial: ps.scroll_item_serial,
            },
        }
    }

    /// Create a `PendingCursor` for a skill target.
    pub fn skill(cursor_id: u32, skill_id: u16, user_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::Skill { skill_id, user_serial },
        }
    }

    /// Create a `PendingCursor` for a bandage target.
    pub fn bandage(cursor_id: u32, healer_serial: u32, bandage_item_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::Bandage { healer_serial, bandage_item_serial },
        }
    }

    /// Create a `PendingCursor` for a dot-command target.
    pub fn dot_command(target: PendingTarget) -> Self {
        Self {
            cursor_id: target.cursor_id(),
            kind: CursorKind::DotCommand(target),
        }
    }

    /// Cursor id used for `.spawner` placement.
    pub fn spawner_cursor_id() -> u32 {
        common::dot_commands::CMD_CURSOR_BASE | 0x04
    }

    /// Create a `PendingCursor` for `.spawner <template>` placement.
    pub fn spawner_placement(template: String) -> Self {
        Self {
            cursor_id: Self::spawner_cursor_id(),
            kind: CursorKind::SpawnerPlacement { template },
        }
    }

    /// Create a `PendingCursor` for a controller-originated target cursor.
    pub fn controller(cursor_id: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::Controller,
        }
    }

    /// Create a `PendingCursor` for house placement (deed → ground target).
    pub fn house_placement(cursor_id: u32, multi_id: u16, deed_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::HousePlacement { multi_id, deed_serial },
        }
    }

    /// Create a `PendingCursor` for ship placement (deed → water target).
    pub fn ship_placement(cursor_id: u32, multi_id: u16, deed_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::ShipPlacement { multi_id, deed_serial },
        }
    }

    /// Create a `PendingCursor` for the Poisoning skill, step 1 (pick a bottle).
    pub fn poison_select_bottle(cursor_id: u32, user_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::PoisonSelectBottle { user_serial },
        }
    }

    /// Create a `PendingCursor` for the Poisoning skill, step 2 (pick a weapon).
    pub fn poison_select_weapon(cursor_id: u32, level: u8, potion_serial: u32, user_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::PoisonSelectWeapon { level, potion_serial, user_serial },
        }
    }

    /// Create a `PendingCursor` for resource gathering (tool → source target).
    pub fn gather(cursor_id: u32, user_serial: u32, tool_graphic: u16) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::GatherTarget { user_serial, tool_graphic },
        }
    }

    /// Create a `PendingCursor` for the shrink potion (potion → animal target).
    pub fn shrink_select_animal(cursor_id: u32, user_serial: u32, potion_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::ShrinkSelectAnimal { user_serial, potion_serial },
        }
    }

    /// Create a `PendingCursor` for treasure digging, step 1 (pick a map).
    pub fn treasure_select_map(cursor_id: u32, user_serial: u32, tool_serial: u32) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::TreasureSelectMap { user_serial, tool_serial },
        }
    }

    /// Create a `PendingCursor` for treasure digging, step 2 (pick a tile).
    pub fn treasure_dig_tile(
        cursor_id: u32, user_serial: u32, tool_serial: u32, map_serial: u32, loc_id: u32, level: u8,
    ) -> Self {
        Self {
            cursor_id,
            kind: CursorKind::TreasureDigTile { user_serial, tool_serial, map_serial, loc_id, level },
        }
    }

    /// Returns `true` if this is a game-logic cursor (Spell, Skill, Bandage)
    /// as opposed to an infrastructure cursor (DotCommand).
    ///
    /// Used for action-slot blocking: a pending game-logic cursor should
    /// prevent starting new casts/skills/bandages, but a DotCommand cursor
    /// should not.
    pub fn is_game_cursor(&self) -> bool {
        !matches!(self.kind, CursorKind::DotCommand(_) | CursorKind::Controller | CursorKind::SpawnerPlacement { .. } | CursorKind::HousePlacement { .. } | CursorKind::ShipPlacement { .. })
    }
}
