//! Player ship (boat) placement, re-deeding, and sailing.
//!
//! ## Overview
//!
//! A ship is a standard multi (from `multi.mul`) placed on **water** via a
//! **ship deed**.  It reuses the same world representation as a house — a
//! [`DemoEntity::Multi`] carrying an `owner` — but is distinguished from a
//! house by its multi graphic (looked up in the [`crate::ships`] catalogue).
//!
//! ## Flows
//!
//! 1. **Placement** — double-click deed → water target cursor →
//!    [`handle_placement_target`] validates the footprint (all water, clear of
//!    statics) and spawns the ship multi, facing North.
//! 2. **Re-deed** — double-click the ship → [`handle_ship_double_click`].  If
//!    the owner is double-clicking and **no mobile is standing on the deck**,
//!    the ship is removed and a fresh deed is returned to the owner's pack.
//! 3. **Sailing** — speech commands ("forward", "stop", "turn left", etc.)
//!    from the owner start/stop/turn the ship.  State is stored in
//!    `item_props.meta` on the ship serial and driven by a periodic tick.

use log::info;

use protocol::RawPacket;
use packets::interaction::{DeleteObject, TargetCursor};
use packets::traits::{encode_packet, BasicPacket};

use network::error;
use network::session::Session;

use common::dot_commands as dot_cmd;
use common::uo_engine::item_props::MetaValue;
use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::{DropTarget, HeldItemInfo, ShipTerrainResult};
use framework::ecumene::Entity as EngineEntity;

use crate::ships::{self, ShipHeading};
use crate::game_util;
use crate::DemoWorkerTx;

use super::game_logic::InfraState;
use super::pending_cursor::PendingCursor;

// ── Deed double-click → placement cursor ───────────────────────────────────

/// Handle a double-click on a possible ship deed.
///
/// Returns `Ok(true)` if the deed was recognised and a placement cursor was
/// sent (caller should treat the packet as consumed).  Returns `Ok(false)`
/// if the double-clicked item is not a ship deed.
pub(super) async fn handle_deed_double_click(
    serial: u32,
    infra: &mut InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some(player) = infra.player.as_ref() else {
        return Ok(false);
    };

    // Resolve the item's graphic (deeds live in the backpack as items).
    let engine = game_util::engine_for(worker_tx, player.world);
    let graphic = match engine.find_item_info(serial).await {
        Some((_, g, _, _)) => g,
        None => return Ok(false),
    };

    let Some(def) = ships::lookup_by_deed(graphic) else {
        return Ok(false);
    };

    // Ships are placed facing North; that multi id drives the placement
    // preview and is echoed back so we can resolve the def again.
    let north_multi = def.multi_id_for(ShipHeading::North);

    info!(
        "[ship] deed {:#010X} ({}) double-clicked — sending placement preview (0x99)",
        serial, def.name
    );

    use packets::interaction::MultiPlacement;
    let preview = MultiPlacement::server_request(serial, north_multi);
    session
        .send(RawPacket::s2c(encode_packet(&preview)))
        .await?;

    infra.pending_cursor = Some(PendingCursor::ship_placement(
        serial, north_multi, serial,
    ));

    Ok(true)
}

// ── Placement cursor response ───────────────────────────────────────────────

/// Dispatch a validated [`CursorKind::ShipPlacement`](super::pending_cursor::CursorKind::ShipPlacement) target response.
pub(super) async fn handle_placement_cursor(
    raw: &RawPacket,
    cursor_id: u32,
    multi_id: u16,
    deed_serial: u32,
    infra: &mut InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    if let Ok(tc) = TargetCursor::from_bytes(&raw.data) {
        if tc.cursor_id == cursor_id {
            handle_placement_target(&tc, multi_id, deed_serial, infra, session, worker_tx).await?;
        }
    }
    Ok(())
}

