//! DEV-ONLY: Grant starter items to a player on login.
//!
//! Adds 20 of every implemented reagent and 10 of every implemented scroll
//! to the player's backpack as a convenience for development/testing.
//! Items are dropped into the backpack container so they automatically merge
//! with any existing stacks.
//!
//! Also equips a starter leather armor set for combat testing.

use log::warn;

use common::uo_engine::handler::{DropTarget, HeldItemInfo};
use common::uo_engine::item_props::{ItemProps, MetaValue};
use common::uo_engine::serial_alloc::SerialAllocator;
use framework::ecumene::Entity as EngineEntity;
use packets::world::EquippedItem;

use std::sync::Arc;

use crate::constants::{armor, item};
use crate::DemoWorkerTx;

use super::PlayerState;

// ── Item tables ──────────────────────────────────────────────────────────

/// All implemented reagent graphics.
const REAGENTS: &[(u16, &str)] = &[
    (item::REAGENT_BLACK_PEARL,    "Black Pearl"),
    (item::REAGENT_BLOOD_MOSS,     "Blood Moss"),
    (item::REAGENT_GARLIC,         "Garlic"),
    (item::REAGENT_GINSENG,        "Ginseng"),
    (item::REAGENT_MANDRAKE_ROOT,  "Mandrake Root"),
    (item::REAGENT_NIGHTSHADE,     "Nightshade"),
    (item::REAGENT_SULPHUROUS_ASH, "Sulphurous Ash"),
    (item::REAGENT_SPIDERS_SILK,   "Spider's Silk"),
];

/// All implemented scroll graphics.
const SCROLLS: &[(u16, &str)] = &[
    (item::SCROLL_HEAL,         "Scroll of Heal"),
    (item::SCROLL_MAGIC_ARROW,  "Scroll of Magic Arrow"),
    (item::SCROLL_BLESS,        "Scroll of Bless"),
    (item::SCROLL_CURSE,        "Scroll of Curse"),
    (item::SCROLL_GREATER_HEAL, "Scroll of Greater Heal"),
    (item::SCROLL_LIGHTNING,    "Scroll of Lightning"),
    (item::SCROLL_ENERGY_BOLT,  "Scroll of Energy Bolt"),
    (item::SCROLL_FLAMESTRIKE,  "Scroll of Flamestrike"),
];

const REAGENT_AMOUNT: u16 = 20;
const SCROLL_AMOUNT: u16  = 10;
const POTION_AMOUNT: u16  = 5;
/// Number of blank recall runes to grant (runes don't stack).
const RUNE_COUNT: u16     = 3;
/// Distinct hues for the starter runes so they don't auto-merge in the
/// backpack (the drop logic stacks items with the same graphic + hue).
const RUNE_HUES: &[u16]   = &[0x0021, 0x0026, 0x002B];

/// Starter potions — one of each type for testing.
const POTIONS: &[(u16, u16, &str)] = &[
    (item::POTION_GREATER_HEAL, 0,      "Greater Heal Potion"),
    (item::POTION_REFRESH,      0x002D, "Greater Refresh Potion"),
    (item::POTION_MANA,         0x0005, "Greater Mana Potion"),
    (item::POTION_CURE,         0,      "Greater Cure Potion"),
    (item::POTION_STRENGTH,     0x0035, "Greater Strength Potion"),
    (item::POTION_AGILITY,      0,      "Greater Agility Potion"),
    // Shrink potion — hue must match the `POTIONS` table in `crate::potions`
    // so it resolves to the shrink effect on double-click.
    (item::POTION_SHRINK,       item::SHRINK_POTION_HUE, "Shrink Potion"),
];

/// Starter poison bottles — one of each level for testing.
///
/// All four share the single poison-bottle graphic; the level is carried in
/// per-instance `ItemProps.meta` (not the hue), so the bottles look identical
/// unless [`crate::potions::poison_level_hue`] tints a specific level.
const POISON_LEVELS: &[u8] = &[1, 2, 3, 4];

/// Starter armor set — leather armor for combat testing.
///
/// Each entry: `(name_in_template_table)` — looked up from `ARMOR_TEMPLATES`
/// by name for clarity.  We use the "Leather" tier (default color).
const STARTER_ARMOR: &[&str] = &[
    "Leather Chest",
    "Leather Gloves",
    "Leather Arms",
    "Leather Legs",
    "Leather Cap",
];

