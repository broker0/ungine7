//! Multi-world teleporter objects.
//!
//! A *teleporter* is an ordinary world item carrying four metadata keys in
//! its [`ItemProps`]:
//!
//! - [`META_TP_WORLD`] — destination map/facet id (`u8`)
//! - [`META_TP_X`] / [`META_TP_Y`] / [`META_TP_Z`] — destination coordinates
//!
//! When a player steps onto a teleporter's tile (or double-clicks it), the
//! session reads these keys and moves the player to the destination — using
//! the atomic cross-zone [`transfer_player`](crate::game_session::transfer)
//! when the destination world differs from the current one, or a plain
//! intra-zone teleport otherwise.
//!
//! Unlike doors or ships, teleporters need no per-tick servicing: they are
//! passive triggers evaluated entirely on the session side after a move, so
//! this module only defines the metadata keys plus small read helpers.

use common::uo_engine::item_props::{ItemProps, MetaValue};

// ── ItemProps meta keys ─────────────────────────────────────────────────────

/// Destination world (map/facet id) of a teleporter object.
///
/// The key is `"teleport_map"` so that Rust-side teleporters
/// (`.maketele`) and the Lua `teleporter.lua` controller share a single
/// meta format.
pub const META_TP_WORLD: &str = "teleport_map";
/// Destination X coordinate.
pub const META_TP_X: &str = "teleport_x";
/// Destination Y coordinate.
pub const META_TP_Y: &str = "teleport_y";
/// Destination Z coordinate.
pub const META_TP_Z: &str = "teleport_z";

/// Optional filter controlling which mobiles a teleporter transports.
///
/// Value is a string: `"players"` (default), `"all"`, or `"no_pets"`.
pub const META_TP_FILTER: &str = "teleport_filter";

// ── Destination ──────────────────────────────────────────────────────────────

/// A resolved teleporter destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeleportDest {
    pub world: u8,
    pub x: u16,
    pub y: u16,
    pub z: i8,
}

/// Read a teleporter destination from an item's [`ItemProps`].
///
/// Returns `None` if the props do not describe a teleporter (i.e. the
/// destination-coordinate keys are absent).  The world key is optional and
/// defaults to `current_world` so a same-map teleporter can omit it.
pub fn dest_from_props(props: &ItemProps, current_world: u8) -> Option<TeleportDest> {
    let x = props.get_meta_int(META_TP_X)?;
    let y = props.get_meta_int(META_TP_Y)?;
    let z = props.get_meta_int(META_TP_Z)?;
    let world = props
        .get_meta_int(META_TP_WORLD)
        .map(|w| w as u8)
        .unwrap_or(current_world);
    Some(TeleportDest {
        world,
        x: x as u16,
        y: y as u16,
        z: z as i8,
    })
}

/// Build [`ItemProps`] describing a teleporter to `dest`, preserving any
/// existing props (name, other meta) passed in.
pub fn write_dest(mut props: ItemProps, dest: TeleportDest) -> ItemProps {
    props.set_meta(META_TP_WORLD, MetaValue::Int(dest.world as i64));
    props.set_meta(META_TP_X, MetaValue::Int(dest.x as i64));
    props.set_meta(META_TP_Y, MetaValue::Int(dest.y as i64));
    props.set_meta(META_TP_Z, MetaValue::Int(dest.z as i64));
    props
}

/// Graphic used for teleporter objects placed via the `.maketele` command.
///
/// `0x1BC3` is the classic UO "teleporter" tile (a faint glowing pad).  Any
/// graphic works — the teleporter behaviour is driven by the meta keys, not
/// the graphic — but a recognisable tile helps mapping.
pub const TELEPORTER_GRAPHIC: u16 = 0x1BC3;

/// Which mobiles a teleporter is allowed to transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleportFilter {
    /// Only player characters (default).
    Players,
    /// Any mobile (players, pets, NPCs, monsters).
    All,
    /// Players and unowned mobiles, but not tamed pets.
    NoPets,
}

impl TeleportFilter {
    /// Parse a filter from its meta string value; unknown / missing values
    /// fall back to [`TeleportFilter::Players`].
    pub fn from_meta(value: Option<&str>) -> Self {
        match value {
            Some("all") => TeleportFilter::All,
            Some("no_pets") => TeleportFilter::NoPets,
            _ => TeleportFilter::Players,
        }
    }
}

/// Read the teleport filter from an item's [`ItemProps`].
pub fn filter_from_props(props: &ItemProps) -> TeleportFilter {
    TeleportFilter::from_meta(props.get_meta_str(META_TP_FILTER))
}
