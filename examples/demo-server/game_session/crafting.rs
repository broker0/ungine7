//! Crafting session flow: smelting ore at a forge and blacksmithing at an
//! anvil via a gump menu.
//!
//! ## Smelting
//! 1. Player double-clicks an **iron ore** stack (backpack or ground).
//! 2. The server checks a **forge** world-object is within range.
//! 3. A timed [`ActionPayload::Smelt`] begins (occupies the `SkillUse` slot).
//! 4. On completion the whole ore stack is consumed and the equivalent number
//!    of ingots is placed in the backpack.
//!
//! ## Blacksmithing
//! 1. Player double-clicks a **smith's hammer**.
//! 2. The server checks an **anvil** world-object is within range and opens a
//!    crafting gump (category tabs → recipes).
//! 3. Selecting a recipe button validates the anvil + ingredients and begins a
//!    timed [`ActionPayload::Craft`].
//! 4. On completion ingredients are consumed and, on a successful roll, the
//!    finished weapon/armor item is placed in the backpack (armor pieces carry
//!    their `armor_rating` in `ItemProps.meta`).

use log::{info, warn};

use protocol::RawPacket;
use packets::interaction::DoubleClick;
use packets::gump::{GumpTextLine, SendGumpDialog};
use packets::traits::{ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::{DropResult, DropTarget, HeldItemInfo};
use common::uo_engine::item_props::{ItemProps, MetaValue};
use common::uo_engine::rpc::EngineProxy;
use common::uo_engine::serial_alloc::SerialAllocator;
use framework::ecumene::{Entity as EngineEntity, TileRect};

use crate::actions::{self, ActionKind, ActionPayload, ActiveAction};
use crate::constants::craft;
use crate::crafting::{self, CraftCategory, RecipeDef};
use crate::game_util;
use crate::{DemoCommand, DemoWorkerTx};

use super::game_logic::InfraState;
use super::session_state::SessionContext;

// ── Gump constants ─────────────────────────────────────────────────────────

/// Gump id for the blacksmithing menu.
pub(super) const CRAFT_GUMP_ID: u32 = 0x424C_4B53; // "BLKS"

/// Button id: close the gump.
const BTN_CLOSE: u32 = 0;
/// Button id base for category-tab switches (`BTN_CATEGORY + index`).
const BTN_CATEGORY: u32 = 1000;
/// Button id base for "make recipe" buttons (`BTN_MAKE + global_recipe_index`).
const BTN_MAKE: u32 = 2000;

// ── Forge / anvil proximity ─────────────────────────────────────────────────

/// Returns `true` if any item-entity with a graphic in `graphics` lies within
/// [`craft::RANGE`] of `(px, py)`.
async fn world_object_near(
    engine: &EngineProxy<DemoCommand>,
    px: u16,
    py: u16,
    graphics: &[u16],
) -> bool {
    let area = TileRect::from_view(px, py, craft::RANGE);
    let entities = engine.query_area(area).await;
    for ent in &entities {
        if let DemoEntity::Item { graphic, x, y, .. } = ent {
            if graphics.contains(graphic)
                && game_util::chebyshev(px, py, *x, *y) <= craft::RANGE
            {
                return true;
            }
        }
    }
    false
}

/// Returns `true` if a forge is within range of the player position.
async fn forge_near(engine: &EngineProxy<DemoCommand>, px: u16, py: u16) -> bool {
    world_object_near(engine, px, py, craft::FORGE_GRAPHICS).await
}

/// Returns `true` if an anvil is within range of the player position.
async fn anvil_near(engine: &EngineProxy<DemoCommand>, px: u16, py: u16) -> bool {
    world_object_near(engine, px, py, craft::ANVIL_GRAPHICS).await
}

// ── Item graphic resolution (equipped → backpack → ground) ───────────────────

async fn resolve_item_graphic(
    engine: &EngineProxy<DemoCommand>,
    player_serial: u32,
    item_serial: u32,
) -> Option<u16> {
    if let Some((_serial, graphic, _color, _amount)) = engine.find_item_info(item_serial).await {
        return Some(graphic);
    }
    if let Some(m) = engine.get_entity(player_serial).await.as_ref().and_then(|e| e.mobile()) {
        if let Some(eq) = m.items.iter().find(|eq| eq.serial == item_serial) {
            return Some(eq.graphic);
        }
    }
    if let Some(DemoEntity::Item { graphic, .. }) = engine.get_entity(item_serial).await {
        return Some(graphic);
    }
    None
}

// ── Smelting: ore double-click ───────────────────────────────────────────────

/// Check if a double-click targets a smeltable ore.  If so, verify a forge is
/// nearby and begin a timed smelt action.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_ore_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    skill_timer: &mut std::pin::Pin<Box<tokio::time::Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if packet.id() != DoubleClick::ID {
        return Ok(false);
    }
    let Ok(dc) = DoubleClick::from_bytes(&packet.data) else {
        return Ok(false);
    };
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false); // paperdoll request
    }
    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let Some(p) = &ctx.infra.player else {
        return Ok(false);
    };
    let (player_serial, world, px, py) = (p.serial, p.world, p.x, p.y);

    let engine = game_util::engine_for(worker_tx, world);
    let Some(graphic) = resolve_item_graphic(&engine, player_serial, clean_serial).await else {
        return Ok(false);
    };

    // Is it a smeltable ore?
    if crafting::smelt_result(graphic).is_none() {
        return Ok(false);
    }

    // Skill-slot blocking.
    let has_pending = ctx.has_pending_cursor();
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, has_pending, ctx.has_blocking_gump()) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Must be near a forge.
    if !forge_near(&engine, px, py).await {
        session.send(game_util::system_message("You must be near a forge to smelt ore.")).await?;
        return Ok(true);
    }

    let delay = std::time::Duration::from_millis(craft::SMELT_DELAY_MS);
    let payload = ActionPayload::Smelt { user_serial: player_serial, ore_serial: clean_serial, world };
    let action = ActiveAction::new(ActionKind::SkillUse, delay, payload);
    skill_timer.as_mut().reset(action.completes_at);
    ctx.active_skill = Some(action);

    session.send(game_util::system_speech("You put the ore in the forge...")).await?;
    Ok(true)
}