// ── Public entry point ───────────────────────────────────────────────────

/// Give starter items to `player` on first spawn.
///
/// Called exactly once per character lifetime — on a fresh entity (first
/// spawn of a test account or a newly created normal character).  Drops
/// reagents, scrolls, potions, tools and other consumables into the backpack
/// with `accessible_containers: None` (GM bypass) so the engine auto-stacks
/// stackable items.  Also equips a starter leather armor set.
pub(super) async fn give_starter_items(
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
) {
    // Resolve backpack serial from the engine.
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let bp_serial = match engine.get_entity(player.serial).await {
        Some(entity) => match entity.backpack_serial() {
            Some(s) => s,
            None => {
                warn!(
                    "[dev_items] player {:#010X} has no backpack — skipping starter items",
                    player.serial
                );
                return;
            }
        },
        None => {
            warn!(
                "[dev_items] could not get entity for player {:#010X}",
                player.serial
            );
            return;
        }
    };

    let target = DropTarget::OnEntity {
        target_serial: bp_serial,
        x: 0xFFFF,
        y: 0xFFFF,
    };

    // Give reagents.
    for &(graphic, name) in REAGENTS {
        give_item(
            player, worker_tx, serial_alloc,
            graphic, 0, REAGENT_AMOUNT, target.clone(), name,
        ).await;
    }

    // Give scrolls.
    for &(graphic, name) in SCROLLS {
        give_item(
            player, worker_tx, serial_alloc,
            graphic, 0, SCROLL_AMOUNT, target.clone(), name,
        ).await;
    }

    // Give potions.  Potions do not stack (bottles are unique items), so give
    // POTION_AMOUNT of each as individual single items rather than one stack.
    for &(graphic, color, name) in POTIONS {
        for _ in 0..POTION_AMOUNT {
            give_item(
                player, worker_tx, serial_alloc,
                graphic, color, 1, target.clone(), name,
            ).await;
        }
    }

    // Give poison bottles — one of each level.  The level is written to
    // per-instance meta; the bottle's name/effect resolve from it.
    for &level in POISON_LEVELS {
        for _ in 0..POTION_AMOUNT {
            give_poison_potion(
                player, worker_tx, serial_alloc, level, target.clone(),
            ).await;
        }
    }

    // Give house deeds (one of each placeable house) for testing.
    give_item(
        player, worker_tx, serial_alloc,
        crate::houses::DEED_SMALL_WOOD, 0, 1, target.clone(),
        "Small Wooden House Deed",
    ).await;
    give_item(
        player, worker_tx, serial_alloc,
        crate::houses::DEED_SMALL_STONE, 0, 1, target.clone(),
        "Small Stone House Deed",
    ).await;

    // Give a ship deed (one placeable ship) for testing.
    give_item(
        player, worker_tx, serial_alloc,
        crate::ships::DEED_SMALL_SHIP, 0, 1, target.clone(),
        "Small Ship Deed",
    ).await;

    // Give a pickaxe for resource-gathering (mining) testing.
    give_item(
        player, worker_tx, serial_alloc,
        0x0E85, 0, 1, target.clone(),
        "Pickaxe",
    ).await;

    // Give treasure digging tools (consumed per dig) for treasure-map testing.
    for _ in 0..5 {
        give_item(
            player, worker_tx, serial_alloc,
            crate::treasure_map::DIGGING_TOOL, 0, 1, target.clone(),
            "a treasure digging tool",
        ).await;
    }

    // Give a couple of tattered treasure maps (double-click to decode) for
    // treasure-hunting testing.
    for _ in 0..2 {
        give_item(
            player, worker_tx, serial_alloc,
            crate::treasure_map::TATTERED_MAP, 0, 1, target.clone(),
            "a tattered treasure map",
        ).await;
    }

    // Give a smith's hammer + iron ore for crafting (smelt → forge) testing.
    give_item(
        player, worker_tx, serial_alloc,
        item::SMITH_HAMMER, 0, 1, target.clone(),
        "Smith's Hammer",
    ).await;
    give_item(
        player, worker_tx, serial_alloc,
        item::IRON_ORE, 0, 50, target.clone(),
        "Iron Ore",
    ).await;

    // Give a spellbook (double-click to open the spell list).
    give_item(
        player, worker_tx, serial_alloc,
        item::SPELLBOOK, 0, 1, target.clone(),
        "Spellbook",
    ).await;

    // Give blank recall runes for Mark / Recall testing.  Runes do not stack
    // (each carries its own marked location), so give each a distinct hue to
    // prevent the backpack drop logic from auto-merging them into one stack.
    for i in 0..RUNE_COUNT {
        let hue = RUNE_HUES[(i as usize) % RUNE_HUES.len()];
        give_item(
            player, worker_tx, serial_alloc,
            item::RUNE, hue, 1, target.clone(),
            "a recall rune",
        ).await;
    }

    // Equip starter armor set.
    equip_starter_armor(player, worker_tx, serial_alloc).await;
}

