//! Item stackability rules.
//!
//! Determines whether two items of the same `graphic + color` may be merged
//! into a single stack.  In real UO this is governed by the `STACKABLE` flag
//! in `tiledata.mul`; here we use a three-tier resolution mirroring the
//! weight system in the demo server:
//!
//! 1. **Override table** ([`STACKABLE_GRAPHICS`]) — a built-in list of
//!    known-stackable item graphics, so stacking works correctly even when the
//!    server is launched without client data files (`--data-dir`).
//! 2. **Tiledata** — the authoritative `STACKABLE` flag from `tiledata.mul`,
//!    consulted via [`StaticDataProvider`] when available.
//! 3. **Default** — `false` (do NOT stack).  This is the safe default: an
//!    unknown item is treated as unique (weapons, spellbooks, maps, etc.).

use files::tiledata::TileFlags;
use framework::vessel::traits::StaticDataProvider;

/// Built-in list of item graphics that are stackable by default.
///
/// Used as the first tier of [`is_stackable`] so that stacking behaves
/// correctly without client `tiledata.mul`.  Keep this in sync with the
/// stackable items the demo server actually creates (reagents, scrolls,
/// ammunition, crafting materials, gems, gold).
///
/// **Note:** potions are intentionally *not* listed — bottles do not stack in
/// the traditional UO ruleset.
pub static STACKABLE_GRAPHICS: &[u16] = &[
    // ── Gold ─────────────────────────────────────────────────────────────
    0x0EED, // gold coin

    // ── Reagents ──────────────────────────────────────────────────────────
    0x0F7A, // black pearl
    0x0F7B, // blood moss
    0x0F84, // garlic
    0x0F85, // ginseng
    0x0F86, // mandrake root
    0x0F88, // nightshade
    0x0F8C, // sulphurous ash
    0x0F8D, // spider's silk

    // ── Ammunition ──────────────────────────────────────────────────────────
    0x0F3F, // arrow
    0x1BFB, // bolt

    // ── Fletching materials ──────────────────────────────────────────────────
    0x1BD1, // feather
    0x1BD4, // shaft

    // ── Spell scrolls ──────────────────────────────────────────────────────
    0x1F2D, // Reactive Armor
    0x1F2E, // Clumsy
    0x1F2F, // Create Food
    0x1F30, // Feeblemind
    0x1F31, // Heal
    0x1F32, // Magic Arrow
    0x1F33, // Night Sight
    0x1F34, // Weaken
    0x1F3D, // Bless
    0x1F47, // Curse
    0x1F49, // Greater Heal
    0x1F4A, // Lightning
    0x1F56, // Energy Bolt
    0x1F5F, // Flamestrike

    // ── Crafting materials ──────────────────────────────────────────────────
    0x19B9, // iron ore
    0x1BF2, // iron ingot

    // ── Misc reagents / drops ──────────────────────────────────────────────
    0x0F7E, // bone

    // ── Gems ─────────────────────────────────────────────────────────────────
    0x0F0F, // star sapphire
    0x0F10, // emerald
    0x0F13, // ruby
    0x0F26, // diamond
];

/// Returns `true` if items of the given `graphic` are allowed to merge into a
/// single stack.
///
/// Resolution order:
/// 1. [`STACKABLE_GRAPHICS`] override list.
/// 2. `tiledata.mul` `STACKABLE` flag via `static_data` (if present).
/// 3. `false`.
pub fn is_stackable(graphic: u16, static_data: Option<&dyn StaticDataProvider>) -> bool {
    // 1. Override table.
    if STACKABLE_GRAPHICS.contains(&graphic) {
        return true;
    }

    // 2. Tiledata flag.
    if let Some(sd) = static_data {
        if let Some(def) = sd.static_tile_def(graphic) {
            return def.flags.has(TileFlags::STACKABLE);
        }
    }

    // 3. Default: not stackable.
    false
}