/// Complete a smelt action: re-check forge, consume the ore stack, produce
/// ingots into the backpack.
pub(super) async fn complete_smelt(
    user_serial: u32,
    ore_serial: u32,
    world: u8,
    serial_alloc: &std::sync::Arc<SerialAllocator>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = game_util::engine_for(worker_tx, world);

    let (px, py) = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y),
        None => return Ok(()),
    };
    if !forge_near(&engine, px, py).await {
        session.send(game_util::system_message("You move too far from the forge.")).await?;
        return Ok(());
    }

    // Look up the ore amount + graphic.
    let Some((_serial, ore_graphic, _color, amount)) = engine.find_item_info(ore_serial).await else {
        session.send(game_util::system_message("The ore is no longer there.")).await?;
        return Ok(());
    };
    let Some((ingot_graphic, per_ore)) = crafting::smelt_result(ore_graphic) else {
        return Ok(());
    };
    if amount == 0 {
        return Ok(());
    }

    // Consume the entire ore stack.
    let consumed = engine.consume_item(ore_serial, amount, Some(ore_graphic)).await;
    if consumed.is_none() {
        session.send(game_util::system_message("You have no ore to smelt.")).await?;
        return Ok(());
    }

    let ingot_amount = amount.saturating_mul(per_ore).max(1);

    // Place ingots in the backpack.
    let Some(bp_serial) = engine.get_entity(user_serial).await.and_then(|e| e.backpack_serial()) else {
        warn!("[craft] player {:#010X} has no backpack — discarding ingots", user_serial);
        return Ok(());
    };
    let Some(serial) = serial_alloc.alloc_item() else {
        warn!("[craft] serial space exhausted — cannot create ingots");
        return Ok(());
    };

    let item = HeldItemInfo { serial, graphic: ingot_graphic, color: 0, amount: ingot_amount };
    let target = DropTarget::OnEntity { target_serial: bp_serial, x: 0xFFFF, y: 0xFFFF };
    match engine.drop_item(user_serial, item, target, None).await {
        DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. } => {
            game_util::send_sound(worker_tx, world, craft::SOUND_SMELT, px, py, 0).await;
            session.send(game_util::system_speech(
                &format!("You smelt the ore into {} ingots.", ingot_amount),
            )).await?;
            info!("[craft] 0x{:08X} smelted {}x ore into {}x ingots", user_serial, amount, ingot_amount);
        }
        other => {
            warn!("[craft] unexpected drop result for ingots: {:?}", other);
            session.send(game_util::system_message("Your backpack cannot hold the ingots.")).await?;
        }
    }
    Ok(())
}