/// Handle the water-target response for a pending ship placement.
///
/// Validates the footprint, and on success spawns the ship multi (facing
/// North), consumes the deed.
async fn handle_placement_target(
    tc: &TargetCursor,
    multi_id: u16,
    deed_serial: u32,
    infra: &mut InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    if dot_cmd::is_target_cancelled(tc) {
        info!("[ship] placement cancelled");
        return Ok(());
    }

    let Some(player) = infra.player.as_ref() else {
        return Ok(());
    };
    let world = player.world;
    let owner = player.serial;

    // Locate the ship def by its (North) multi id.
    let Some(def) = ships::lookup_by_multi(multi_id) else {
        session.send(game_util::system_message("Unknown ship type.")).await?;
        return Ok(());
    };

    let ox = tc.x;
    let oy = tc.y;

    // Resolve the ship's geometry from the data files (footprint + deck Z).
    let Some(static_data) = infra.static_data.clone() else {
        session
            .send(game_util::system_message("Ship placement is not available in this area."))
            .await?;
        return Ok(());
    };
    let north_multi = def.multi_id_for(ShipHeading::North);
    let Some(shape) = ships::ShipShape::from_static(north_multi, static_data.as_ref()) else {
        session.send(game_util::system_message("Unknown ship type.")).await?;
        return Ok(());
    };

    let engine = game_util::engine_for(worker_tx, world);

    // ── Validate the footprint (entities + water terrain) ──────────────
    let water_z = match validate_placement(&shape, ox, oy, &engine).await {
        Ok(z) => z,
        Err(reason) => {
            session.send(game_util::system_message(reason)).await?;
            return Ok(());
        }
    };

    // Place the multi origin at the water surface; the deck then stands at
    // `water_z + shape.deck_rel_z` (e.g. -5 + 3 = -2), so a mobile can walk
    // on the deck at the expected Z.
    let oz = water_z;

    // ── Allocate the multi serial ──────────────────────────────────────
    let multi_serial = engine.allocate_serial().await;
    if multi_serial == 0 {
        session.send(game_util::system_message("Serial space exhausted.")).await?;
        return Ok(());
    }

    // ── Allocate child component serials ───────────────────────────────
    //
    // Layout: `door_serials = [port_plank, starboard_plank, hold]`,
    // `sign_serial = tillerman`.  A `0` serial means allocation failed and
    // that component is skipped.
    let port_serial = engine.allocate_serial().await;
    let star_serial = engine.allocate_serial().await;
    let hold_serial = engine.allocate_serial().await;
    let tiller_serial = engine.allocate_serial().await;

    let mut door_serials = Vec::with_capacity(3);
    for s in [port_serial, star_serial, hold_serial] {
        if s != 0 {
            door_serials.push(s);
        }
    }

    // ── Spawn the ship multi (a Multi carrying ownership + components) ──
    let ship = DemoEntity::Multi {
        serial: multi_serial,
        graphic: north_multi,
        x: ox,
        y: oy,
        z: oz,
        owner,
        door_serials,
        sign_serial: tiller_serial,
    };
    engine.spawn_entity(multi_serial, ship).await;

    // Record the ship's heading (North on placement) so turns can resolve
    // per-heading child graphics.
    {
        let mut props = engine.get_item_props(multi_serial).await.unwrap_or_default();
        props.set_meta(ships::META_SHIP_HEADING, MetaValue::Int(ShipHeading::North.index() as i64));
        engine.set_item_props(multi_serial, Some(props)).await;
    }

    // ── Spawn the components (tillerman, planks, hold) facing North ────
    spawn_ship_components(
        def, multi_serial, ox, oy, oz,
        port_serial, star_serial, hold_serial, tiller_serial,
        &engine,
    ).await;

    // ── Consume the deed ───────────────────────────────────────────────
    let _ = engine.consume_item(deed_serial, 1, None).await;
    session
        .send(RawPacket::s2c(encode_packet(&DeleteObject {
            id: DeleteObject::ID,
            serial: deed_serial,
        })))
        .await?;

    let deck_z = oz.saturating_add(shape.deck_rel_z);

    info!(
        "[ship] placed {} (multi {:#010X}) at ({},{},{}) owner={:#010X} \
         (deck stands at z={})",
        def.name, multi_serial, ox, oy, oz, owner, deck_z,
    );

    // Teleport the player onto the ship deck so they can immediately walk
    // on it (otherwise they'd be stranded in the water).
    engine.teleport(owner, ox, oy, deck_z, None).await;

    session
        .send(game_util::system_message_gray(&format!(
            "You have placed {}.",
            def.name
        )))
        .await?;

    Ok(())
}

