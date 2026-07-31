//! Mobile movement operations: step, teleport.
//!
//! - [`MobileStepResult`] — result type for step/teleport
//! - `handle_mobile_step` — one-tile step with UO movement rules
//! - `handle_teleport` — direct position update (no passability check)

use log::trace;

use framework::continuum::{Zone, WorldEvent};
use framework::continuum::item_props::ZoneItemProps;
use framework::continuum::container::HashContainerStore;
use framework::ecumene::Entity as EngineEntity;
use u_core::{Facing, MobilePos};

use crate::uo_engine::entity::DemoEntity;

// ── Result type ─────────────────────────────────────────────────────────

/// Result of a successful `MobileStep` command.
///
/// Contains the new authoritative position of the mobile after the step
/// (or after a turn-in-place if the heading differed from the current
/// facing).
#[derive(Debug, Clone, Copy)]
pub struct MobileStepResult {
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub direction: u8,
    pub world: u8,
}

// ── Step logic ──────────────────────────────────────────────────────────

/// Perform a one-tile step for a mobile entity within the zone.
///
/// UO movement rules:
/// 1. If the requested heading differs from the entity's current facing,
///    only a turn-in-place occurs (direction is updated, no coordinate
///    change).  The result still contains the current position.
/// 2. If the heading matches, passability is checked via `zone.test_step`.
///    On success the entity's position and direction are updated in the
///    store.  On failure `None` is returned.
pub(super) fn handle_mobile_step<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    serial: u32,
    direction: Facing,
) -> Option<MobileStepResult>
where
    P::Value: 'static,
{
    let entity = zone.get(serial)?;

    // Only mobiles can step.
    let DemoEntity::Mobile(m) = entity else {
        return None;
    };

    let cur_x = m.x;
    let cur_y = m.y;
    let cur_z = m.z;
    let cur_heading = Facing::new(m.direction).heading();
    let new_heading = direction.heading();
    let cur_ship = m.ship_serial;
    let map_id = zone.map_id;

    if new_heading != cur_heading {
        // Turn-in-place: update direction only (no spatial index change).
        zone.move_entity(serial, cur_x, cur_y, cur_z, Some(direction.raw()));
        trace!(
            "[mobile_step] 0x{:08X} turn {} -> {} at ({},{},{})",
            serial, cur_heading, new_heading, cur_x, cur_y, cur_z,
        );
        return Some(MobileStepResult {
            x: cur_x,
            y: cur_y,
            z: cur_z,
            direction: direction.raw(),
            world: map_id,
        });
    }

    let (dx, dy) = new_heading.delta();
    let new_x = (cur_x as i32 + dx).clamp(0, 0x1FFF) as u16;
    let new_y = (cur_y as i32 + dy).clamp(0, 0x1FFF) as u16;

    // ── Ship-deck relative step ───────────────────────────────────────
    //
    // If the mobile is bound to a ship, first try to resolve the target as
    // a deck tile of that same ship.  This makes walking around the deck
    // independent of whether the ship's origin shifted a tile between the
    // client's view and the server state during a sailing tick — the step
    // is validated relative to the deck, not the (already-moved) water.
    if let Some(ship_serial) = cur_ship {
        if let Some(deck_z) =
            super::ship_deck_z_at(zone, ship_serial, new_x, new_y)
        {
            zone.move_entity(serial, new_x, new_y, deck_z, Some(direction.raw()));
            trace!(
                "[mobile_step] 0x{:08X} deck-step {} ({},{},{}) -> ({},{},{}) ship={:#010X}",
                serial, new_heading, cur_x, cur_y, cur_z, new_x, new_y, deck_z, ship_serial,
            );
            return Some(MobileStepResult {
                x: new_x,
                y: new_y,
                z: deck_z,
                direction: direction.raw(),
                world: map_id,
            });
        }
    }

    // Same heading — attempt a normal passability step.
    let new_z = zone.test_step(cur_x, cur_y, cur_z, new_heading)?;

    // Update entity in store + spatial index.
    zone.move_entity(serial, new_x, new_y, new_z, Some(direction.raw()));

    // ── Maintain the ship binding ─────────────────────────────────────
    //
    // The normal step succeeded onto plain terrain (or onto a ship deck the
    // mobile was not yet bound to — e.g. boarding from a dock).  Recompute
    // which ship, if any, the new tile belongs to and update the binding.
    let new_ship = ship_at_tile(zone, new_x, new_y);
    if new_ship != cur_ship {
        if let Some(DemoEntity::Mobile(mm)) = zone.store.get_mut(serial) {
            mm.ship_serial = new_ship;
        }
    }

    trace!(
        "[mobile_step] 0x{:08X} step {} ({},{},{}) -> ({},{},{})",
        serial, new_heading, cur_x, cur_y, cur_z, new_x, new_y, new_z,
    );

    Some(MobileStepResult {
        x: new_x,
        y: new_y,
        z: new_z,
        direction: direction.raw(),
        world: map_id,
    })
}

/// Find the serial of a ship multi whose footprint contains `(x, y)`, if any.
///
/// Returns the first multi with a walkable deck at the tile (via
/// [`super::ship_deck_z_at`]).  Used to (re)establish a mobile's ship binding
/// after a normal terrain step.
fn ship_at_tile<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    x: u16,
    y: u16,
) -> Option<u32>
where
    P::Value: 'static,
{
    use framework::ecumene::TileRect;
    let rect = TileRect { x_min: x, y_min: y, x_max: x, y_max: y };
    for e in zone.query_area(&rect) {
        if let DemoEntity::Multi { serial: s, .. } = &e {
            if super::ship_deck_z_at(zone, *s, x, y).is_some() {
                return Some(*s);
            }
        }
    }
    None
}

// ── Teleport logic ──────────────────────────────────────────────────────

/// Teleport a mobile entity to a new position without passability checks.
///
/// Updates the entity's x/y/z directly in the store.  If `direction` is
/// `Some`, the entity's facing is also updated.  Emits `EntityMoved` if
/// the entity exists and is a mobile.
pub(super) fn handle_teleport<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial: u32,
    x: u16,
    y: u16,
    z: i8,
    direction: Option<u8>,
) {
    // Capture old position (shared borrow).
    let old_mpos = zone.get(serial).and_then(|e| {
        if let DemoEntity::Mobile(m) = e {
            Some(MobilePos::new(m.x, m.y, m.z, Facing::new(m.direction)))
        } else {
            None
        }
    });

    if let Some(old_pos) = old_mpos {
        // Use zone.move_entity() to update store + spatial index.
        zone.move_entity(serial, x, y, z, direction);
        trace!(
            "[shadow] teleport 0x{:08X} ({},{},{}) -> ({},{},{}){}",
            serial, old_pos.x, old_pos.y, old_pos.z, x, y, z,
            if direction.is_some() { format!(" dir={}", direction.unwrap()) } else { String::new() },
        );

        // Snapshot after mutation (shared borrow again).
        let snap = zone.get(serial).and_then(|e| e.snapshot());
        let new_pos = MobilePos {
            x, y, z,
            facing: direction.map(Facing::new).unwrap_or(old_pos.facing),
        };
        let _ = event_tx.send(WorldEvent::EntityMoved {
            map_id: zone.map_id,
            serial,
            old_pos,
            new_pos,
            entity: snap,
            is_teleport: true,
        });
    } else if zone.get(serial).is_some() {
        trace!(
            "[shadow] teleport 0x{:08X} -- not a mobile, ignoring",
            serial,
        );
    }
}
