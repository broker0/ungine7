//! Ship (boat) definitions for the demo server.
//!
//! A ship is a standard multi (from `multi.mul`) placed on **water** via a
//! **ship deed** (a "did" / boat deed in the player's pack).  Conceptually a
//! ship is very close to a [`crate::houses`] house: it is a multi with an
//! `owner`, placed by targeting the ground (here: water) with a cursor.
//!
//! The differences from a house, for this first cut:
//!
//! - placement requires **water** under the whole footprint (a house requires
//!   dry, flat land — see [`crate::houses`]);
//! - a ship has a **facing**: it can point North / East / South / West, and
//!   each facing maps to a different multi id in `multi.mul`;
//! - a ship can be **re-deeded** (packed back into its deed) as long as no
//!   one is standing on the deck.
//!
//! Movement (sailing) is intentionally **not** implemented yet — that is a
//! later step.  For now a placed ship is a static, walk-on platform on the
//! water, exactly like a house is on land.
//!
//! ## Footprint and deck height come from the data files
//!
//! Unlike [`crate::houses`], a ship's footprint (bounding box) and deck
//! height are **not** hard-coded.  They are derived at runtime from the
//! multi's parts in `multi.mul` + `tiledata.mul` via
//! [`ShipShape::from_static`].  Each facing is a distinct multi id with its
//! own already-correct (rotated) footprint, so nothing needs to be
//! transposed by hand.
//!
//! ## Headings
//!
//! | index | facing |
//! |-------|--------|
//! | 0     | North  |
//! | 1     | East   |
//! | 2     | South  |
//! | 3     | West   |

use files::tiledata::TileFlags;
use framework::ecumene::StaticDataProvider;

// ── Deed graphic ──────────────────────────────────────────────────────────

/// Deed graphic for the small boat ("a small ship").
///
/// `0x14F2` is the classic UO "ship deed" item graphic.
pub const DEED_SMALL_SHIP: u16 = 0x14F2;

// ── Ship-component meta keys ────────────────────────────────────────────────
//
// Tillerman, planks and the cargo hold are spawned as ordinary item entities
// whose serials are stored on the ship `Multi` (planks + hold in
// `door_serials`, tillerman in `sign_serial`).  Each child carries the
// following `ItemProps.meta` so the engine (which has no access to this
// demo-server catalogue) can move/turn it correctly:
//
// - `META_SHIP_SERIAL` — parent ship multi serial.
// - `META_SHIP_ROLE`   — one of [`ROLE_TILLER`], [`ROLE_PLANK_PORT`],
//   [`ROLE_PLANK_STAR`], [`ROLE_HOLD`].
// - `META_SHIP_GFX_N/E/S/W` — the child graphic for each ship heading, so a
//   turn can swap the child art without consulting the catalogue.  For a
//   plank the stored value is its *closed* graphic for that heading; the
//   open/closed state is tracked separately (the plank is an even/odd pair
//   like a door — see [`crate::planks`]).

/// `ItemProps.meta` key: serial of the parent ship multi.
pub const META_SHIP_SERIAL: &str = "ship_serial";
/// `ItemProps.meta` key: the component role (see `ROLE_*`).
pub const META_SHIP_ROLE: &str = "ship_role";
/// `ItemProps.meta` keys: child graphic per heading (closed graphic for planks).
pub const META_SHIP_GFX_N: &str = "ship_gfx_n";
pub const META_SHIP_GFX_E: &str = "ship_gfx_e";
pub const META_SHIP_GFX_S: &str = "ship_gfx_s";
pub const META_SHIP_GFX_W: &str = "ship_gfx_w";

/// `ItemProps.meta` key on the **ship multi**: the current heading index
/// (`0..=3`, see [`ShipHeading`]).  Maintained on placement and every turn so
/// the engine can resolve child graphics for the post-turn heading without
/// mapping the hull graphic back to a heading.
pub const META_SHIP_HEADING: &str = "ship_heading";

/// Component role values stored under [`META_SHIP_ROLE`].
pub const ROLE_TILLER: &str = "tiller";
pub const ROLE_PLANK_PORT: &str = "plank_port";
pub const ROLE_PLANK_STAR: &str = "plank_star";
pub const ROLE_HOLD: &str = "hold";

/// Return the per-heading meta key for graphic storage.
pub const fn gfx_meta_key(heading: ShipHeading) -> &'static str {
    match heading {
        ShipHeading::North => META_SHIP_GFX_N,
        ShipHeading::East => META_SHIP_GFX_E,
        ShipHeading::South => META_SHIP_GFX_S,
        ShipHeading::West => META_SHIP_GFX_W,
    }
}

// ── Heading ─────────────────────────────────────────────────────────────────

/// A ship facing: which way the bow points.
///
/// The numeric value is the index into [`ShipDef::multi_ids`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipHeading {
    North = 0,
    East = 1,
    South = 2,
    West = 3,
}