// ── Internal helper ──────────────────────────────────────────────────────

async fn give_item(
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
    graphic: u16,
    color: u16,
    amount: u16,
    target: DropTarget,
    name: &str,
) {
    let serial = match serial_alloc.alloc_item() {
        Some(s) => s,
        None => {
            warn!("[dev_items] serial space exhausted — cannot create {}", name);
            return;
        }
    };

    let item = HeldItemInfo { serial, graphic, color, amount };
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let result = engine.drop_item(
        player.serial,
        item,
        target,
        None, // GM bypass — no access check
    ).await;

    use common::uo_engine::handler::DropResult;
    match result {
        DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. } => {
            // success
        }
        other => {
            warn!(
                "[dev_items] unexpected result placing {} (graphic={:#06X}) \
                 into backpack of {:#010X}: {:?}",
                name, graphic, player.serial, other
            );
        }
    }
}

// ── Starter armor ────────────────────────────────────────────────────────

/// Create a poison bottle of `level` (`1..=4`) in the player's backpack and
/// stamp the level into its per-instance `ItemProps.meta`.
///
/// Unlike [`give_item`], the bottle's name/effect are derived from this meta
/// level at lookup time, so the bottle needs no explicit `ItemProps::name`.
async fn give_poison_potion(
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
    level: u8,
    target: DropTarget,
) {
    let serial = match serial_alloc.alloc_item() {
        Some(s) => s,
        None => {
            warn!("[dev_items] serial space exhausted — cannot create poison potion");
            return;
        }
    };

    let item = HeldItemInfo {
        serial,
        graphic: item::POTION_POISON,
        color: crate::potions::poison_level_hue(level),
        amount: 1,
    };
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let result = engine.drop_item(player.serial, item, target, None).await;

    use common::uo_engine::handler::DropResult;
    match result {
        DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. } => {
            // Stamp the per-instance poison level.
            let mut props = ItemProps::default();
            props.set_meta(
                crate::game_session::poison::META_POISON_LEVEL,
                MetaValue::Int(level as i64),
            );
            engine.set_item_props(serial, Some(props)).await;
        }
        other => {
            warn!(
                "[dev_items] unexpected result placing poison potion (level {}) \
                 into backpack of {:#010X}: {:?}",
                level, player.serial, other
            );
        }
    }
}
/// Equip a starter leather armor set on the player.
///
/// For each piece, allocates an item serial, equips it on the mobile via
/// `EngineCommand::EquipOnMobile`, and sets `ItemProps` with the armor
/// rating and custom name.
async fn equip_starter_armor(
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
    serial_alloc: &Arc<SerialAllocator>,
) {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    for &armor_name in STARTER_ARMOR {
        // Look up the template by name.
        let Some(template) = armor::ARMOR_TEMPLATES
            .iter()
            .find(|t| t.name == armor_name)
        else {
            warn!("[dev_items] armor template not found: {}", armor_name);
            continue;
        };

        let Some(item_serial) = serial_alloc.alloc_item() else {
            warn!("[dev_items] serial space exhausted — cannot create {}", armor_name);
            return;
        };

        // Equip the item on the player mobile.
        let eq = EquippedItem {
            serial: item_serial,
            graphic: template.graphic,
            layer: template.layer,
            color: if template.color == 0 { None } else { Some(template.color) },
        };
        let ok = engine.equip_on_mobile(
            player.serial, eq,
        ).await;

        if !ok {
            warn!(
                "[dev_items] failed to equip {} on {:#010X}",
                armor_name, player.serial
            );
            continue;
        }

        // Set item properties (name + armor_rating in meta).
        let mut props = ItemProps::with_name(armor_name);
        props.set_meta("armor_rating", MetaValue::Int(template.armor_rating as i64));
        engine.set_item_props(item_serial, Some(props)).await;
    }
}