// ── Blacksmithing: hammer double-click → gump ────────────────────────────────

/// Check if a double-click targets a smith's hammer.  If so, verify an anvil
/// is nearby and open the crafting gump.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_hammer_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if packet.id() != DoubleClick::ID {
        return Ok(false);
    }
    let Ok(dc) = DoubleClick::from_bytes(&packet.data) else {
        return Ok(false);
    };
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }
    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let Some(p) = &ctx.infra.player else {
        return Ok(false);
    };
    let (player_serial, world, px, py) = (p.serial, p.world, p.x, p.y);

    let engine = game_util::engine_for(worker_tx, world);
    let Some(graphic) = resolve_item_graphic(&engine, player_serial, clean_serial).await else {
        return Ok(false);
    };
    if !crafting::is_smith_hammer(graphic) {
        return Ok(false);
    }

    // Must be near an anvil.
    if !anvil_near(&engine, px, py).await {
        session.send(game_util::system_message("You must be near an anvil to smith.")).await?;
        return Ok(true);
    }

    // Open the gump on the first category.
    open_craft_gump(CraftCategory::Weapons, &mut ctx.infra, session).await?;
    Ok(true)
}

/// Build and send the crafting gump for the given category.
pub(super) async fn open_craft_gump(
    category: CraftCategory,
    infra: &mut InfraState,
    session: &mut Session,
) -> error::Result<()> {
    let mut layout = String::new();
    layout.push_str("{ page 0 }{ noclose }");
    layout.push_str("{ resizepic 0 0 5054 300 340 }");
    // Title line (text index 0).
    layout.push_str("{ text 20 12 1153 0 }");

    let mut text_lines: Vec<GumpTextLine> = vec![GumpTextLine("Blacksmithing".to_string())];

    // Category tabs along the top.
    let mut tab_x = 20u32;
    for (i, cat) in CraftCategory::all().iter().enumerate() {
        let btn = BTN_CATEGORY + i as u32;
        let idx = text_lines.len() as u32;
        // Highlighted tab uses a different gump-button art when selected.
        let (up, down) = if *cat == category { (4006, 4007) } else { (4005, 4006) };
        layout.push_str(&format!("{{ button {} 40 {} {} 1 0 {} }}", tab_x, up, down, btn));
        layout.push_str(&format!("{{ text {} 42 996 {} }}", tab_x + 35, idx));
        text_lines.push(GumpTextLine(cat.title().to_string()));
        tab_x += 130;
    }

    // Recipe list for the active category.
    let mut row_y = 80u32;
    for recipe in crafting::recipes_in_category(category) {
        let Some(global_idx) = crafting::RECIPES.iter().position(|r| r.key == recipe.key) else {
            continue;
        };
        let btn = BTN_MAKE + global_idx as u32;
        let idx = text_lines.len() as u32;
        layout.push_str(&format!("{{ button 20 {} 4005 4007 1 0 {} }}", row_y, btn));
        layout.push_str(&format!("{{ text 55 {} 996 {} }}", row_y + 2, idx));
        let cost = recipe_cost_label(recipe);
        text_lines.push(GumpTextLine(format!("{}  ({})", recipe.name, cost)));
        row_y += 26;
    }

    // Close button at the bottom.
    let close_idx = text_lines.len() as u32;
    layout.push_str(&format!("{{ button 20 {} 4017 4019 1 0 {} }}", row_y + 6, BTN_CLOSE));
    layout.push_str(&format!("{{ text 55 {} 996 {} }}", row_y + 8, close_idx));
    text_lines.push(GumpTextLine("Close".to_string()));

    let dialog = SendGumpDialog {
        serial: 0,
        gump_id: CRAFT_GUMP_ID,
        x: 100,
        y: 100,
        layout,
        text_lines,
        trailing_pad: vec![],
    };
    session.send(RawPacket::s2c(dialog.to_bytes())).await?;

    infra.open_craft = Some(category);
    Ok(())
}