/// Validate that a ship footprint (described by `shape`) at `(ox, oy)` is
/// placeable.
///
/// Checks two independent layers:
///
/// 1. **Dynamic entities** — no other multi or item may overlap the footprint
///    (mobiles are allowed).
/// 2. **Terrain** — every footprint tile must be open water, clear of
///    blocking statics.
///
/// On success returns `Ok(water_z)`, the water surface height the ship sits
/// at.  On failure returns `Err(reason)` with a player-facing message.
async fn validate_placement(
    shape: &ships::ShipShape,
    ox: u16,
    oy: u16,
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
) -> Result<i8, &'static str> {
    let rect = footprint_rect(shape, ox, oy);

    // ── 1. Dynamic entities ────────────────────────────────────────────
    let entities = engine.query_area(rect).await;
    for e in &entities {
        match e {
            DemoEntity::Multi { .. } => {
                return Err("That location is blocked by another structure.");
            }
            DemoEntity::Item { .. } => {
                return Err("The water must be clear of items to place a ship.");
            }
            DemoEntity::Mobile(_) => {}
        }
    }

    // ── 2. Terrain (must be water) ─────────────────────────────────────
    match engine.validate_ship_footprint(rect).await {
        ShipTerrainResult::Ok { water_z } => Ok(water_z),
        ShipTerrainResult::NotWater => {
            Err("Ships can only be placed on water.")
        }
        ShipTerrainResult::Blocked => {
            Err("The water here is too obstructed to place a ship.")
        }
        ShipTerrainResult::OutOfBounds => {
            Err("You cannot place a ship there.")
        }
        ShipTerrainResult::NoData => {
            Err("Ship placement is not available in this area.")
        }
    }
}

/// Compute the world-space footprint rectangle for a ship `shape` at
/// `(ox, oy)`.
fn footprint_rect(
    shape: &ships::ShipShape,
    ox: u16,
    oy: u16,
) -> framework::ecumene::TileRect {
    use framework::ecumene::TileRect;
    TileRect {
        x_min: (ox as i32 + shape.foot_x_min as i32).max(0) as u16,
        y_min: (oy as i32 + shape.foot_y_min as i32).max(0) as u16,
        x_max: (ox as i32 + shape.foot_x_max as i32).max(0) as u16,
        y_max: (oy as i32 + shape.foot_y_max as i32).max(0) as u16,
    }
}

// ── Ship component spawning ──────────────────────────────────────────────────

