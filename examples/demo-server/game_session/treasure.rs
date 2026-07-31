//! Treasure hunting: decode tattered maps, dig at marked locations to spawn a
//! guarded treasure chest.
//!
//! Flow:
//! 1. Player double-clicks a **tattered map** ([`handle_tattered_map_double_click`]):
//!    the tattered map is consumed and a **treasure map** is created in the
//!    backpack, carrying a random buried-location id + level in its meta.
//! 2. Double-clicking the **treasure map** ([`handle_treasure_map_double_click`])
//!    opens the region map (a `MapMessage`), showing where to dig.
//! 3. Player double-clicks a **digging tool**
//!    ([`handle_digging_tool_double_click`]) → an object target cursor asks
//!    which treasure map to use.
//! 4. Player targets the map ([`handle_treasure_select_map_target`]) → a ground
//!    target cursor asks where to dig.
//! 5. Player targets the tile ([`handle_treasure_dig_tile_target`]) → if the
//!    tile matches the buried location, a timed [`ActionPayload::TreasureDig`]
//!    begins (occupies the `SkillUse` slot).
//! 6. On completion ([`complete_treasure_dig`]) the guardians + a closed
//!    treasure chest are spawned, the chest is filled with loot, the map and
//!    digging tool are consumed, and a decay timer is scheduled to remove the
//!    chest + guardians after a fixed period.

use log::{info, warn};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};
use packets::interaction::{DoubleClick, DrawContainer, DrawContainerLegacy, TargetCursor};
use packets::map::{MapMessage, MapPacket};
use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;

use u_core::Heading;

use network::error;
use network::session::Session;

use common::uo_engine::entity::{DemoEntity, MobileData};
use common::uo_engine::handler::{DropResult, DropTarget, HeldItemInfo};
use common::uo_engine::item_props::{ItemProps, MetaValue};
use common::uo_engine::notoriety::NotorietyClass;
use common::uo_engine::rpc::EngineProxy;

use framework::continuum::WorkerCommand;
use framework::ecumene::Entity as EngineEntity;

use crate::actions::{self, ActionKind, ActionPayload, ActiveAction};
use crate::controller_registry::{MonsterCfg, MonsterController};
use crate::game_util;
use crate::treasure_map::{self, GuardianDef};
use crate::{DemoCommand, DemoWorkerTx};

use super::pending_cursor::{CursorKind, PendingCursor};
use super::session_state::SessionContext;

// ── Constants ────────────────────────────────────────────────────────────

/// Cursor ID base for treasure targets (distinct from spell/skill/gather).
const TREASURE_CURSOR_BASE: u32 = 0x7EA5_0000;

// ── tattered map → treasure map ────────────────────────────────────────────

/// Decode a tattered map into a treasure map on double-click.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_tattered_map_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some((clean_serial, player_serial, world)) = parse_item_doubleclick(packet, ctx) else {
        return Ok(false);
    };

    let engine = game_util::engine_for(worker_tx, world);
    let Some(graphic) = resolve_item_graphic(&engine, player_serial, clean_serial).await else {
        return Ok(false);
    };
    if graphic != treasure_map::TATTERED_MAP {
        return Ok(false);
    }

    // Resolve the player's backpack to place the decoded map.
    let bp_serial = match engine.get_entity(player_serial).await.and_then(|e| e.backpack_serial()) {
        Some(s) => s,
        None => return Ok(true),
    };

    // Consume the tattered map.
    if engine.consume_item(clean_serial, 1, Some(treasure_map::TATTERED_MAP)).await.is_none() {
        return Ok(true);
    }

    // Choose a random buried location + level for the decoded map.
    let loc_id = treasure_map::random_location_id();
    let level = treasure_map::roll_level_for_body(0);

    // Create the treasure map item.
    let Some(map_serial) = ctx.serial_alloc.alloc_item() else {
        warn!("[treasure] serial space exhausted — cannot create treasure map");
        return Ok(true);
    };

    let item = HeldItemInfo {
        serial: map_serial,
        graphic: treasure_map::TREASURE_MAP,
        color: treasure_map::map_hue_for_location(loc_id),
        amount: 1,
    };
    let target = DropTarget::OnEntity { target_serial: bp_serial, x: 0xFFFF, y: 0xFFFF };
    let result = engine.drop_item(player_serial, item, target, None).await;

    match result {
        DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. } => {
            // Store the buried-location id + level on the new map.
            let mut props = ItemProps::with_name("a treasure map");
            props.set_meta(treasure_map::META_TREASURE_LOC, MetaValue::Int(loc_id as i64));
            props.set_meta(treasure_map::META_TREASURE_LEVEL, MetaValue::Int(level as i64));
            engine.set_item_props(map_serial, Some(props)).await;

            session.send(game_util::system_speech(
                "The tattered map crumbles away, revealing a clearer treasure map.",
            )).await?;
            info!(
                "[treasure] 0x{:08X} decoded tattered map → treasure map 0x{:08X} (loc={}, level={})",
                player_serial, map_serial, loc_id, level,
            );
        }
        other => {
            warn!("[treasure] unexpected drop result for treasure map: {:?}", other);
            session.send(game_util::system_message("Your backpack cannot hold the map.")).await?;
        }
    }

    Ok(true)
}

