//! Player house placement, ownership, management and demolition.
//!
//! ## Overview
//!
//! A house is a standard multi (from `multi.mul`) placed via a **house deed**.
//! On placement the server spawns:
//!
//! - the multi itself (a [`DemoEntity::Multi`] carrying `owner` + child serials),
//! - one or more doors (regular items, toggled open/closed by the owner),
//! - a sign (a regular item that opens the management gump on double-click).
//!
//! Ownership is intentionally minimal for this first cut: a single `owner`
//! serial.  There are no co-owners, friends, or public/private modes.
//!
//! ## Flows
//!
//! 1. **Placement** — double-click deed → ground target cursor →
//!    [`handle_placement_target`] validates the footprint and spawns the house.
//! 2. **Management** — double-click the sign → [`handle_sign_double_click`] sends a
//!    management gump.  [`handle_house_gump`] handles its buttons (Demolish).
//! 3. **Doors** — double-click a door → [`handle_door_double_click`] toggles
//!    the door graphic (owner only).

use log::info;

use protocol::RawPacket;
use packets::interaction::{DeleteObject, TargetCursor};
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use common::dot_commands as dot_cmd;
use common::uo_engine::entity::DemoEntity;
use common::uo_engine::item_props::MetaValue;
use common::uo_engine::handler::{DropTarget, HeldItemInfo};
use framework::ecumene::Entity as EngineEntity;

use crate::constants::sound;
use crate::houses::{self, HouseDef};
use crate::game_util;
use crate::{DemoWorkerTx};

use super::game_logic::InfraState;
use super::pending_cursor::PendingCursor;

/// Gump id for the house-management dialog.
const HOUSE_GUMP_ID: u32 = 0x484F_5547; // "HOUG"

// ── Deed double-click → placement cursor ───────────────────────────────────

/// Handle a double-click on a possible house deed.
///
/// Returns `Ok(true)` if the deed was recognised and a placement cursor was
/// sent (caller should treat the packet as consumed).  Returns `Ok(false)`
/// if the double-clicked item is not a house deed.
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

    let Some(def) = houses::lookup_by_deed(graphic) else {
        return Ok(false);
    };

    info!(
        "[house] deed {:#010X} ({}) double-clicked — sending placement preview (0x99)",
        serial, def.name
    );

    // Send a 0x99 MultiPlacement server-request: the client shows a moving
    // house outline at the cursor and opens a multi target cursor.  The
    // client echoes the deed serial back as the `cursor_id` of its 0x6C
    // response, so we track the pending cursor under the deed serial.
    use packets::interaction::MultiPlacement;
    let preview = MultiPlacement::server_request(serial, def.multi_id);
    session
        .send(RawPacket::s2c(encode_packet(&preview)))
        .await?;

    infra.pending_cursor = Some(PendingCursor::house_placement(
        serial, def.multi_id, serial,
    ));

    Ok(true)
}

// ── Placement cursor response ───────────────────────────────────────────────