/// Spawn the tillerman, two planks, and the cargo hold for a freshly placed
/// ship (facing North).
///
/// Each component is an item entity tagged with `META_SHIP_SERIAL`,
/// `META_SHIP_ROLE`, and per-heading graphics (`META_SHIP_GFX_*`) so the
/// engine can carry and re-orient it on move / turn.  The hold is additionally
/// registered as a container.
#[allow(clippy::too_many_arguments)]
async fn spawn_ship_components(
    def: &ships::ShipDef,
    multi_serial: u32,
    ox: u16,
    oy: u16,
    oz: i8,
    port_serial: u32,
    star_serial: u32,
    hold_serial: u32,
    tiller_serial: u32,
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
) {
    use ships::ShipHeading;

    let north = ShipHeading::North;

    // (serial, role, component def, is_container)
    let specs: [(u32, &str, &ships::ComponentDef, bool); 4] = [
        (tiller_serial, ships::ROLE_TILLER, &def.tillerman, false),
        (port_serial, ships::ROLE_PLANK_PORT, &def.plank_port, false),
        (star_serial, ships::ROLE_PLANK_STAR, &def.plank_star, false),
        (hold_serial, ships::ROLE_HOLD, &def.hold, true),
    ];

    for (serial, role, comp, is_container) in specs {
        if serial == 0 {
            continue;
        }
        let (dx, dy) = comp.offset(north);
        let cx = (ox as i32 + dx as i32).max(0) as u16;
        let cy = (oy as i32 + dy as i32).max(0) as u16;
        let cz = oz.saturating_add(comp.dz);
        let graphic = comp.graphic(north);

        let item = DemoEntity::Item {
            serial,
            graphic,
            color: 0,
            amount: 1,
            x: cx,
            y: cy,
            z: cz,
            is_container,
            hidden: false,
            facing: None,
        };
        engine.spawn_entity(serial, item).await;

        // Tag the component with its parent ship, role, and per-heading art.
        let mut props = engine.get_item_props(serial).await.unwrap_or_default();
        props.set_meta(ships::META_SHIP_SERIAL, MetaValue::Int(multi_serial as i64));
        props.set_meta(ships::META_SHIP_ROLE, MetaValue::Str(role.to_string()));
        for h in ShipHeading::ALL {
            props.set_meta(
                ships::gfx_meta_key(h),
                MetaValue::Int(comp.graphic(h) as i64),
            );
        }
        let name = match role {
            r if r == ships::ROLE_TILLER => Some("the tillerman"),
            r if r == ships::ROLE_HOLD => Some("the hold"),
            _ => Some("a gangplank"),
        };
        if let Some(n) = name {
            props.set_name(n);
        }
        engine.set_item_props(serial, Some(props)).await;

        // Register the hold as an openable container.
        if is_container {
            use packets::interaction::DrawContainerLegacy;
            let draw = DrawContainerLegacy {
                id: DrawContainerLegacy::ID,
                serial,
                gump_model: 0x4C, // small wooden chest gump
            };
            engine.ingest_container(bytes::Bytes::from(encode_packet(&draw))).await;
        }
    }
}

// ── Ship component double-click (plank toggle / hold open) ───────────────────

/// Handle a double-click on a ship component (tillerman, plank, or hold).
///
/// - **Plank** → toggle open/closed (graphic swap on the same tile, plus a
///   door sound).  Boarding/disembarking is a later step.
/// - **Hold** → returns `Ok(false)` so the click falls through to the generic
///   container-open path in `interaction.rs`.
/// - **Tillerman** → currently inert (sailing is driven by speech commands);
///   returns `Ok(true)` to swallow the click.
///
/// Returns `Ok(false)` if the serial is not a ship component (the dispatcher
/// then continues to other handlers / infra).
pub(super) async fn handle_component_double_click(
    serial: u32,
    infra: &InfraState,
    _session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    use crate::planks;

    let Some(player) = infra.player.as_ref() else {
        return Ok(false);
    };
    let world = player.world;
    let engine = game_util::engine_for(worker_tx, world);

    // Read the clicked item's props; a ship component carries META_SHIP_ROLE.
    let Some(props) = engine.get_item_props(serial).await else {
        return Ok(false);
    };
    let role = match props.get_meta_str(ships::META_SHIP_ROLE) {
        Some(r) => r.to_string(),
        None => return Ok(false),
    };

    // The hold opens via the generic container path — let it fall through.
    if role == ships::ROLE_HOLD {
        return Ok(false);
    }

    // Tillerman: no double-click action yet (commands are spoken).
    if role == ships::ROLE_TILLER {
        return Ok(true);
    }

    // Otherwise it must be a plank — toggle open/closed.
    if !planks::is_plank_role(Some(role.as_str())) {
        return Ok(true);
    }

    let Some(item) = engine.get_entity(serial).await else {
        return Ok(true);
    };
    let (graphic, x, y, z, color, amount, is_container, hidden, facing) =
        match item.item() {
            Some(i) => (
                i.graphic, i.x, i.y, i.z, i.color, i.amount,
                i.is_container, i.hidden, i.facing,
            ),
            None => return Ok(true),
        };

    let state = planks::classify(graphic);
    let new_graphic = planks::toggle_target(graphic);
    let opening = !state.is_open;

    let updated = DemoEntity::Item {
        serial,
        graphic: new_graphic,
        color,
        amount,
        x,
        y,
        z,
        is_container,
        hidden,
        facing,
    };
    engine.update_entity(serial, updated).await;

    let snd = if opening {
        crate::constants::sound::DOOR_OPEN
    } else {
        crate::constants::sound::DOOR_CLOSE
    };
    game_util::send_sound(worker_tx, world, snd, x, y, z as i16).await;

    Ok(true)
}