// ── treasure map → open region map ──────────────────────────────────────────

/// Open the region map when a treasure map is double-clicked.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_treasure_map_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some((clean_serial, player_serial, world)) = parse_item_doubleclick(packet, ctx) else {
        return Ok(false);
    };

    let engine = game_util::engine_for(worker_tx, world);
    let Some(graphic) = resolve_item_graphic(&engine, player_serial, clean_serial).await else {
        return Ok(false);
    };
    if graphic != treasure_map::TREASURE_MAP {
        return Ok(false);
    }

    // Read the buried location from the map's meta.
    let Some(loc) = read_map_location(&engine, clean_serial).await else {
        session.send(game_util::system_message("This map is blank.")).await?;
        return Ok(true);
    };

    let half = treasure_map::MAP_REGION_HALF;
    let ul_x = loc.x.saturating_sub(half);
    let ul_y = loc.y.saturating_sub(half);
    let lr_x = loc.x.saturating_add(half);
    let lr_y = loc.y.saturating_add(half);

    let msg = MapMessage {
        id: MapMessage::ID,
        map_serial: clean_serial,
        gump_art: treasure_map::MAP_GUMP_ART,
        upper_left_x: ul_x,
        upper_left_y: ul_y,
        lower_right_x: lr_x,
        lower_right_y: lr_y,
        gump_width: treasure_map::MAP_GUMP_SIZE,
        gump_height: treasure_map::MAP_GUMP_SIZE,
    };

    session.send(RawPacket::s2c(encode_packet(&msg))).await?;
    session.send(RawPacket::s2c(MapPacket::clear_pins(clean_serial).to_bytes())).await?;
    Ok(true)
}

// ── digging tool double-click ────────────────────────────────────────────

/// Begin a treasure dig when a digging tool is double-clicked: show an object
/// target cursor asking which treasure map to use.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_digging_tool_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some((clean_serial, player_serial, world)) = parse_item_doubleclick(packet, ctx) else {
        return Ok(false);
    };

    let engine = game_util::engine_for(worker_tx, world);
    let Some(graphic) = resolve_item_graphic(&engine, player_serial, clean_serial).await else {
        return Ok(false);
    };
    if graphic != treasure_map::DIGGING_TOOL {
        return Ok(false);
    }

    // Block if a skill action / cursor is already active.
    let has_pending = ctx.has_pending_cursor();
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, has_pending, ctx.has_blocking_gump()) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Object target cursor (cursor_target = 0) for the treasure map.
    let cursor_id = TREASURE_CURSOR_BASE | (clean_serial & 0x0000_FFFF);
    let tc = object_cursor(cursor_id);

    ctx.infra.pending_cursor = Some(PendingCursor::treasure_select_map(
        cursor_id, player_serial, clean_serial,
    ));

    session.send(game_util::system_speech("Which treasure map will you dig with?")).await?;
    session.send(RawPacket::s2c(encode_packet(&tc))).await?;
    Ok(true)
}

// ── step 1: select the treasure map ──────────────────────────────────────