impl ShipHeading {
    /// All four headings in index order.
    pub const ALL: [ShipHeading; 4] =
        [ShipHeading::North, ShipHeading::East, ShipHeading::South, ShipHeading::West];

    /// The facing index (0..=3).
    pub fn index(self) -> usize {
        self as usize
    }

    /// Build a heading from a raw index (`0..=3`), defaulting to North.
    pub fn from_index(i: usize) -> ShipHeading {
        Self::ALL.get(i).copied().unwrap_or(ShipHeading::North)
    }

    /// Turn the ship 90° to port (left / counter-clockwise).
    pub fn turn_left(self) -> ShipHeading {
        Self::from_index((self.index() + 3) % 4)
    }

    /// Turn the ship 90° to starboard (right / clockwise).
    pub fn turn_right(self) -> ShipHeading {
        Self::from_index((self.index() + 1) % 4)
    }

    /// The (dx, dy) delta for one tile of forward movement.
    pub fn delta(self) -> (i32, i32) {
        match self {
            ShipHeading::North => (0, -1),
            ShipHeading::East  => (1, 0),
            ShipHeading::South => (0, 1),
            ShipHeading::West  => (-1, 0),
        }
    }

    /// Resolve the heading that the given multi `graphic` represents.
    ///
    /// Returns `None` if the graphic is not a ship multi.
    pub fn from_multi_graphic(graphic: u16) -> Option<ShipHeading> {
        let def = lookup_by_multi(graphic)?;
        def.multi_ids.iter().position(|&id| id == graphic)
            .map(ShipHeading::from_index)
    }

    /// Parse a heading from a speech command keyword.
    pub fn from_keyword(kw: &str) -> Option<ShipHeading> {
        match kw {
            "north" => Some(ShipHeading::North),
            "east"  => Some(ShipHeading::East),
            "south" => Some(ShipHeading::South),
            "west"  => Some(ShipHeading::West),
            _ => None,
        }
    }
}

// ── ShipDef ───────────────────────────────────────────────────────────────

/// Definition of a placeable ship.
///
/// Only the identity of the ship is stored here; geometry (footprint, deck
/// height) is read from the data files via [`ShipShape::from_static`].
#[derive(Debug, Clone, Copy)]
pub struct ShipDef {
    /// Human-readable name.
    pub name: &'static str,
    /// Deed graphic that places this ship.
    pub deed_graphic: u16,
    /// Multi ids (indices into `multi.mul`) per [`ShipHeading`]:
    /// `[North, East, South, West]`.
    pub multi_ids: [u16; 4],
    /// The tillerman component (a single fixed graphic per heading).
    pub tillerman: ComponentDef,
    /// The port (left) plank.
    pub plank_port: ComponentDef,
    /// The starboard (right) plank.
    pub plank_star: ComponentDef,
    /// The cargo hold (a container).
    pub hold: ComponentDef,
}

/// Per-heading placement of one ship component (tillerman / plank / hold).
///
/// Both the position offset (relative to the ship origin) and the graphic are
/// indexed by [`ShipHeading`], because the same component sits on a different
/// tile and uses a different art id depending on which way the ship faces.
///
/// For planks `graphics[h]` is the **closed** graphic for heading `h`; the
/// open graphic is `closed | 1` (a door-like even/odd pair, see
/// [`crate::planks`]).
#[derive(Debug, Clone, Copy)]
pub struct ComponentDef {
    /// `(dx, dy)` offset from the ship origin per heading `[N, E, S, W]`.
    pub offsets: [(i16, i16); 4],
    /// Z offset from the ship origin (same for all headings).
    pub dz: i8,
    /// Graphic per heading `[N, E, S, W]`.
    pub graphics: [u16; 4],
}

impl ComponentDef {
    /// `(dx, dy)` offset for a heading.
    pub fn offset(&self, heading: ShipHeading) -> (i16, i16) {
        self.offsets[heading.index()]
    }

    /// Graphic for a heading.
    pub fn graphic(&self, heading: ShipHeading) -> u16 {
        self.graphics[heading.index()]
    }
}

impl ShipDef {
    /// The multi id to spawn for a given facing.
    pub fn multi_id_for(&self, heading: ShipHeading) -> u16 {
        self.multi_ids[heading.index()]
    }

    /// Returns `true` if `multi_id` is one of this ship's facing multis.
    pub fn has_multi_id(&self, multi_id: u16) -> bool {
        self.multi_ids.contains(&multi_id)
    }
}

// ── ShipShape (computed from the data files) ──────────────────────────────

/// The geometry of one ship facing, derived from `multi.mul` + `tiledata.mul`.
///
/// - `foot_*` is the relative bounding box of all multi parts.
/// - `deck_rel_z` is how far the walkable deck surface stands **above** the
///   multi origin (so a mobile on the deck reports `origin_z + deck_rel_z`).
#[derive(Debug, Clone, Copy)]
pub struct ShipShape {
    pub foot_x_min: i16,
    pub foot_y_min: i16,
    pub foot_x_max: i16,
    pub foot_y_max: i16,
    pub deck_rel_z: i8,
}

