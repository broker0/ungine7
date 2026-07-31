//! Standard house definitions for the demo server.
//!
//! A house is a standard multi (from `multi.mul`) placed in the world via a
//! **house deed**.  This module holds a small static catalogue mapping a
//! deed graphic to a [`HouseDef`] that describes:
//!
//! - the multi id to spawn,
//! - the footprint (relative bounding box) used for placement validation,
//! - the door(s) to spawn, with their relative offsets and graphics,
//! - the sign offset (the sign is what you double-click to manage the house).
//!
//! The catalogue is intentionally tiny (a couple of small houses) — it is a
//! demonstration of the placement + ownership mechanic, not a complete set.

// ── Door graphics ──────────────────────────────────────────────────────────
//
// UO doors come in graphic families.  Each closed graphic has an adjacent
// "open" graphic.  For the door types used here the doors open to the **left**
// (the open-left graphic is the closed graphic + 1).

/// Wooden door, closed, opens left.
pub const DOOR_WOOD_CLOSED: u16 = 0x06A5;
/// Wooden door, open (hinged left).
pub const DOOR_WOOD_OPEN: u16 = 0x06A6;

/// Metal door, closed, opens left.
pub const DOOR_METAL_CLOSED: u16 = 0x0675;
/// Metal door, open (hinged left).
pub const DOOR_METAL_OPEN: u16 = 0x0676;

/// House sign graphics (hanging signs).
pub const SIGN_WOOD: u16 = 0x0BD0;
pub const SIGN_METAL: u16 = 0x0BD2;

/// `ItemProps.meta` key storing the parent house (multi) serial on each
/// player-placed door and sign.  Lets a door/sign click resolve its owning
/// house in O(1) without scanning the map.  Doors/signs from a replay log
/// do not carry this and are treated as ordinary (non-house) items.
pub const META_HOUSE_SERIAL: &str = "house_serial";

// ── Deed graphics ────────────────────────────────────────────────────────────
//
// House deeds are blank scrolls / deeds in the player's pack.  These graphics
// are placeholders for the demo (the deed is consumed on placement and
// returned on demolish).

/// Deed for the small wooden house.
pub const DEED_SMALL_WOOD: u16 = 0x14F0;
/// Deed for the small stone house.
pub const DEED_SMALL_STONE: u16 = 0x14F1;

// ── DoorDef ──────────────────────────────────────────────────────────────────

/// One door of a house, described by its relative offset from the multi origin
/// and its closed/open graphics.
#[derive(Debug, Clone, Copy)]
pub struct DoorDef {
    /// X offset from the multi origin.
    pub dx: i16,
    /// Y offset from the multi origin.
    pub dy: i16,
    /// Z offset from the multi origin.
    pub dz: i8,
    /// Graphic shown when the door is closed.
    pub closed: u16,
    /// Graphic shown when the door is open.
    pub open: u16,
}

// ── HouseDef ───────────────────────────────────────────────────────────────

/// Definition of a standard, placeable house.
#[derive(Debug, Clone, Copy)]
pub struct HouseDef {
    /// Human-readable name.
    pub name: &'static str,
    /// Deed graphic that places this house.
    pub deed_graphic: u16,
    /// Standard multi id (index into `multi.mul`) to spawn.
    pub multi_id: u16,
    /// Footprint bounding box (relative offsets), used for placement checks.
    pub foot_x_min: i16,
    pub foot_y_min: i16,
    pub foot_x_max: i16,
    pub foot_y_max: i16,
    /// Doors to spawn alongside the multi.
    pub doors: &'static [DoorDef],
    /// Sign relative offset.
    pub sign_dx: i16,
    pub sign_dy: i16,
    pub sign_dz: i8,
    /// Sign graphic.
    pub sign_graphic: u16,
}

impl HouseDef {
    /// Footprint width in tiles.
    #[allow(dead_code)]
    pub fn width(&self) -> i16 {
        self.foot_x_max - self.foot_x_min + 1
    }

    /// Footprint height in tiles.
    #[allow(dead_code)]
    pub fn height(&self) -> i16 {
        self.foot_y_max - self.foot_y_min + 1
    }
}

// ── Catalogue ────────────────────────────────────────────────────────────────

/// All placeable houses.
static HOUSES: &[HouseDef] = &[
    // Small wooden house (7x7 stone-and-plaster, multi 0x0064 in classic UO).
    HouseDef {
        name: "a small brick house",
        deed_graphic: DEED_SMALL_WOOD,
        multi_id: 0x0068,
        foot_x_min: -3,
        foot_y_min: -3,
        foot_x_max: 3,
        foot_y_max: 3,
        doors: &[DoorDef {
            dx: 0,
            dy: 3,
            dz: 7,
            closed: DOOR_WOOD_CLOSED,
            open: DOOR_WOOD_OPEN,
        }],
        sign_dx: 2,
        sign_dy: 4,
        sign_dz: 5,
        sign_graphic: SIGN_WOOD,
    },
    // Small stone house (multi 0x0065 in classic UO).
    HouseDef {
        name: "a small wooden house",
        deed_graphic: DEED_SMALL_STONE,
        multi_id: 0x006a,
        foot_x_min: -3,
        foot_y_min: -3,
        foot_x_max: 3,
        foot_y_max: 3,
        doors: &[DoorDef {
            dx: 0,
            dy: 3,
            dz: 7,
            closed: DOOR_METAL_CLOSED,
            open: DOOR_METAL_OPEN,
        }],
        sign_dx: 2,
        sign_dy: 4,
        sign_dz: 5,
        sign_graphic: SIGN_METAL,
    },
];

/// Look up a house definition by the deed graphic that places it.
pub fn lookup_by_deed(deed_graphic: u16) -> Option<&'static HouseDef> {
    HOUSES.iter().find(|h| h.deed_graphic == deed_graphic)
}

/// Return all placeable house definitions.
pub fn all() -> &'static [HouseDef] {
    HOUSES
}

/// Look up a door definition (within any house) by its closed/open graphic.
///
/// Returns the `(closed, open)` graphics so the toggle logic can swap them
/// without needing the full house def.
///
/// Superseded by [`crate::doors::classify`], which decodes the open/closed
/// state and hinge direction from any door graphic.  Retained for the static
/// house catalogue.
#[allow(dead_code)]
pub fn door_toggle_graphics(graphic: u16) -> Option<(u16, u16)> {
    for h in HOUSES {
        for d in h.doors {
            if d.closed == graphic {
                return Some((d.closed, d.open));
            }
            if d.open == graphic {
                return Some((d.closed, d.open));
            }
        }
    }
    None
}
