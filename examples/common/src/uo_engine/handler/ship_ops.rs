//! Ship movement operations: move one tile, turn (change facing).
//!
//! - [`handle_move_ship`] — advance a ship by `(dx, dy)` tiles
//! - [`handle_turn_ship`] — swap the ship's multi graphic (rotation)

use log::trace;

use framework::continuum::{Zone, WorldEvent};
use framework::continuum::item_props::ZoneItemProps;
use framework::continuum::container::HashContainerStore;
use framework::ecumene::{TileRect, Entity as EngineEntity};
use u_core::{Facing, MobilePos};

use crate::uo_engine::entity::DemoEntity;
use crate::uo_engine::item_props::ItemProps;

use super::validate_ship_terrain;
use super::ShipTerrainResult;

// ── Ship-component meta keys (mirrors `demo-server`'s `ships` module) ──────
//
// These string keys are duplicated here (rather than imported from the
// demo-server binary) because `common` is a library that the demo-server
// depends on, not the other way around.  They must stay in sync with
// `examples/demo-server/ships.rs`.

const META_SHIP_HEADING: &str = "ship_heading";
const META_SHIP_GFX: [&str; 4] = ["ship_gfx_n", "ship_gfx_e", "ship_gfx_s", "ship_gfx_w"];

/// Downcast a generic `&P::Value` to `&ItemProps` (always `ItemProps` for the
/// demo-server).
fn as_item_props<P: ZoneItemProps>(value: Option<&P::Value>) -> Option<&ItemProps>
where
    P::Value: 'static,
{
    let v = value?;
    (v as &dyn std::any::Any).downcast_ref::<ItemProps>()
}

/// Re-box an `ItemProps` as the zone's concrete `P::Value`.
///
/// Safe as long as `P::Value == ItemProps`, which always holds for the
/// demo-server (mirrors `kill_ops::unsafe_cast_props`).
fn unsafe_cast_props<P: ZoneItemProps>(props: ItemProps) -> P::Value
where
    P::Value: 'static,
{
    let boxed: Box<dyn std::any::Any> = Box::new(props);
    *boxed.downcast::<P::Value>().expect("ZoneItemProps::Value must be ItemProps")
}

/// Read the integer heading (0..=3) stored on a ship multi's item props.
fn ship_heading_index<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    serial: u32,
) -> Option<u8>
where
    P::Value: 'static,
{
    as_item_props::<P>(zone.item_props.get(serial))
        .and_then(|p| p.get_meta_int(META_SHIP_HEADING))
        .map(|v| (v & 0x3) as u8)
}

/// Read a ship child's graphic for the given heading index from its meta,
/// preserving the child's current open/closed parity (so an open plank stays
/// open after a turn).
fn child_graphic_for_heading<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    child_serial: u32,
    cur_graphic: u16,
    heading_index: u8,
) -> Option<u16>
where
    P::Value: 'static,
{
    let props = as_item_props::<P>(zone.item_props.get(child_serial))?;
    let key = META_SHIP_GFX[(heading_index & 0x3) as usize];
    let base = props.get_meta_int(key)? as u16;
    // Stored value is the closed graphic; carry over the current parity so an
    // open plank stays open (closed components are unaffected — even base,
    // even result).
    Some((base & !1) | (cur_graphic & 1))
}

/// Collect the explicit child serials (planks + hold in `door_serials`,
/// tillerman in `sign_serial`) of a ship multi.
fn ship_child_serials<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    serial: u32,
) -> Vec<u32>
where
    P::Value: 'static,
{
    match zone.get(serial) {
        Some(DemoEntity::Multi { door_serials, sign_serial, .. }) => {
            let mut v = door_serials.clone();
            if *sign_serial != 0 {
                v.push(*sign_serial);
            }
            v
        }
        _ => Vec::new(),
    }
}

/// Read the `(door_serials, sign_serial)` of a ship multi so they can be
/// preserved when the hull is rebuilt in place.
fn ship_children_fields<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    serial: u32,
) -> (Vec<u32>, u32)
where
    P::Value: 'static,
{
    match zone.get(serial) {
        Some(DemoEntity::Multi { door_serials, sign_serial, .. }) => {
            (door_serials.clone(), *sign_serial)
        }
        _ => (Vec::new(), 0),
    }
}