impl ShipShape {
    /// Compute the shape of a ship multi from the static data files.
    ///
    /// The footprint is the bounding box over **all** parts (including
    /// `flags == 0` hull edges) so the whole hull is validated against the
    /// water.  The deck height is the maximum standing height of the
    /// walkable (`SURFACE` and not `IMPASSABLE`) parts — `part.z +
    /// tile.height`.  For the classic small boat every part sits at part
    /// `z = 0` with deck tiles of `tiledata` height `3`, giving
    /// `deck_rel_z = 3`.
    ///
    /// Returns `None` if the multi has no parts (unknown multi id) or no
    /// static data is loaded.
    pub fn from_static(
        multi_id: u16,
        static_data: &dyn StaticDataProvider,
    ) -> Option<ShipShape> {
        let parts = static_data.multi_parts(multi_id);
        if parts.is_empty() {
            return None;
        }

        let mut x_min = i16::MAX;
        let mut y_min = i16::MAX;
        let mut x_max = i16::MIN;
        let mut y_max = i16::MIN;
        let mut deck_rel_z: i8 = 0;

        for part in parts {
            x_min = x_min.min(part.x);
            y_min = y_min.min(part.y);
            x_max = x_max.max(part.x);
            y_max = y_max.max(part.y);

            // Walkable deck surface: SURFACE flag set, IMPASSABLE not set.
            if let Some(def) = static_data.static_tile_def(part.tile_id) {
                let flags = def.flags;
                if flags.has(TileFlags::SURFACE) && !flags.has(TileFlags::IMPASSABLE) {
                    let part_z = part.z.clamp(i8::MIN as i16, i8::MAX as i16) as i8;
                    let stand = part_z.saturating_add(def.height as i8);
                    deck_rel_z = deck_rel_z.max(stand);
                }
            }
        }

        Some(ShipShape {
            foot_x_min: x_min,
            foot_y_min: y_min,
            foot_x_max: x_max,
            foot_y_max: y_max,
            deck_rel_z,
        })
    }
}

// ── Catalogue ────────────────────────────────────────────────────────────────

/// All placeable ships.
///
/// The demo ships with a single small boat.  The four multi ids are the
/// classic UO small-boat facings (North / East / South / West indices into
/// `multi.mul`): `0x0014..=0x0017`.
///
/// Component graphics and offsets are the classic small-boat values.  Planks
/// store their **closed** graphic per heading (the open graphic is `closed |
/// 1`).  The offsets place the tillerman at the stern, the two planks on the
/// port/starboard sides amidships, and the hold over the centre of the deck.
static SHIPS: &[ShipDef] = &[ShipDef {
    name: "a small ship",
    deed_graphic: DEED_SMALL_SHIP,
    multi_ids: [0x0014, 0x0015, 0x0016, 0x0017],
    // Tillerman: stern-mounted; classic small-boat tiller art per facing.
    tillerman: ComponentDef {
        // Stern is opposite the bow: N bow → stern at +y, etc.
        offsets: [(0, 2), (-2, 0), (0, -2), (2, 0)],
        dz: 0,
        graphics: [0x3E4B, 0x3E4E, 0x3E50, 0x3E53],
    },
    // Port plank (left side of the hull when facing the bow).
    // Closed graphics are even; open is `closed | 1`.
    plank_port: ComponentDef {
        offsets: [(-1, 0), (0, -1), (1, 0), (0, 1)],
        dz: 2,
        graphics: [0x3ED4, 0x3EE0, 0x3EEA, 0x3EF2],
    },
    // Starboard plank (right side).
    plank_star: ComponentDef {
        offsets: [(1, 0), (0, 1), (-1, 0), (0, -1)],
        dz: 2,
        graphics: [0x3EDA, 0x3EE6, 0x3EEE, 0x3EF8],
    },
    // Cargo hold: centred on the deck.
    hold: ComponentDef {
        offsets: [(0, 0), (0, 0), (0, 0), (0, 0)],
        dz: 2,
        graphics: [0x3EAE, 0x3EAE, 0x3EAE, 0x3EAE],
    },
}];

/// Look up a ship definition by the deed graphic that places it.
pub fn lookup_by_deed(deed_graphic: u16) -> Option<&'static ShipDef> {
    SHIPS.iter().find(|s| s.deed_graphic == deed_graphic)
}

/// Look up a ship definition by any of its facing multi ids.
pub fn lookup_by_multi(multi_id: u16) -> Option<&'static ShipDef> {
    SHIPS.iter().find(|s| s.has_multi_id(multi_id))
}

/// Return all placeable ship definitions.
#[allow(dead_code)]
pub fn all() -> &'static [ShipDef] {
    SHIPS
}