/// Short ingredient summary, e.g. `"8 ingots"`.
fn recipe_cost_label(recipe: &RecipeDef) -> String {
    recipe
        .ingredients
        .iter()
        .map(|ing| format!("{} ingots", ing.amount))
        .collect::<Vec<_>>()
        .join(", ")
}

// ── Gump response (0xB1) ─────────────────────────────────────────────────────

/// Handle a crafting-gump response.  Returns `Ok(true)` if the gump id matched.
pub(super) async fn handle_craft_gump(
    gump_id: u32,
    button_id: u32,
    ctx: &mut SessionContext,
    skill_timer: &mut std::pin::Pin<Box<tokio::time::Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if gump_id != CRAFT_GUMP_ID {
        return Ok(false);
    }

    // Close button (or any non-handled button) — dismiss the gump.
    if button_id == BTN_CLOSE {
        ctx.infra.open_craft = None;
        return Ok(true);
    }

    // Category-tab switch — re-open the gump on the chosen category.
    if (BTN_CATEGORY..BTN_MAKE).contains(&button_id) {
        let cat_idx = (button_id - BTN_CATEGORY) as usize;
        if let Some(cat) = CraftCategory::all().get(cat_idx).copied() {
            open_craft_gump(cat, &mut ctx.infra, session).await?;
        }
        return Ok(true);
    }

    // "Make" button — look up the recipe by global index.
    if button_id >= BTN_MAKE {
        let recipe_idx = (button_id - BTN_MAKE) as usize;
        let Some(recipe) = crafting::RECIPES.get(recipe_idx) else {
            ctx.infra.open_craft = None;
            return Ok(true);
        };
        begin_craft(recipe, ctx, skill_timer, session, worker_tx).await?;
        return Ok(true);
    }

    Ok(true)
}

/// Validate the anvil + ingredients and begin a timed craft action.
async fn begin_craft(
    recipe: &'static RecipeDef,
    ctx: &mut SessionContext,
    skill_timer: &mut std::pin::Pin<Box<tokio::time::Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let Some(p) = &ctx.infra.player else {
        return Ok(());
    };
    let (player_serial, world, px, py) = (p.serial, p.world, p.x, p.y);

    let engine = game_util::engine_for(worker_tx, world);

    // Skill-slot blocking.
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, false, false) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(());
    }

    // Must still be near an anvil.
    if !anvil_near(&engine, px, py).await {
        session.send(game_util::system_message("You must be near an anvil to smith.")).await?;
        return Ok(());
    }

    // Must have the ingredients in the backpack.
    if !has_ingredients(&engine, player_serial, recipe).await {
        session.send(game_util::system_message(
            &format!("You lack the materials to make a {}.", recipe.name),
        )).await?;
        return Ok(());
    }

    // Keep the gump open so the player can craft repeatedly; start the action.
    let delay = std::time::Duration::from_millis(craft::CRAFT_DELAY_MS);
    let payload = ActionPayload::Craft { user_serial: player_serial, recipe_key: recipe.key, world };
    let action = ActiveAction::new(ActionKind::SkillUse, delay, payload);
    skill_timer.as_mut().reset(action.completes_at);
    ctx.active_skill = Some(action);
    ctx.combat_state.set_weapon_away();

    session.send(game_util::system_speech("You begin working the metal...")).await?;
    Ok(())
}