// ── Move ship one tile ─────────────────────────────────────────────────

/// Move a ship multi by `(dx, dy)` tiles.
///
/// 1. Looks up the multi entity and computes the new footprint.
/// 2. Validates that the new footprint is all-water (via
///    `validate_ship_terrain`).
/// 3. Finds all mobiles standing on the current deck.
/// 4. Updates the multi via `zone.update()` (handles `EntityRegistry`).
/// 5. Teleports each passenger by the same delta.
/// 6. Emits `EntityRemoved` + `EntitySpawned` for the multi, and
///    `EntityMoved` for each passenger.
pub fn handle_move_ship<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    dx: i32,
    dy: i32,
) -> Result<(), String>
where
    P::Value: 'static,
{
    // ── 1. Look up the ship entity ────────────────────────────────────
    let entity = zone.get(serial).ok_or("Ship not found")?;
    let (graphic, old_x, old_y, old_z, owner) = match entity {
        DemoEntity::Multi { graphic, x, y, z, owner, .. } => {
            (*graphic, *x, *y, *z, *owner)
        }
        _ => return Err("Not a multi entity".into()),
    };

    let new_x = (old_x as i32 + dx).clamp(0, u16::MAX as i32) as u16;
    let new_y = (old_y as i32 + dy).clamp(0, u16::MAX as i32) as u16;

    // ── 2. Compute the footprint from static data ─────────────────────
    let static_data = zone.static_data()
        .ok_or("No static data")?;
    let parts = static_data.multi_parts(graphic);
    if parts.is_empty() {
        return Err("Unknown multi graphic".into());
    }

    // Compute footprint bounding box from multi parts.
    let (mut fx_min, mut fy_min, mut fx_max, mut fy_max) =
        (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    for part in parts {
        fx_min = fx_min.min(part.x);
        fy_min = fy_min.min(part.y);
        fx_max = fx_max.max(part.x);
        fy_max = fy_max.max(part.y);
    }

    let foot_x_min = (new_x as i32 + fx_min as i32).max(0) as u16;
    let foot_y_min = (new_y as i32 + fy_min as i32).max(0) as u16;
    let foot_x_max = (new_x as i32 + fx_max as i32).max(0) as u16;
    let foot_y_max = (new_y as i32 + fy_max as i32).max(0) as u16;

    // ── 3. Validate new footprint is all water ────────────────────────
    match validate_ship_terrain(zone, foot_x_min, foot_y_min, foot_x_max, foot_y_max) {
        ShipTerrainResult::Ok { water_z: _ } => {}
        ShipTerrainResult::NotWater => return Err("Blocked: not water".into()),
        ShipTerrainResult::Blocked => return Err("Blocked: obstruction".into()),
        ShipTerrainResult::OutOfBounds => return Err("Blocked: out of bounds".into()),
        ShipTerrainResult::NoData => return Err("No terrain data".into()),
    }

    // Check no other multi overlaps the new footprint (but ignore self).
    //
    // Items are only an obstruction when they do **not** sit on this ship's
    // own deck.  A loose item dropped on the deck is cargo (carried along by
    // this move, see below) and must not stop the ship; an item floating in
    // the open water ahead still blocks.
    let new_rect = TileRect {
        x_min: foot_x_min,
        y_min: foot_y_min,
        x_max: foot_x_max,
        y_max: foot_y_max,
    };
    let area_entities = zone.query_area(&new_rect);
    for e in &area_entities {
        match e {
            DemoEntity::Multi { serial: s, .. } if *s != serial => {
                return Err("Blocked: another structure".into());
            }
            DemoEntity::Item { x: ix, y: iy, .. } => {
                // On *this* ship's deck (relative to its current origin) → cargo,
                // not an obstruction.  Anything else blocks.
                if super::ship_deck_z_at(zone, serial, *ix, *iy).is_none() {
                    return Err("Blocked: item in the way".into());
                }
            }
            _ => {}
        }
    }

    // ── 4. Collect passengers on the deck ─────────────────────────────
    //
    // A mobile counts as a passenger when it is either already bound to
    // this ship (`ship_serial == serial`) or is standing on a walkable
    // deck tile of this ship right now (handles boarding without an
    // explicit bind, e.g. teleport / spawn onto the deck).  Using the deck
    // test rather than a raw bbox hit avoids dragging along swimmers or
    // mobiles merely adjacent to the hull.
    let old_rect = TileRect {
        x_min: (old_x as i32 + fx_min as i32).max(0) as u16,
        y_min: (old_y as i32 + fy_min as i32).max(0) as u16,
        x_max: (old_x as i32 + fx_max as i32).max(0) as u16,
        y_max: (old_y as i32 + fy_max as i32).max(0) as u16,
    };
    let passengers: Vec<(u32, u16, u16, i8, u8)> = zone.query_area(&old_rect)
        .into_iter()
        .filter_map(|e| {
            if let DemoEntity::Mobile(m) = &e {
                let bound = m.ship_serial == Some(serial);
                let on_deck = super::ship_deck_z_at(zone, serial, m.x, m.y).is_some();
                if bound || on_deck {
                    Some((m.serial, m.x, m.y, m.z, m.direction))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Explicit ship components (tillerman, planks, hold).  Unlike loose
    // cargo these may sit on impassable hull tiles (mast / gunwale) that
    // `ship_deck_z_at` rejects, so they are carried by their recorded serials
    // rather than by a geometric deck scan.
    let children = ship_child_serials(zone, serial);

    // Cargo: loose items resting on this ship's deck travel with the hull.
    // Collected from the *old* footprint (before the hull moves) so their
    // current deck tile is still valid for the `ship_deck_z_at` test.
    // Explicit children are excluded so they are not moved twice.
    let cargo: Vec<(u32, u16, u16, i8)> = zone.query_area(&old_rect)
        .into_iter()
        .filter_map(|e| {
            if let DemoEntity::Item { serial: s, x, y, z, .. } = &e {
                if children.contains(s) {
                    return None;
                }
                if super::ship_deck_z_at(zone, serial, *x, *y).is_some() {
                    Some((*s, *x, *y, *z))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // ── 5. Move the ship multi ────────────────────────────────────────
    //
    // We update the multi **in place** (`zone.update`) rather than
    // remove + spawn.  `update` re-indexes the EntityRegistry collision
    // shapes for the new origin internally, but — crucially — it never
    // leaves the multi absent.
    //
    // The hull move **and** every passenger snap are bundled into a single
    // `WorldEvent::ShipMoved` so the session drains and renders them in one
    // atomic batch (one `PauseClient` frame).  Emitting them as separate
    // events let the session occasionally drain the hull and a passenger in
    // *different* batches, which made the on-deck player jitter / desync.
    let map_id = zone.map_id;
    let ship_old_pos = u_core::Pos3D::new(old_x, old_y, old_z);

    // Preserve the child serials carried on the hull (planks + hold in
    // `door_serials`, tillerman in `sign_serial`); rebuilding the multi with
    // empty fields would orphan the components.
    let (door_serials, sign_serial) = ship_children_fields(zone, serial);

    let new_ship = DemoEntity::Multi {
        serial,
        graphic,
        x: new_x,
        y: new_y,
        z: old_z,
        owner,
        door_serials,
        sign_serial,
    };
    let ship_snapshot = new_ship.snapshot();
    zone.update(serial, new_ship);
    let ship_new_pos = u_core::Pos3D::new(new_x, new_y, old_z);

    trace!(
        "[ship_ops] moved ship {:#010X} ({},{}) -> ({},{}), {} passengers, {} cargo",
        serial, old_x, old_y, new_x, new_y, passengers.len(), cargo.len(),
    );

    // ── 6. Move passengers, collecting them into the bundled event ─────
    let mut passenger_events:
        Vec<(u32, MobilePos, MobilePos, Option<framework::continuum::EntitySnapshot>)> =
        Vec::with_capacity(passengers.len());

    for (mob_serial, mx, my, mz, mdir) in &passengers {
        let new_mx = (*mx as i32 + dx).clamp(0, u16::MAX as i32) as u16;
        let new_my = (*my as i32 + dy).clamp(0, u16::MAX as i32) as u16;

        let old_mpos = MobilePos::new(*mx, *my, *mz, Facing::new(*mdir));
        zone.move_entity(*mob_serial, new_mx, new_my, *mz, None);

        // Make sure the carried mobile is bound to this ship so subsequent
        // deck steps are validated relative to it.
        if let Some(DemoEntity::Mobile(mm)) = zone.store.get_mut(*mob_serial) {
            mm.ship_serial = Some(serial);
        }

        // Re-read the mobile's **current** facing from the zone rather than
        // re-using `*mdir` (the direction captured when the passenger list
        // was built, before this tick ran).  A player may have turned in
        // place between two sail ticks; that turn is already applied in the
        // store (`move_entity(.., None)` preserves the direction).  Note: for
        // the player's *own* serial the session ignores this facing entirely
        // and keeps the client-driven heading — see `collect_world_event_packets`.
        let cur_dir = match zone.get(*mob_serial) {
            Some(DemoEntity::Mobile(m)) => m.direction,
            _ => *mdir,
        };

        let snap = zone.get(*mob_serial).and_then(|e| e.snapshot());
        let new_mpos = MobilePos::new(new_mx, new_my, *mz, Facing::new(cur_dir));
        passenger_events.push((*mob_serial, old_mpos, new_mpos, snap));
    }

    // ── 7. Move cargo (deck items) into the bundled event ─────────────
    let mut cargo_events:
        Vec<(u32, u_core::Pos3D, u_core::Pos3D, Option<framework::continuum::EntitySnapshot>)> =
        Vec::with_capacity(cargo.len());

    for (item_serial, ix, iy, iz) in &cargo {
        let new_ix = (*ix as i32 + dx).clamp(0, u16::MAX as i32) as u16;
        let new_iy = (*iy as i32 + dy).clamp(0, u16::MAX as i32) as u16;

        let old_ipos = u_core::Pos3D::new(*ix, *iy, *iz);
        zone.move_entity(*item_serial, new_ix, new_iy, *iz, None);

        let snap = zone.get(*item_serial).and_then(|e| e.snapshot());
        let new_ipos = u_core::Pos3D::new(new_ix, new_iy, *iz);
        cargo_events.push((*item_serial, old_ipos, new_ipos, snap));
    }

    // ── 7b. Move explicit ship components (tillerman, planks, hold) ───
    //
    // Carried by serial (they may sit on impassable hull tiles).  They ride
    // in the same `ShipMoved` event as cargo so the client redraws them in
    // the same atomic frame as the hull.
    for child_serial in &children {
        let Some((cx, cy, cz)) = zone.get(*child_serial).map(|e| e.xyz()) else {
            continue;
        };
        let new_cx = (cx as i32 + dx).clamp(0, u16::MAX as i32) as u16;
        let new_cy = (cy as i32 + dy).clamp(0, u16::MAX as i32) as u16;

        let old_cpos = u_core::Pos3D::new(cx, cy, cz);
        zone.move_entity(*child_serial, new_cx, new_cy, cz, None);

        let snap = zone.get(*child_serial).and_then(|e| e.snapshot());
        let new_cpos = u_core::Pos3D::new(new_cx, new_cy, cz);
        cargo_events.push((*child_serial, old_cpos, new_cpos, snap));
    }

    let _ = event_tx.send(WorldEvent::ShipMoved {
        map_id,
        ship_serial: serial,
        ship_old_pos,
        ship_new_pos,
        ship_snapshot,
        passengers: passenger_events,
        cargo: cargo_events,
    });

    Ok(())
}

// ── Turn ship (change facing) ──────────────────────────────────────────

/// Turn a ship by swapping its multi graphic to `new_graphic`.
///
/// The new graphic must be a valid facing of the same ship type.  The
/// footprint is re-validated at the new facing before the swap.
///
/// `quarter_turns_cw` is the clockwise 90° rotation applied to the hull
/// (`1` = right, `-1` = left, `2` = about-face).  Each passenger keeps the
/// same spot on the deck: their offset from the ship origin is rotated by the
/// same amount, and their deck Z is recomputed for the new facing.
pub fn handle_turn_ship<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    new_graphic: u16,
    quarter_turns_cw: i8,
) -> Result<u16, String>
where
    P::Value: 'static,
{
    // ── 1. Look up the ship ───────────────────────────────────────────
    let entity = zone.get(serial).ok_or("Ship not found")?;
    let (old_graphic, ox, oy, oz, owner) = match entity {
        DemoEntity::Multi { graphic, x, y, z, owner, .. } => {
            (*graphic, *x, *y, *z, *owner)
        }
        _ => return Err("Not a multi entity".into()),
    };

    if old_graphic == new_graphic {
        return Ok(new_graphic);
    }

    // ── 2. Compute new footprint ──────────────────────────────────────
    let static_data = zone.static_data()
        .ok_or("No static data")?;
    let parts = static_data.multi_parts(new_graphic);
    if parts.is_empty() {
        return Err("Unknown new multi graphic".into());
    }

    let (mut fx_min, mut fy_min, mut fx_max, mut fy_max) =
        (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    for part in parts {
        fx_min = fx_min.min(part.x);
        fy_min = fy_min.min(part.y);
        fx_max = fx_max.max(part.x);
        fy_max = fy_max.max(part.y);
    }

    let foot_x_min = (ox as i32 + fx_min as i32).max(0) as u16;
    let foot_y_min = (oy as i32 + fy_min as i32).max(0) as u16;
    let foot_x_max = (ox as i32 + fx_max as i32).max(0) as u16;
    let foot_y_max = (oy as i32 + fy_max as i32).max(0) as u16;

    // ── 3. Validate new footprint ─────────────────────────────────────
    match validate_ship_terrain(zone, foot_x_min, foot_y_min, foot_x_max, foot_y_max) {
        ShipTerrainResult::Ok { .. } => {}
        ShipTerrainResult::NotWater => return Err("Cannot turn: not enough water".into()),
        ShipTerrainResult::Blocked => return Err("Cannot turn: obstruction".into()),
        ShipTerrainResult::OutOfBounds => return Err("Cannot turn: out of bounds".into()),
        ShipTerrainResult::NoData => return Err("No terrain data".into()),
    }

    // Check no other multi overlaps the new footprint (ignore self).
    let new_rect = TileRect {
        x_min: foot_x_min,
        y_min: foot_y_min,
        x_max: foot_x_max,
        y_max: foot_y_max,
    };
    let area_entities = zone.query_area(&new_rect);
    for e in &area_entities {
        match e {
            DemoEntity::Multi { serial: s, .. } if *s != serial => {
                return Err("Cannot turn: another structure in the way".into());
            }
            _ => {}
        }
    }

    // ── 4. Collect passengers ─────────────────────────────────────────
    let old_parts = static_data.multi_parts(old_graphic);
    let (mut old_fx_min, mut old_fy_min, mut old_fx_max, mut old_fy_max) =
        (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    for part in old_parts {
        old_fx_min = old_fx_min.min(part.x);
        old_fy_min = old_fy_min.min(part.y);
        old_fx_max = old_fx_max.max(part.x);
        old_fy_max = old_fy_max.max(part.y);
    }
    let old_rect = TileRect {
        x_min: (ox as i32 + old_fx_min as i32).max(0) as u16,
        y_min: (oy as i32 + old_fy_min as i32).max(0) as u16,
        x_max: (ox as i32 + old_fx_max as i32).max(0) as u16,
        y_max: (oy as i32 + old_fy_max as i32).max(0) as u16,
    };
    let passengers: Vec<(u32, u16, u16, i8, u8)> = zone.query_area(&old_rect)
        .into_iter()
        .filter_map(|e| {
            if let DemoEntity::Mobile(m) = &e {
                let bound = m.ship_serial == Some(serial);
                let on_deck = super::ship_deck_z_at(zone, serial, m.x, m.y).is_some();
                if bound || on_deck {
                    Some((m.serial, m.x, m.y, m.z, m.direction))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Explicit ship components (carried/rotated by serial).
    let children = ship_child_serials(zone, serial);

    // Cargo: loose items on this ship's deck rotate with the hull, keeping
    // their relative spot on the deck.  Explicit children are handled
    // separately (they also swap graphic per heading), so exclude them here.
    let cargo: Vec<(u32, u16, u16, i8)> = zone.query_area(&old_rect)
        .into_iter()
        .filter_map(|e| {
            if let DemoEntity::Item { serial: s, x, y, z, .. } = &e {
                if children.contains(s) {
                    return None;
                }
                if super::ship_deck_z_at(zone, serial, *x, *y).is_some() {
                    Some((*s, *x, *y, *z))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Resolve the heading the ship will face after this turn so children can
    // pick the right per-heading graphic.  Falls back to North if the multi
    // has no recorded heading (e.g. a world/static ship without components).
    let turns_for_heading = (((quarter_turns_cw % 4) + 4) % 4) as u8;
    let old_heading = ship_heading_index(zone, serial).unwrap_or(0);
    let new_heading = (old_heading + turns_for_heading) & 0x3;

    // ── 5. Replace the multi with the new facing ──────────────────────
    //
    // The origin does not change on a turn — only the graphic.  Update the
    // multi in place (`zone.update` re-indexes the registry shapes for the
    // new facing) and emit a single `EntityMoved` so the client re-sends the
    // `0x1A ObjectInfo` for the new facing without a delete/redraw flicker.
    let map_id = zone.map_id;
    let mpos_origin = MobilePos::new(ox, oy, oz, Facing::new(0));

    // Preserve child serials carried on the hull (rebuilding with empty
    // fields would orphan tillerman / planks / hold).
    let (door_serials, sign_serial) = ship_children_fields(zone, serial);

    let new_ship = DemoEntity::Multi {
        serial,
        graphic: new_graphic,
        x: ox,
        y: oy,
        z: oz,
        owner,
        door_serials,
        sign_serial,
    };
    let snap = new_ship.snapshot();
    zone.update(serial, new_ship);

    // Record the new heading on the ship so the next turn resolves children
    // correctly.
    {
        use crate::uo_engine::item_props::MetaValue;
        let mut props = as_item_props::<P>(zone.item_props.get(serial))
            .cloned()
            .unwrap_or_default();
        props.set_meta(META_SHIP_HEADING, MetaValue::Int(new_heading as i64));
        zone.item_props.insert(serial, unsafe_cast_props::<P>(props));
    }

    let _ = event_tx.send(WorldEvent::EntityMoved {
        map_id,
        serial,
        old_pos: mpos_origin,
        new_pos: mpos_origin,
        entity: snap,
        is_teleport: false,
    });

    trace!(
        "[ship_ops] turned ship {:#010X} graphic {:#06X} -> {:#06X}, {} passengers, {} cargo, {} quarter-turns",
        serial, old_graphic, new_graphic, passengers.len(), cargo.len(), quarter_turns_cw,
    );

    // ── 6. Rotate passengers around the ship origin ───────────────────
    //
    // Each passenger keeps the same spot on the deck: rotate its offset from
    // the origin by the hull rotation, then recompute the deck Z at the new
    // tile for the new facing.  Emit `EntityMoved(is_teleport: true)` so the
    // client snaps the mobile to the rotated position / new height.
    let turns = (((quarter_turns_cw % 4) + 4) % 4) as u8;
    for (mob_serial, mx, my, mz, mdir) in &passengers {
        // Offset relative to the ship origin.
        let mut rx = *mx as i32 - ox as i32;
        let mut ry = *my as i32 - oy as i32;
        // Apply `turns` clockwise 90° rotations: (rx, ry) -> (-ry, rx).
        for _ in 0..turns {
            let (nrx, nry) = (-ry, rx);
            rx = nrx;
            ry = nry;
        }
        let new_mx = (ox as i32 + rx).clamp(0, u16::MAX as i32) as u16;
        let new_my = (oy as i32 + ry).clamp(0, u16::MAX as i32) as u16;

        // Rotate the passenger's own facing by the same amount so they turn
        // with the boat (matching real-shard `SetFacing`).  In UO's 8-way
        // direction space one 90° hull quarter-turn is two direction steps.
        let new_mdir = (*mdir + turns * 2) & 0x7;

        // Recompute the deck Z under the new tile (falls back to old Z if the
        // tile is somehow off-deck after rotation).
        let new_mz = super::ship_deck_z_at(zone, serial, new_mx, new_my).unwrap_or(*mz);

        let old_mpos = MobilePos::new(*mx, *my, *mz, Facing::new(*mdir));
        zone.move_entity(*mob_serial, new_mx, new_my, new_mz, Some(new_mdir));
        if let Some(DemoEntity::Mobile(mm)) = zone.store.get_mut(*mob_serial) {
            mm.ship_serial = Some(serial);
        }
        let snap = zone.get(*mob_serial).and_then(|e| e.snapshot());
        let new_mpos = MobilePos::new(new_mx, new_my, new_mz, Facing::new(new_mdir));
        let _ = event_tx.send(WorldEvent::EntityMoved {
            map_id,
            serial: *mob_serial,
            old_pos: old_mpos,
            new_pos: new_mpos,
            entity: snap,
            is_teleport: true,
        });
    }

    // ── 7. Rotate cargo (deck items) around the ship origin ───────────
    //
    // Items keep their relative spot on the deck: rotate the offset from the
    // origin by the hull rotation.  Items have no facing and keep their Z.
    // Emit `EntityMoved(is_teleport: true)` so the client re-draws the item
    // (`0x1A ObjectInfo`) at the rotated tile.
    for (item_serial, ix, iy, iz) in &cargo {
        let mut rx = *ix as i32 - ox as i32;
        let mut ry = *iy as i32 - oy as i32;
        for _ in 0..turns {
            let (nrx, nry) = (-ry, rx);
            rx = nrx;
            ry = nry;
        }
        let new_ix = (ox as i32 + rx).clamp(0, u16::MAX as i32) as u16;
        let new_iy = (oy as i32 + ry).clamp(0, u16::MAX as i32) as u16;

        let old_ipos = MobilePos::new(*ix, *iy, *iz, Facing::new(0));
        zone.move_entity(*item_serial, new_ix, new_iy, *iz, None);

        let snap = zone.get(*item_serial).and_then(|e| e.snapshot());
        let new_ipos = MobilePos::new(new_ix, new_iy, *iz, Facing::new(0));
        let _ = event_tx.send(WorldEvent::EntityMoved {
            map_id,
            serial: *item_serial,
            old_pos: old_ipos,
            new_pos: new_ipos,
            entity: snap,
            is_teleport: true,
        });
    }

    // ── 8. Rotate explicit ship components (tillerman, planks, hold) ──
    //
    // Each component rotates around the origin like cargo, **and** swaps its
    // graphic to the per-heading art recorded on its `ItemProps.meta`.  An
    // open plank stays open (the stored graphic is the closed id; current
    // parity is preserved by `child_graphic_for_heading`).
    for child_serial in &children {
        let Some(view) = zone.get(*child_serial).and_then(|e| e.item()) else {
            continue;
        };
        let (cx, cy, cz, cur_gfx, color, amount, is_container, hidden, facing) = (
            view.x, view.y, view.z, view.graphic, view.color, view.amount,
            view.is_container, view.hidden, view.facing,
        );

        let mut rx = cx as i32 - ox as i32;
        let mut ry = cy as i32 - oy as i32;
        for _ in 0..turns {
            let (nrx, nry) = (-ry, rx);
            rx = nrx;
            ry = nry;
        }
        let new_cx = (ox as i32 + rx).clamp(0, u16::MAX as i32) as u16;
        let new_cy = (oy as i32 + ry).clamp(0, u16::MAX as i32) as u16;

        let new_gfx = child_graphic_for_heading(zone, *child_serial, cur_gfx, new_heading)
            .unwrap_or(cur_gfx);

        // Rebuild the item with the new graphic, then move it (graphic change
        // requires a store rewrite; `move_entity` only updates position).
        let updated = DemoEntity::Item {
            serial: *child_serial,
            graphic: new_gfx,
            color,
            amount,
            x: cx,
            y: cy,
            z: cz,
            is_container,
            hidden,
            facing,
        };
        zone.update(*child_serial, updated);
        zone.move_entity(*child_serial, new_cx, new_cy, cz, None);

        let old_cpos = MobilePos::new(cx, cy, cz, Facing::new(0));
        let new_cpos = MobilePos::new(new_cx, new_cy, cz, Facing::new(0));
        let snap = zone.get(*child_serial).and_then(|e| e.snapshot());
        let _ = event_tx.send(WorldEvent::EntityMoved {
            map_id,
            serial: *child_serial,
            old_pos: old_cpos,
            new_pos: new_cpos,
            entity: snap,
            is_teleport: true,
        });
    }

    Ok(new_graphic)
}