/// Handle the ground-target response for a pending house placement.
///
/// Validates the footprint, and on success spawns the multi + doors + sign,
/// consumes the deed, and streams the new entities to the placing client.
pub(super) async fn handle_placement_target(
    tc: &TargetCursor,
    multi_id: u16,
    deed_serial: u32,
    infra: &mut InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    if dot_cmd::is_target_cancelled(tc) {
        info!("[house] placement cancelled");
        return Ok(());
    }

    let Some(player) = infra.player.as_ref() else {
        return Ok(());
    };
    let world = player.world;
    let owner = player.serial;

    // Locate the house def by multi id.
    let Some(def) = houses::all().iter().find(|h| h.multi_id == multi_id) else {
        session.send(game_util::system_message("Unknown house type.")).await?;
        return Ok(());
    };

    let ox = tc.x;
    let oy = tc.y;

    let engine = game_util::engine_for(worker_tx, world);

    // ── Validate the footprint (entities + terrain) ────────────────────
    let oz = match validate_placement(def, ox, oy, &engine).await {
        Ok(foundation_z) => foundation_z,
        Err(reason) => {
            session.send(game_util::system_message(reason)).await?;
            return Ok(());
        }
    };

    // ── Allocate serials ───────────────────────────────────────────────
    let multi_serial = engine.allocate_serial().await;
    if multi_serial == 0 {
        session.send(game_util::system_message("Serial space exhausted.")).await?;
        return Ok(());
    }

    let mut door_serials = Vec::with_capacity(def.doors.len());
    for _ in def.doors {
        let s = engine.allocate_serial().await;
        if s != 0 {
            door_serials.push(s);
        }
    }
    let sign_serial = engine.allocate_serial().await;

    // ── Spawn the multi (carrying ownership + child serials) ───────────
    let multi = DemoEntity::Multi {
        serial: multi_serial,
        graphic: multi_id,
        x: ox,
        y: oy,
        z: oz,
        owner,
        door_serials: door_serials.clone(),
        sign_serial,
    };
    engine.spawn_entity(multi_serial, multi.clone()).await;

    // ── Spawn doors (closed) ───────────────────────────────────────────
    for (door_def, &dserial) in def.doors.iter().zip(door_serials.iter()) {
        let dx = (ox as i32 + door_def.dx as i32) as u16;
        let dy = (oy as i32 + door_def.dy as i32) as u16;
        let dz = oz.saturating_add(door_def.dz);
        let door = DemoEntity::Item {
            serial: dserial,
            graphic: door_def.closed,
            color: 0,
            amount: 1,
            x: dx,
            y: dy,
            z: dz,
            is_container: false,
            hidden: false,
            facing: None,
        };
        engine.spawn_entity(dserial, door).await;

        // Tag the door with its parent house so a door click can resolve the
        // owning house in O(1) (see `find_house_by_door`).
        let mut props = engine.get_item_props(dserial).await.unwrap_or_default();
        props.set_meta(houses::META_HOUSE_SERIAL, MetaValue::Int(multi_serial as i64));
        engine.set_item_props(dserial, Some(props)).await;
    }

    // ── Spawn sign ─────────────────────────────────────────────────────
    if sign_serial != 0 {
        let sx = (ox as i32 + def.sign_dx as i32) as u16;
        let sy = (oy as i32 + def.sign_dy as i32) as u16;
        let sz = oz.saturating_add(def.sign_dz);
        let sign = DemoEntity::Item {
            serial: sign_serial,
            graphic: def.sign_graphic,
            color: 0,
            amount: 1,
            x: sx,
            y: sy,
            z: sz,
            is_container: false,
            hidden: false,
            facing: None,
        };
        engine.spawn_entity(sign_serial, sign).await;

        // Tag the sign with its parent house so a sign click can resolve the
        // owning house in O(1) (see `find_house_by_sign`).
        let mut props = engine.get_item_props(sign_serial).await.unwrap_or_default();
        props.set_meta(houses::META_HOUSE_SERIAL, MetaValue::Int(multi_serial as i64));
        engine.set_item_props(sign_serial, Some(props)).await;
    }

    // ── Move the owner inside, in front of the door ────────────────────
    //
    // The house multi was registered synchronously on spawn, so `resolve_z`
    // now reports the floor height (~7) for tiles under the multi.  Without
    // this the owner stays at ground Z and ends up clipped into the floor.
    //
    // Target tile: one tile north of the (first) door, i.e. just inside the
    // doorway.  We teleport via the engine, which emits an `EntityMoved`
    // event with `is_teleport: true`; the session's event pipeline then sends
    // the client a `DrawGamePlayer` (0x20) snap and updates `PlayerState`.
    {
        use u_core::Heading;

        let (tx, ty) = match def.doors.first() {
            Some(d) => (
                (ox as i32 + d.dx as i32).max(0) as u16,
                (oy as i32 + d.dy as i32 - 1).max(0) as u16,
            ),
            // No doors: fall back to the multi origin.
            None => (ox, oy),
        };
        let floor_z = engine
            .resolve_z(tx, ty, oz, Heading::South)
            .await
            .unwrap_or(oz);
        engine.teleport(owner, tx, ty, floor_z, None).await;
    }

    // ── Consume the deed ───────────────────────────────────────────────
    let _ = engine.consume_item(deed_serial, 1, None).await;
    // Tell the client the deed is gone (it lived in the backpack).
    session
        .send(RawPacket::s2c(encode_packet(&DeleteObject {
            id: DeleteObject::ID,
            serial: deed_serial,
        })))
        .await?;

    info!(
        "[house] placed {} (multi {:#010X}) at ({},{},{}) owner={:#010X}",
        def.name, multi_serial, ox, oy, oz, owner,
    );

    session
        .send(game_util::system_message_gray(&format!(
            "You have placed {}.",
            def.name
        )))
        .await?;

    Ok(())
}