/// Handle the "select treasure map" target cursor response.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_treasure_select_map_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::TreasureSelectMap { user_serial, tool_serial } = pending.kind else {
        unreachable!("handle_treasure_select_map_target called with wrong cursor kind");
    };

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(tc) => tc,
        Err(_) => return Ok(true),
    };
    if common::dot_commands::is_target_cancelled(&tc) {
        return Ok(true);
    }

    let world = match ctx.infra.player.as_ref() {
        Some(p) => p.world,
        None => return Ok(true),
    };
    let map_serial = tc.target_serial & 0x7FFF_FFFF;
    if map_serial == 0 {
        session.send(game_util::system_message("That is not a treasure map.")).await?;
        return Ok(true);
    }

    let engine = game_util::engine_for(worker_tx, world);

    // Validate the targeted item is a treasure map.
    let graphic = resolve_item_graphic(&engine, user_serial, map_serial).await;
    if graphic != Some(treasure_map::TREASURE_MAP) {
        session.send(game_util::system_message("That is not a treasure map.")).await?;
        return Ok(true);
    }

    let Some(loc) = read_map_location(&engine, map_serial).await else {
        session.send(game_util::system_message("This map is blank.")).await?;
        return Ok(true);
    };
    let level = read_map_level(&engine, map_serial).await.unwrap_or(1);

    // Re-check the skill slot before showing the second cursor.
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, false, ctx.has_blocking_gump()) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Show a ground cursor for the dig tile.
    let cursor_id = TREASURE_CURSOR_BASE | (map_serial & 0x0000_FFFF);
    let ground = ground_cursor(cursor_id);

    ctx.infra.pending_cursor = Some(PendingCursor::treasure_dig_tile(
        cursor_id, user_serial, tool_serial, map_serial, loc.id, level,
    ));

    session.send(game_util::system_speech("Where do you wish to dig?")).await?;
    session.send(RawPacket::s2c(encode_packet(&ground))).await?;
    Ok(true)
}

// ── step 2: target the dig tile ──────────────────────────────────────────

/// Handle the "dig tile" target cursor response.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_treasure_dig_tile_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    skill_timer: &mut std::pin::Pin<Box<tokio::time::Sleep>>,
    session: &mut Session,
    _worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::TreasureDigTile { user_serial, tool_serial, map_serial, loc_id, level } = pending.kind else {
        unreachable!("handle_treasure_dig_tile_target called with wrong cursor kind");
    };

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(tc) => tc,
        Err(_) => return Ok(true),
    };
    if common::dot_commands::is_target_cancelled(&tc) {
        return Ok(true);
    }

    let p = match ctx.infra.player.as_ref() {
        Some(p) => p,
        None => return Ok(true),
    };
    let world = p.world;
    let (px, py) = (p.x, p.y);

    let Some(loc) = treasure_map::lookup_location(loc_id) else {
        return Ok(true);
    };

    // Player must be close enough to the chosen tile.
    if game_util::chebyshev(px, py, tc.x, tc.y) > treasure_map::PLAYER_DIG_RANGE {
        session.send(game_util::system_message("That is too far away.")).await?;
        return Ok(true);
    }

    // The chosen tile must match the buried location.
    if game_util::chebyshev(tc.x, tc.y, loc.x, loc.y) > treasure_map::DIG_RANGE {
        session.send(game_util::system_speech(
            "You dig for a while but find nothing here. The map must mean elsewhere.",
        )).await?;
        return Ok(true);
    }

    // Re-check the skill slot (this IS the resolving cursor → has_pending=false).
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, false, ctx.has_blocking_gump()) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Start the timed dig action.
    let delay = tokio::time::Duration::from_millis(treasure_map::DIG_DELAY_MS);
    let payload = ActionPayload::TreasureDig {
        user_serial,
        tool_serial,
        map_serial,
        level,
        target_x: tc.x,
        target_y: tc.y,
        target_z: tc.z,
        world,
    };
    let new_action = ActiveAction::new(ActionKind::SkillUse, delay, payload);
    skill_timer.as_mut().reset(new_action.completes_at);
    ctx.active_skill = Some(new_action);

    session.send(game_util::system_speech("You begin digging for treasure...")).await?;
    Ok(true)
}

// ── completion ───────────────────────────────────────────────────────────