// ── Ship double-click → re-deed ─────────────────────────────────────────────
/// Handle a double-click on a possible ship multi (re-deed it).
///
/// Returns `Ok(true)` if the clicked serial is a ship and the click was
/// handled (re-deeded, or rejected with a message).  Returns `Ok(false)` if
/// the serial is not a ship.
pub(super) async fn handle_ship_double_click(
    serial: u32,
    infra: &InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some(player) = infra.player.as_ref() else {
        return Ok(false);
    };
    let world = player.world;
    let viewer = player.serial;

    let engine = game_util::engine_for(worker_tx, world);

    // Resolve the clicked entity; it must be a Multi whose graphic is a ship.
    let Some(entity) = engine.get_entity(serial).await else {
        return Ok(false);
    };
    let (graphic, x, y, owner, child_serials) = match &entity {
        DemoEntity::Multi { graphic, x, y, owner, door_serials, sign_serial, .. } => {
            let mut children = door_serials.clone();
            if *sign_serial != 0 {
                children.push(*sign_serial);
            }
            (*graphic, *x, *y, *owner, children)
        }
        _ => return Ok(false),
    };
    let Some(def) = ships::lookup_by_multi(graphic) else {
        // A Multi but not a ship (probably a house) — not ours.
        return Ok(false);
    };

    // Only the owner may pack up the ship.
    if owner != viewer {
        session.send(game_util::system_message("That is not your ship.")).await?;
        return Ok(true);
    }

    // Resolve the ship's footprint from the data files for the current
    // facing (the graphic already encodes the heading; each facing is a
    // distinct multi id with its own bounding box).
    let Some(static_data) = infra.static_data.clone() else {
        session.send(game_util::system_message("That is not your ship.")).await?;
        return Ok(true);
    };
    let Some(shape) = ships::ShipShape::from_static(graphic, static_data.as_ref()) else {
        return Ok(false);
    };
    let rect = footprint_rect(&shape, x, y);

    // Reject re-deed if anyone is standing on the deck.
    let occupants = engine.query_area(rect).await;
    let has_mobile = occupants.iter().any(|e| matches!(e, DemoEntity::Mobile(_)));
    if has_mobile {
        session
            .send(game_util::system_message(
                "You cannot pack up the ship while someone is aboard.",
            ))
            .await?;
        return Ok(true);
    }

    info!("[ship] re-deeding {:#010X} (owner {:#010X})", serial, owner);

    // Remove the ship's components (tillerman, planks, hold) first.
    for child in &child_serials {
        engine.remove_entity(*child).await;
        session
            .send(RawPacket::s2c(encode_packet(&DeleteObject {
                id: DeleteObject::ID,
                serial: *child,
            })))
            .await?;
    }

    // Remove the ship multi.
    engine.remove_entity(serial).await;
    session
        .send(RawPacket::s2c(encode_packet(&DeleteObject {
            id: DeleteObject::ID,
            serial,
        })))
        .await?;

    // Return a fresh deed to the owner's backpack.
    return_deed(viewer, world, def.deed_graphic, worker_tx).await;

    session
        .send(game_util::system_message_gray(
            "You pack up the ship. A ship deed has been placed in your backpack.",
        ))
        .await?;

    Ok(true)
}