/// Validate that a house footprint at `(ox, oy)` is buildable.
///
/// Checks two independent layers:
///
/// 1. **Dynamic entities** — no other multi or item may overlap the
///    footprint (mobiles are allowed; a house can be placed over a standing
///    player).
/// 2. **Terrain** — the land under the footprint must be flat, dry, passable
///    ground, free of blocking statics (trees, rocks, walls, foliage).
///
/// On success returns `Ok(foundation_z)`, the common ground height the house
/// should sit at (computed server-side from the land data).  On failure
/// returns `Err(reason)` with a player-facing message.
async fn validate_placement(
    def: &HouseDef,
    ox: u16,
    oy: u16,
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
) -> Result<i8, &'static str> {
    use common::uo_engine::handler::HouseTerrainResult;
    use framework::ecumene::TileRect;

    // Compute world-space footprint bbox.
    let x_min = (ox as i32 + def.foot_x_min as i32).max(0) as u16;
    let y_min = (oy as i32 + def.foot_y_min as i32).max(0) as u16;
    let x_max = (ox as i32 + def.foot_x_max as i32).max(0) as u16;
    let y_max = (oy as i32 + def.foot_y_max as i32).max(0) as u16;

    let rect = TileRect { x_min, y_min, x_max, y_max };

    // ── 1. Dynamic entities ────────────────────────────────────────────
    // Reject if any other multi or item overlaps the footprint.
    let entities = engine.query_area(rect).await;
    for e in &entities {
        match e {
            DemoEntity::Multi { .. } => {
                return Err("That location is blocked by another structure.");
            }
            DemoEntity::Item { .. } => {
                return Err("The area must be clear of items to place a house.");
            }
            DemoEntity::Mobile(_) => {}
        }
    }

    // ── 2. Terrain ─────────────────────────────────────────────────────
    match engine.validate_house_footprint(rect).await {
        HouseTerrainResult::Ok { foundation_z } => Ok(foundation_z),
        HouseTerrainResult::Water => {
            Err("You cannot place a house on water.")
        }
        HouseTerrainResult::Impassable => {
            Err("You cannot build a house here.")
        }
        HouseTerrainResult::Uneven => {
            Err("The ground here is too uneven to place a house.")
        }
        HouseTerrainResult::Blocked => {
            Err("The area must be clear of trees and rocks to place a house.")
        }
        HouseTerrainResult::OutOfBounds => {
            Err("You cannot place a house there.")
        }
        HouseTerrainResult::NoData => {
            Err("House placement is not available in this area.")
        }
    }
}

// ── Sign double-click → management gump ─────────────────────────────────────

/// Handle a double-click on a possible house sign.
///
/// Returns `Ok(true)` if the clicked item is a house sign and the management
/// gump was opened.
pub(super) async fn handle_sign_double_click(
    serial: u32,
    infra: &mut InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let Some(player) = infra.player.as_ref() else {
        return Ok(false);
    };
    let world = player.world;
    let viewer = player.serial;

    // Guard: only treat the item as a sign if its graphic is actually a house
    // sign.  Doors also carry META_HOUSE_SERIAL (so they can resolve their
    // owning house in O(1)), which would otherwise cause find_house_by_sign to
    // match a door and open the house gump instead of opening the door.
    let engine = game_util::engine_for(worker_tx, world);
    let graphic = engine
        .get_entity(serial)
        .await
        .and_then(|e| e.item().map(|i| i.graphic));
    match graphic {
        Some(g) if g == crate::houses::SIGN_WOOD || g == crate::houses::SIGN_METAL => {}
        _ => return Ok(false),
    }

    // Find the house whose sign this serial is.
    let Some((house_serial, owner)) = find_house_by_sign(serial, world, worker_tx).await else {
        return Ok(false);
    };

    info!(
        "[house] sign {:#010X} double-clicked → house {:#010X} owner={:#010X}",
        serial, house_serial, owner
    );

    // Resolve the owner's display name (best-effort).
    let engine = game_util::engine_for(worker_tx, world);
    let owner_name = engine
        .get_entity(owner)
        .await
        .as_ref()
        .and_then(|e| e.mobile().map(|m| m.name.clone()))
        .unwrap_or_else(|| format!("{:#010X}", owner));

    let is_owner = viewer == owner;
    send_house_gump(session, &owner_name, is_owner).await?;
    infra.open_house_gump = Some(house_serial);
    Ok(true)
}