/// Complete a treasure dig: spawn guardians + a filled chest, consume the map
/// and tool, and schedule the dig site for decay.
#[allow(clippy::too_many_arguments)]
pub(super) async fn complete_treasure_dig(
    user_serial: u32,
    tool_serial: u32,
    map_serial: u32,
    level: u8,
    target_x: u16,
    target_y: u16,
    target_z: i8,
    world: u8,
    serial_alloc: &std::sync::Arc<common::uo_engine::serial_alloc::SerialAllocator>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let Some(level_def) = treasure_map::lookup_level(level) else {
        return Ok(());
    };

    let engine = game_util::engine_for(worker_tx, world);

    // Re-check the player is still nearby.
    let (px, py, mounted) = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => {
            let mt = m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount);
            (m.x, m.y, mt)
        }
        None => return Ok(()),
    };
    if game_util::chebyshev(px, py, target_x, target_y) > treasure_map::PLAYER_DIG_RANGE {
        session.send(game_util::system_message("You move too far away to continue digging.")).await?;
        return Ok(());
    }

    // Dig feedback.
    game_util::send_resolved_animation(
        worker_tx, world, user_serial, crate::constants::anim::SWING_2H, mounted, 7, 1, px, py,
    ).await;
    game_util::send_sound(worker_tx, world, treasure_map::DIG_SOUND, target_x, target_y, target_z as i16).await;

    // Consume the digging tool + map.
    let _ = engine.consume_item(tool_serial, 1, Some(treasure_map::DIGGING_TOOL)).await;
    let _ = engine.consume_item(map_serial, 1, Some(treasure_map::TREASURE_MAP)).await;

    // Track everything we spawn so it can be decayed together.
    let mut decay_serials: Vec<u32> = Vec::new();

    // ── Spawn the treasure chest ──────────────────────────────────────
    let Some(chest_serial) = serial_alloc.alloc_item() else {
        warn!("[treasure] serial space exhausted — cannot spawn chest");
        return Ok(());
    };
    spawn_chest(&engine, chest_serial, target_x, target_y, target_z).await;
    engine.add_container_items(chest_serial, level_def.roll_loot()).await;
    decay_serials.push(chest_serial);

    // ── Spawn the guardians ───────────────────────────────────────────
    for (i, guardian) in level_def.guardians.iter().enumerate() {
        if let Some(serial) = spawn_guardian(
            &engine, serial_alloc, worker_tx, world, guardian, target_x, target_y, target_z, i,
        ).await {
            decay_serials.push(serial);
        }
    }

    // ── Schedule the dig site for decay ───────────────────────────────
    game_util::schedule_treasure_decay(worker_tx, world, decay_serials, level_def.decay_secs);

    session.send(game_util::system_speech(
        "You unearth a treasure chest! Guardians rise to defend it!",
    )).await?;
    info!(
        "[treasure] 0x{:08X} dug up chest 0x{:08X} at ({},{}) level {} ({} guardians)",
        user_serial, chest_serial, target_x, target_y, level, level_def.guardians.len(),
    );

    Ok(())
}

// ── Spawn helpers ──────────────────────────────────────────────────────────

/// Spawn a closed treasure chest as a ground container with a gump.
async fn spawn_chest(
    engine: &EngineProxy<DemoCommand>,
    chest_serial: u32,
    x: u16,
    y: u16,
    z: i8,
) {
    let chest = DemoEntity::Item {
        serial: chest_serial,
        graphic: treasure_map::TREASURE_CHEST,
        color: 0,
        amount: 1,
        x,
        y,
        z,
        is_container: true,
        hidden: false,
        facing: None,
    };
    engine.spawn_entity(chest_serial, chest).await;

    // Register the container so it can be opened (DrawContainer with gump model).
    // Use the legacy (7-byte) form — this goes to the engine's ingest path,
    // not to the client wire.
    let draw = DrawContainerLegacy {
        id: DrawContainer::ID,
        serial: chest_serial,
        gump_model: treasure_map::CHEST_GUMP,
    };
    engine.ingest_container(bytes::Bytes::from(encode_packet(&draw))).await;

    // Give the chest a name.
    engine.set_item_props(chest_serial, Some(ItemProps::with_name("a treasure chest"))).await;
}