/// Drop a fresh ship deed into a player's backpack.
async fn return_deed(
    player_serial: u32,
    world: u8,
    deed_graphic: u16,
    worker_tx: &DemoWorkerTx,
) {
    let engine = game_util::engine_for(worker_tx, world);

    let bp_serial = match engine.get_entity(player_serial).await {
        Some(e) => match e.backpack_serial() {
            Some(s) => s,
            None => return,
        },
        None => return,
    };

    let serial = engine.allocate_serial().await;
    if serial == 0 {
        return;
    }

    let item = HeldItemInfo {
        serial,
        graphic: deed_graphic,
        color: 0,
        amount: 1,
    };
    let target = DropTarget::OnEntity {
        target_serial: bp_serial,
        x: 0xFFFF,
        y: 0xFFFF,
    };
    let _ = engine.drop_item(player_serial, item, target, None).await;
}

// ── Sailing: meta keys ──────────────────────────────────────────────────────

/// Meta key: current sail heading (`"north"`, `"east"`, `"south"`, `"west"`).
/// Absent or empty = ship is stopped.
pub const META_SAIL_HEADING: &str = "sail_heading";

/// Meta key: the *scheduled* time (monotonic millis from process start,
/// see `sail_clock_now_ms` in `main.rs`) of the last movement tick (i64).
///
/// This is the scheduled cadence anchor, **not** the wall-clock instant the
/// move actually executed, so the per-tile interval stays constant even when
/// the worker wakes late.
pub const META_SAIL_LAST_MOVE: &str = "sail_last_move";

/// Interval between movement ticks in milliseconds.
pub const SAIL_TICK_MS: i64 = 500;

// ── Sailing: speech commands ────────────────────────────────────────────────

/// Recognised ship speech commands.
enum SailCommand {
    /// Start sailing in the ship's current heading.
    Forward,
    /// Start sailing backwards (opposite heading).
    Backward,
    /// Stop the ship.
    Stop,
    /// Turn 90° to port.
    TurnLeft,
    /// Turn 90° to starboard.
    TurnRight,
    /// Sail in an explicit cardinal direction.
    SetHeading(ShipHeading),
}

impl SailCommand {
    fn parse(text: &str) -> Option<SailCommand> {
        match text {
            "forward" | "unfurl sail"  => Some(SailCommand::Forward),
            "backward" | "backwards"   => Some(SailCommand::Backward),
            "stop" | "furl sail"       => Some(SailCommand::Stop),
            "turn left" | "port"       => Some(SailCommand::TurnLeft),
            "turn right" | "starboard" => Some(SailCommand::TurnRight),
            _ => {
                // "north", "east", "south", "west"
                ShipHeading::from_keyword(text).map(SailCommand::SetHeading)
            }
        }
    }
}