/// Build and send the house-management gump.
async fn send_house_gump(
    session: &mut Session,
    owner_name: &str,
    is_owner: bool,
) -> error::Result<()> {
    use packets::gump::{GumpTextLine, SendGumpDialog};

    // Layout: background + title + (owner-only) Demolish button + Close button.
    let layout = if is_owner {
        "\
        { page 0 }{ noclose }\
        { resizepic 0 0 2600 200 150 }\
        { text 20 15 1153 0 }\
        { text 20 40 996 1 }\
        { button 20 75 4017 4018 1 0 1 }{ text 55 77 996 2 }\
        { button 20 110 4014 4015 1 0 2 }{ text 55 112 996 3 }"
    } else {
        "\
        { page 0 }{ noclose }\
        { resizepic 0 0 2600 200 120 }\
        { text 20 15 1153 0 }\
        { text 20 40 996 1 }\
        { button 20 80 4014 4015 1 0 2 }{ text 55 82 996 3 }"
    };

    let text_lines = vec![
        GumpTextLine("House".to_string()),                       // 0 — title
        GumpTextLine(format!("Owner: {}", owner_name)),          // 1
        GumpTextLine("Demolish house".to_string()),             // 2 (button 1)
        GumpTextLine("Close".to_string()),                      // 3 (button 2)
    ];

    let dialog = SendGumpDialog {
        serial: 0,
        gump_id: HOUSE_GUMP_ID,
        x: 100,
        y: 100,
        layout: layout.to_string(),
        text_lines,
        trailing_pad: vec![],
    };
    session.send(RawPacket::s2c(dialog.to_bytes())).await?;
    Ok(())
}

/// Handle a gump response for the house-management gump.
///
/// Returns `Ok(true)` if the gump id matched (response consumed).
pub(super) async fn handle_house_gump(
    gump_id: u32,
    button_id: u32,
    infra: &mut InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if gump_id != HOUSE_GUMP_ID {
        return Ok(false);
    }

    let house_serial = infra.open_house_gump.take();

    // Button 1 = Demolish; anything else (incl. 0/Close) = dismiss.
    if button_id != 1 {
        return Ok(true);
    }

    let Some(house_serial) = house_serial else {
        return Ok(true);
    };
    let Some(player) = infra.player.as_ref() else {
        return Ok(true);
    };
    let world = player.world;
    let viewer = player.serial;

    demolish_house(house_serial, viewer, world, session, worker_tx).await?;
    Ok(true)
}

// ── Demolish ────────────────────────────────────────────────────────────────

