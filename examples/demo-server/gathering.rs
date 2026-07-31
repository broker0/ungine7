//! Resource gathering — tool/resource definition tables.
//!
//! Data-driven gathering system (mirrors `weapon` / `potions` / `vendor`
//! tables).  A player double-clicks a **gathering tool**, then targets a
//! resource source.  A source is valid if either:
//!
//! 1. the targeted **static tile** graphic is in the tool's
//!    [`ToolDef::valid_tile_graphics`], or
//! 2. the targeted **item entity** carries a [`META_GATHER_RESOURCE`] meta
//!    value matching the tool's [`GatherKind`].
//!
//! On success a resource item is dropped into the player's backpack.  The
//! amount/quality actually produced is governed by the source's **resource
//! node** state (capacity, depletion and time-based regeneration / maturation)
//! — see [`crate::resource_nodes`].  Each [`GatherKind`] maps to a node policy
//! there; the [`ResourceDrop`] below is the *fallback* used only when no node
//! policy applies.
//!
//! ## Extending
//!
//! Add a new [`ToolDef`] entry to [`TOOLS`].  To add a new resource type,
//! add a [`GatherKind`] variant and a tool that produces it, then bind the
//! kind to a node policy in [`crate::resource_nodes::policy_for`].  Spawned
//! resource nodes (item entities) opt in by setting their `ItemProps` meta
//! key [`META_GATHER_RESOURCE`] to the [`GatherKind::as_str`] value.

#![allow(dead_code)]

use crate::constants::anim;
use crate::game_util;

// ── Meta key ───────────────────────────────────────────────────────────────

/// `ItemProps` meta key used to mark an item entity as a gatherable resource
/// node.  Its string value must equal a [`GatherKind::as_str`].
pub const META_GATHER_RESOURCE: &str = "gather_resource";

// ── GatherKind ──────────────────────────────────────────────────────────────

/// The category of gathering a tool performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatherKind {
    /// Mining ore from rock/cave tiles with a pickaxe.
    Mining,
}

impl GatherKind {
    /// Stable string identifier (used for the `gather_resource` meta value).
    pub fn as_str(self) -> &'static str {
        match self {
            GatherKind::Mining => "mining",
        }
    }
}

// ── ResourceDrop ─────────────────────────────────────────────────────────────

/// What a successful gather yields.
#[derive(Debug, Clone, Copy)]
pub struct ResourceDrop {
    /// Item graphic of the produced resource.
    pub graphic: u16,
    /// Item hue.
    pub color: u16,
    /// Display name for the produced resource.
    pub name: &'static str,
    /// Minimum amount produced on success (inclusive).
    pub amount_min: u16,
    /// Maximum amount produced on success (inclusive).
    pub amount_max: u16,
    /// Success chance, percent `0..=100`.
    pub chance: u8,
}

// ── ToolDef ──────────────────────────────────────────────────────────────────

/// A gathering tool definition.
#[derive(Debug, Clone, Copy)]
pub struct ToolDef {
    /// Item graphic of the tool the player double-clicks.
    pub tool_graphic: u16,
    /// What kind of gathering this tool performs.
    pub kind: GatherKind,
    /// Human-readable tool name.
    pub name: &'static str,
    /// Static tile graphics that this tool can gather from.
    pub valid_tile_graphics: &'static [u16],
    /// The resource produced on success.
    pub resource: ResourceDrop,
    /// Sound played while working.
    pub sound: u16,
    /// Mobile animation (action id) played while working.
    pub anim: u16,
    /// Time the gather takes before completion, milliseconds.
    pub delay_ms: u64,
}

// ── Tables ────────────────────────────────────────────────────────────────────

/// Iron ore item graphic produced by mining.
pub const IRON_ORE: u16 = 0x19B9;

/// Mining sound (pickaxe striking stone).
pub const SOUND_PICKAXE: u16 = 0x0125;

/// Mining range, in tiles (Chebyshev distance to the targeted source).
pub const GATHER_RANGE: u16 = 2;

/// All gathering tools.  Currently: mining only.
pub static TOOLS: &[ToolDef] = &[ToolDef {
    tool_graphic: 0x0E85, // pickaxe
    kind: GatherKind::Mining,
    name: "pickaxe",
    // Cave / mountain rock tiles that yield ore.
    valid_tile_graphics: &[0x053B, 0x053C],
    resource: ResourceDrop {
        graphic: IRON_ORE,
        color: 0,
        name: "iron ore",
        amount_min: 1,
        amount_max: 3,
        chance: 70,
    },
    sound: SOUND_PICKAXE,
    anim: anim::SWING_2H,
    delay_ms: 2000,
}];

// ── Lookups ───────────────────────────────────────────────────────────────────

/// Look up a tool by its item graphic.
pub fn lookup_tool(tool_graphic: u16) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.tool_graphic == tool_graphic)
}

impl ToolDef {
    /// Returns `true` if the given static tile graphic is a valid source for
    /// this tool.
    pub fn tile_is_valid(&self, tile_graphic: u16) -> bool {
        self.valid_tile_graphics.contains(&tile_graphic)
    }

    /// Roll the success chance and amount for this tool's resource.
    ///
    /// Returns `Some((graphic, color, amount))` on success, `None` on a
    /// failed roll.
    pub fn roll_drop(&self) -> Option<(u16, u16, u16)> {
        let r = &self.resource;
        let roll = game_util::random_range(1, 100);
        if roll as u8 > r.chance {
            return None;
        }
        let amount = game_util::random_range(r.amount_min, r.amount_max);
        Some((r.graphic, r.color, amount))
    }
}