/// Handle a player speech packet that may be a ship command.
///
/// If the speech text matches a sailing keyword and the player owns a
/// ship within earshot, the command is applied.  Returns `Ok(true)` if
/// the speech was a ship command (even if it was rejected).
pub(super) async fn maybe_ship_command(
    packet: &protocol::RawPacket,
    infra: &InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some(text) = common::dot_commands::extract_speech_text(packet) else {
        return Ok(false);
    };
    let kw = text.trim().to_ascii_lowercase();
    let Some(cmd) = SailCommand::parse(&kw) else {
        return Ok(false);
    };

    let Some(player) = infra.player.as_ref() else {
        return Ok(false);
    };
    let world = player.world;
    let owner = player.serial;
    let engine = game_util::engine_for(worker_tx, world);

    // Find the player's ship (within a generous range — the player should
    // be on the deck, but allow some slack for standing near the edge).
    const SHIP_COMMAND_RANGE: u16 = 18;
    let area = framework::ecumene::TileRect::from_view(player.x, player.y, SHIP_COMMAND_RANGE);
    let entities = engine.query_area(area).await;

    let ship = entities.iter().find(|e| {
        if let DemoEntity::Multi { owner: o, graphic, .. } = e {
            *o == owner && ships::lookup_by_multi(*graphic).is_some()
        } else {
            false
        }
    });
    let Some(DemoEntity::Multi { serial: ship_serial, graphic, .. }) = ship else {
        session.send(game_util::system_message("You do not have a ship nearby.")).await?;
        return Ok(true);
    };
    let ship_serial = *ship_serial;
    let graphic = *graphic;

    let Some(def) = ships::lookup_by_multi(graphic) else {
        return Ok(true);
    };
    let current_heading = ShipHeading::from_multi_graphic(graphic)
        .unwrap_or(ShipHeading::North);

    use common::uo_engine::item_props::MetaValue;

    match cmd {
        SailCommand::Forward => {
            let heading_str = heading_to_str(current_heading);
            let mut props = engine.get_item_props(ship_serial).await.unwrap_or_default();
            props.meta.insert(META_SAIL_HEADING.to_string(), MetaValue::Str(heading_str.to_string()));
            engine.set_item_props(ship_serial, Some(props)).await;
            session.send(game_util::system_message_gray("Aye, sir! Forward!")).await?;
        }
        SailCommand::Backward => {
            let back = match current_heading {
                ShipHeading::North => ShipHeading::South,
                ShipHeading::South => ShipHeading::North,
                ShipHeading::East  => ShipHeading::West,
                ShipHeading::West  => ShipHeading::East,
            };
            let heading_str = heading_to_str(back);
            let mut props = engine.get_item_props(ship_serial).await.unwrap_or_default();
            props.meta.insert(META_SAIL_HEADING.to_string(), MetaValue::Str(heading_str.to_string()));
            engine.set_item_props(ship_serial, Some(props)).await;
            session.send(game_util::system_message_gray("Aye, sir! Backward!")).await?;
        }
        SailCommand::Stop => {
            let mut props = engine.get_item_props(ship_serial).await.unwrap_or_default();
            props.meta.remove(META_SAIL_HEADING);
            props.meta.remove(META_SAIL_LAST_MOVE);
            engine.set_item_props(ship_serial, Some(props)).await;
            session.send(game_util::system_message_gray("Aye, sir! Stopping!")).await?;
        }
        SailCommand::TurnLeft => {
            let new_heading = current_heading.turn_left();
            let new_gfx = def.multi_id_for(new_heading);
            match engine.turn_ship(ship_serial, new_gfx, -1).await {
                Ok(_) => {
                    session.send(game_util::system_message_gray("Aye, sir! Turning port!")).await?;
                }
                Err(reason) => {
                    session.send(game_util::system_message(&reason)).await?;
                }
            }
        }
        SailCommand::TurnRight => {
            let new_heading = current_heading.turn_right();
            let new_gfx = def.multi_id_for(new_heading);
            match engine.turn_ship(ship_serial, new_gfx, 1).await {
                Ok(_) => {
                    session.send(game_util::system_message_gray("Aye, sir! Turning starboard!")).await?;
                }
                Err(reason) => {
                    session.send(game_util::system_message(&reason)).await?;
                }
            }
        }
        SailCommand::SetHeading(heading) => {
            // Turn the ship to face the requested heading, then start moving.
            let new_gfx = def.multi_id_for(heading);
            if new_gfx != graphic {
                // Clockwise quarter-turns from the current facing to the new
                // one (heading indices advance clockwise N→E→S→W).
                let quarter_turns_cw =
                    (((heading.index() as i32 - current_heading.index() as i32) % 4 + 4) % 4) as i8;
                if let Err(reason) = engine.turn_ship(ship_serial, new_gfx, quarter_turns_cw).await {
                    session.send(game_util::system_message(&reason)).await?;
                    return Ok(true);
                }
            }
            let heading_str = heading_to_str(heading);
            let mut props = engine.get_item_props(ship_serial).await.unwrap_or_default();
            props.meta.insert(META_SAIL_HEADING.to_string(), MetaValue::Str(heading_str.to_string()));
            engine.set_item_props(ship_serial, Some(props)).await;
            session.send(game_util::system_message_gray(
                &format!("Aye, sir! Heading {}!", heading_str),
            )).await?;
        }
    }

    Ok(true)
}

fn heading_to_str(h: ShipHeading) -> &'static str {
    match h {
        ShipHeading::North => "north",
        ShipHeading::East  => "east",
        ShipHeading::South => "south",
        ShipHeading::West  => "west",
    }
}