/// Demolish a house: remove the multi, its doors and sign, and return a deed
/// to the owner's backpack.  Only the owner may demolish.
async fn demolish_house(
    house_serial: u32,
    viewer: u32,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = game_util::engine_for(worker_tx, world);

    let Some(entity) = engine.get_entity(house_serial).await else {
        session.send(game_util::system_message("That house no longer exists.")).await?;
        return Ok(());
    };

    let (graphic, owner, door_serials, sign_serial) = match &entity {
        DemoEntity::Multi { graphic, owner, door_serials, sign_serial, .. } => {
            (*graphic, *owner, door_serials.clone(), *sign_serial)
        }
        _ => {
            session.send(game_util::system_message("That is not a house.")).await?;
            return Ok(());
        }
    };

    if owner != viewer {
        session.send(game_util::system_message("You do not own this house.")).await?;
        return Ok(());
    }

    info!("[house] demolishing {:#010X} (owner {:#010X})", house_serial, owner);

    // Remove the multi.
    engine.remove_entity(house_serial).await;
    session
        .send(RawPacket::s2c(encode_packet(&DeleteObject {
            id: DeleteObject::ID,
            serial: house_serial,
        })))
        .await?;

    // Remove doors.
    for &d in &door_serials {
        engine.remove_entity(d).await;
        session
            .send(RawPacket::s2c(encode_packet(&DeleteObject {
                id: DeleteObject::ID,
                serial: d,
            })))
            .await?;
    }

    // Remove sign.
    if sign_serial != 0 {
        engine.remove_entity(sign_serial).await;
        session
            .send(RawPacket::s2c(encode_packet(&DeleteObject {
                id: DeleteObject::ID,
                serial: sign_serial,
            })))
            .await?;
    }

    // Return a deed to the owner's backpack.
    let deed_graphic = match houses::all().iter().find(|h| h.multi_id == graphic) {
        Some(def) => def.deed_graphic,
        None => houses::DEED_SMALL_WOOD,
    };
    return_deed(viewer, world, deed_graphic, worker_tx).await;

    session
        .send(game_util::system_message_gray(
            "Your house has been demolished. A deed has been placed in your backpack.",
        ))
        .await?;

    Ok(())
}

/// Drop a fresh house deed into a player's backpack.
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

// ── Door toggle ─────────────────────────────────────────────────────────────

/// Handle a double-click on a possible door (house door or world/replay door).
///
/// Returns `Ok(true)` if the clicked item is a door (open/close toggled, or a
/// "locked" message for a house door the viewer does not own).  Returns
/// `Ok(false)` if the item is not a door.
///
/// Door identification is universal: a graphic is treated as a door if
/// tiledata flags it `DOOR` (when `--data` is loaded) or — as a fallback when
/// tiledata is unavailable — if it falls within the known door block.  The
/// open/closed state and the per-facing position shift are decoded purely
/// from the graphic via [`crate::doors`], so doors that hinge in any
/// direction toggle correctly.
///
/// Access: a door attached to a player house is owner-only; a door that does
/// not belong to any house (e.g. loaded from a replay log) is public — anyone
/// may open it, mirroring tavern / public doors in classic UO.
pub(super) async fn handle_door_double_click(
    serial: u32,
    infra: &InfraState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    use crate::doors;

    let Some(player) = infra.player.as_ref() else {
        return Ok(false);
    };
    let world = player.world;
    let viewer = player.serial;

    let engine = game_util::engine_for(worker_tx, world);

    // Resolve the item's graphic and position (and preserve its other fields).
    let item = engine.get_entity(serial).await;
    let (graphic, x, y, z, color, amount, is_container, hidden, facing) =
        match item.as_ref().and_then(|e| e.item()) {
            Some(i) => (
                i.graphic, i.x, i.y, i.z, i.color, i.amount,
                i.is_container, i.hidden, i.facing,
            ),
            None => return Ok(false),
        };

    // Is this graphic a door?  Authoritative tiledata flag first, falling
    // back to the arithmetic block test when no static data is loaded.
    let is_door = match infra.static_data.as_ref() {
        Some(sd) => sd
            .static_tile_def(graphic)
            .map(|d| d.flags.has(files::tiledata::TileFlags::DOOR))
            .unwrap_or(false),
        None => doors::is_door_graphic(graphic),
    };
    if !is_door {
        return Ok(false);
    }

    // House doors are owner-only; ownerless (replay/world) doors are public.
    if let Some((_house_serial, owner)) =
        find_house_by_door(serial, world, worker_tx).await
    {
        if owner != viewer {
            session.send(game_util::system_message("That is locked.")).await?;
            return Ok(true);
        }
    }

    // Toggle: decode the new graphic + per-facing position shift.
    let state = doors::classify(graphic);
    let opening = !state.is_open;
    let (new_graphic, dx, dy) = doors::toggle_target(graphic);
    let new_x = (x as i32 + dx as i32) as u16;
    let new_y = (y as i32 + dy as i32) as u16;

    // A door cannot close onto a mobile standing in the doorway.  If the tile
    // the leaf would return to is occupied, leave the door open and queue it
    // for a prompt retry — the worker tick will close it shortly after the
    // doorway clears (e.g. the player steps through and walks on).
    if !opening {
        use framework::ecumene::TileRect;
        let rect = TileRect { x_min: new_x, y_min: new_y, x_max: new_x, y_max: new_y };
        let blocked = engine
            .query_area(rect)
            .await
            .iter()
            .any(|e| e.is_mobile());
        if blocked {
            let mut props = engine.get_item_props(serial).await.unwrap_or_default();
            let close_at = crate::handler::door_clock_now_ms() + doors::DOOR_RETRY_CLOSE_MS;
            props.meta.insert(
                doors::META_DOOR_CLOSE_AT.to_string(),
                common::uo_engine::item_props::MetaValue::Int(close_at),
            );
            engine.set_item_props(serial, Some(props)).await;
            return Ok(true);
        }
    }

    let updated = DemoEntity::Item {
        serial,
        graphic: new_graphic,
        color,
        amount,
        x: new_x,
        y: new_y,
        z,
        is_container,
        hidden,
        facing,
    };
    engine.update_entity(serial, updated).await;

    // Schedule (or cancel) the auto-close via item-props meta.  Opening sets
    // the close-at timestamp; closing clears it.
    {
        let mut props = engine.get_item_props(serial).await.unwrap_or_default();
        if opening {
            let close_at = crate::handler::door_clock_now_ms() + doors::DOOR_AUTO_CLOSE_MS;
            props.meta.insert(
                doors::META_DOOR_CLOSE_AT.to_string(),
                common::uo_engine::item_props::MetaValue::Int(close_at),
            );
        } else {
            props.meta.remove(doors::META_DOOR_CLOSE_AT);
        }
        engine.set_item_props(serial, Some(props)).await;
    }

    // Door sound (broadcast to nearby observers) at the door's new position.
    let snd = if opening { sound::DOOR_OPEN } else { sound::DOOR_CLOSE };
    game_util::send_sound(worker_tx, world, snd, new_x, new_y, z as i16).await;

    Ok(true)
}