/// Spawn a single guardian monster near the dig site and attach its AI.
///
/// Returns the spawned serial on success.
#[allow(clippy::too_many_arguments)]
async fn spawn_guardian(
    engine: &EngineProxy<DemoCommand>,
    _serial_alloc: &std::sync::Arc<common::uo_engine::serial_alloc::SerialAllocator>,
    worker_tx: &DemoWorkerTx,
    world: u8,
    guardian: &GuardianDef,
    x: u16,
    y: u16,
    z: i8,
    index: usize,
) -> Option<u32> {
    // Scatter guardians around the dig point.
    let (dx, dy) = match index % 4 {
        0 => (1i32, 0i32),
        1 => (-1, 0),
        2 => (0, 1),
        _ => (0, -1),
    };
    let nx = (x as i32 + dx).clamp(0, u16::MAX as i32) as u16;
    let ny = (y as i32 + dy).clamp(0, u16::MAX as i32) as u16;
    let nz = engine.resolve_z(nx, ny, z, Heading::South).await.unwrap_or(z);

    let serial = engine.allocate_mobile_serial().await;
    if serial == 0 {
        warn!("[treasure] mobile serial space exhausted — cannot spawn guardian");
        return None;
    }

    let npc = DemoEntity::Mobile(MobileData {
        serial,
        graphic: guardian.graphic,
        x: nx,
        y: ny,
        z: nz,
        direction: 0,
        color: 0,
        status: MobileFlags(0),
        notoriety: Notoriety::Attackable,
        items: Vec::new(),
        name: guardian.name.to_string(),
        hits: guardian.hits,
        hits_max: guardian.hits,
        mana: 0,
        mana_max: 0,
        stamina: 100,
        stamina_max: 100,
        str_: guardian.str_,
        dex: guardian.dex,
        int: guardian.int_,
        is_player: false,
        dead: false,
        living_graphic: 0,
        noto_class: NotorietyClass::Murderer,
        ..Default::default()
    });
    engine.spawn_entity(serial, npc).await;

    // Attach the aggressive monster AI controller.
    let cfg = MonsterCfg {
        aggro_range: guardian.aggro_range,
        leash_range: guardian.leash_range,
        damage_min: guardian.damage_min,
        damage_max: guardian.damage_max,
        swing_delay_ms: guardian.swing_delay_ms,
    };
    let controller = Box::new(MonsterController::new(cfg));
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        world,
        DemoCommand::AttachControllerPersist {
            serial,
            controller,
            controller_id: cfg.controller_id(),
        },
    )).await;

    Some(serial)
}

// ── Misc helpers ───────────────────────────────────────────────────────────

/// Parse a non-paperdoll item double-click, returning `(clean_serial,
/// player_serial, world)`.  Returns `None` if it's a paperdoll request, an
/// unparsable packet, or there's no player.
fn parse_item_doubleclick(
    packet: &RawPacket,
    ctx: &SessionContext,
) -> Option<(u32, u32, u8)> {
    if packet.id() != DoubleClick::ID {
        return None;
    }
    let dc = DoubleClick::from_bytes(&packet.data).ok()?;
    if dc.serial & 0x8000_0000 != 0 {
        return None; // paperdoll
    }
    let clean_serial = dc.serial & 0x7FFF_FFFF;
    let p = ctx.infra.player.as_ref()?;
    Some((clean_serial, p.serial, p.world))
}

/// Build a neutral object-target cursor.
fn object_cursor(cursor_id: u32) -> TargetCursor {
    TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 0, // object target
        cursor_id,
        cursor_type: 0,
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    }
}

/// Build a neutral ground-target cursor.
fn ground_cursor(cursor_id: u32) -> TargetCursor {
    TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 1, // ground target
        cursor_id,
        cursor_type: 0,
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    }
}

/// Read the buried treasure location from a map item's meta.
async fn read_map_location(
    engine: &EngineProxy<DemoCommand>,
    map_serial: u32,
) -> Option<&'static treasure_map::TreasureLocation> {
    let props = engine.get_item_props(map_serial).await?;
    let loc_id = props.get_meta_int(treasure_map::META_TREASURE_LOC)? as u32;
    treasure_map::lookup_location(loc_id)
}

/// Read the treasure level from a map item's meta.
async fn read_map_level(
    engine: &EngineProxy<DemoCommand>,
    map_serial: u32,
) -> Option<u8> {
    let props = engine.get_item_props(map_serial).await?;
    Some(props.get_meta_int(treasure_map::META_TREASURE_LEVEL)? as u8)
}

/// Resolve a clicked item's graphic (container/backpack → equipped → ground).
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