/// Complete a craft action: re-validate, consume ingredients, and on a
/// successful roll produce the item into the backpack.
pub(super) async fn complete_craft(
    user_serial: u32,
    recipe_key: &'static str,
    world: u8,
    serial_alloc: &std::sync::Arc<SerialAllocator>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let Some(recipe) = crafting::lookup_recipe(recipe_key) else {
        return Ok(());
    };
    let engine = game_util::engine_for(worker_tx, world);

    let (px, py, mounted) = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => {
            let mt = m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount);
            (m.x, m.y, mt)
        }
        None => return Ok(()),
    };
    if !anvil_near(&engine, px, py).await {
        session.send(game_util::system_message("You move too far from the anvil.")).await?;
        return Ok(());
    }

    // Working feedback.
    game_util::send_resolved_animation(worker_tx, world, user_serial, recipe.anim, mounted, 7, 1, px, py).await;
    game_util::send_sound(worker_tx, world, recipe.sound, px, py, 0).await;

    // Re-check + consume ingredients (the player may have moved items).
    if !consume_ingredients(&engine, user_serial, recipe).await {
        session.send(game_util::system_message(
            &format!("You lack the materials to make a {}.", recipe.name),
        )).await?;
        return Ok(());
    }

    // Success roll.
    if !recipe.roll_success() {
        session.send(game_util::system_speech(
            "You fail to create the item and lose some material.",
        )).await?;
        info!("[craft] 0x{:08X} failed to craft {}", user_serial, recipe.key);
        return Ok(());
    }

    // Produce the item.
    let Some(bp_serial) = engine.get_entity(user_serial).await.and_then(|e| e.backpack_serial()) else {
        warn!("[craft] player {:#010X} has no backpack — discarding crafted item", user_serial);
        return Ok(());
    };
    let Some(serial) = serial_alloc.alloc_item() else {
        warn!("[craft] serial space exhausted — cannot create crafted item");
        return Ok(());
    };

    let item = HeldItemInfo {
        serial,
        graphic: recipe.result_graphic,
        color: recipe.result_color,
        amount: 1,
    };
    let target = DropTarget::OnEntity { target_serial: bp_serial, x: 0xFFFF, y: 0xFFFF };
    match engine.drop_item(user_serial, item, target, None).await {
        DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. } => {
            // Stamp item properties: name (+ armor rating for armor pieces).
            let mut props = ItemProps::with_name(recipe.name);
            if recipe.armor_rating > 0 {
                props.set_meta("armor_rating", MetaValue::Int(recipe.armor_rating as i64));
            }
            engine.set_item_props(serial, Some(props)).await;

            session.send(game_util::system_speech(
                &format!("You create a {} and place it in your backpack.", recipe.name),
            )).await?;
            info!("[craft] 0x{:08X} crafted {} (graphic={:#06X})", user_serial, recipe.key, recipe.result_graphic);
        }
        other => {
            warn!("[craft] unexpected drop result for crafted item: {:?}", other);
            session.send(game_util::system_message("Your backpack cannot hold the item.")).await?;
        }
    }
    Ok(())
}

// ── Ingredient helpers ───────────────────────────────────────────────────────

/// Sum the available amount of `graphic` across the player's backpack.
async fn available_amount(
    engine: &EngineProxy<DemoCommand>,
    user_serial: u32,
    graphic: u16,
) -> u16 {
    let Some(bp_serial) = engine.get_entity(user_serial).await.and_then(|e| e.backpack_serial()) else {
        return 0;
    };
    let Some(info) = engine.get_container(bp_serial).await else {
        return 0;
    };
    info.items
        .iter()
        .filter(|it| it.graphic == graphic)
        .map(|it| it.amount as u32)
        .sum::<u32>()
        .min(u16::MAX as u32) as u16
}

/// Returns `true` if the backpack holds all of the recipe's ingredients.
async fn has_ingredients(
    engine: &EngineProxy<DemoCommand>,
    user_serial: u32,
    recipe: &RecipeDef,
) -> bool {
    for ing in recipe.ingredients {
        if available_amount(engine, user_serial, ing.graphic).await < ing.amount {
            return false;
        }
    }
    true
}

/// Consume all of the recipe's ingredients from the backpack.
///
/// Returns `true` if everything was consumed.  Performs a final availability
/// check first to avoid partial consumption.
async fn consume_ingredients(
    engine: &EngineProxy<DemoCommand>,
    user_serial: u32,
    recipe: &RecipeDef,
) -> bool {
    if !has_ingredients(engine, user_serial, recipe).await {
        return false;
    }

    let Some(bp_serial) = engine.get_entity(user_serial).await.and_then(|e| e.backpack_serial()) else {
        return false;
    };

    for ing in recipe.ingredients {
        let mut remaining = ing.amount;
        // Consume from matching stacks in the backpack until satisfied.
        let Some(container) = engine.get_container(bp_serial).await else {
            return false;
        };
        for it in container.items {
            if remaining == 0 {
                break;
            }
            if it.graphic != ing.graphic {
                continue;
            }
            let take = remaining.min(it.amount);
            if engine.consume_item(it.serial, take, Some(ing.graphic)).await.is_some() {
                remaining -= take;
            }
        }
        if remaining > 0 {
            return false;
        }
    }
    true
}