// ── House lookup helpers ────────────────────────────────────────────────────

/// Find the house (multi) that owns a given sign serial.
///
/// Returns `(house_serial, owner_serial)`, or `None` if the item is not a
/// player-house sign.  Resolution is O(1): the parent house serial is read
/// from the sign's [`houses::META_HOUSE_SERIAL`] meta (written at placement),
/// and the owner is read from the house multi itself.
async fn find_house_by_sign(
    sign_serial: u32,
    world: u8,
    worker_tx: &DemoWorkerTx,
) -> Option<(u32, u32)> {
    find_house_for_item(sign_serial, world, worker_tx).await
}

/// Find the house (multi) that owns a given door serial.
///
/// Resolution is O(1) via the door's [`houses::META_HOUSE_SERIAL`] meta.
/// Doors that lack this meta (e.g. ordinary doors from a replay log) return
/// `None` and are treated as non-house items.
async fn find_house_by_door(
    door_serial: u32,
    world: u8,
    worker_tx: &DemoWorkerTx,
) -> Option<(u32, u32)> {
    find_house_for_item(door_serial, world, worker_tx).await
}

/// Resolve the owning house of a player-placed door or sign.
///
/// Reads the parent house serial from the item's
/// [`houses::META_HOUSE_SERIAL`] meta, then reads the current `owner` from
/// the house multi.  Returns `None` if the item carries no house meta or the
/// referenced house no longer exists.
async fn find_house_for_item(
    item_serial: u32,
    world: u8,
    worker_tx: &DemoWorkerTx,
) -> Option<(u32, u32)> {
    let engine = game_util::engine_for(worker_tx, world);

    let props = engine.get_item_props(item_serial).await?;
    let house_serial = props.get_meta_int(houses::META_HOUSE_SERIAL)? as u32;

    // Read the authoritative owner from the multi (single source of truth).
    match engine.get_entity(house_serial).await {
        Some(DemoEntity::Multi { owner, .. }) => Some((house_serial, owner)),
        _ => None,
    }
}

// ── Cursor dispatch entry point ─────────────────────────────────────────────

/// Dispatch a validated [`CursorKind::HousePlacement`](super::pending_cursor::CursorKind::HousePlacement) target response.
///
/// `cursor_id` is the placement cursor id that already matched the response.
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
