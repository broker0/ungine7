//! [`MultiDef`] — definition of a multi-object with a per-tile index.
//!
//! A multi-object (house, boat, castle, etc.) consists of many parts —
//! static tiles placed at relative offsets from the multi's origin.
//!
//! `MultiDef` pre-indexes these parts by their `(x, y)` offset so that
//! looking up which parts occupy a specific tile is O(1) via `HashMap`.
//! The bounding box ([`MultiExtent`]) allows fast rejection of queries
//! that fall outside the multi's footprint.
//!
//! Standard multis (from `multi.mul`) and custom houses (from packet
//! `0xD8`) use the same representation — only the source of parts differs.

use std::collections::HashMap;

use files::multi::MultiPart;

// ── PartEntry ────────────────────────────────────────────────────────────

/// A single part of a multi in the per-tile index.
///
/// Stores only the data needed to build a [`TileShape`](super::TileShape)
/// at query time: the static tile graphic and the relative Z offset.
#[derive(Clone, Copy, Debug)]
pub struct PartEntry {
    /// Static tile graphic ID (from `tiledata.mul`).
    pub tile_id: u16,
    /// Z offset relative to the multi's origin.
    pub z: i16,
}

// ── MultiExtent ──────────────────────────────────────────────────────────

/// Bounding box of a multi's parts in relative coordinates.
///
/// All values are offsets from the multi's origin tile.
#[derive(Clone, Copy, Debug, Default)]
pub struct MultiExtent {
    pub x_min: i16,
    pub x_max: i16,
    pub y_min: i16,
    pub y_max: i16,
}

impl MultiExtent {
    /// Check whether a relative offset falls within the bounding box.
    #[inline]
    pub fn contains(&self, dx: i16, dy: i16) -> bool {
        dx >= self.x_min && dx <= self.x_max && dy >= self.y_min && dy <= self.y_max
    }
}

// ── MultiDef ─────────────────────────────────────────────────────────────

/// Definition of a multi-object with a pre-built per-tile index.
///
/// Created once from a list of [`MultiPart`]s and shared via `Arc` when
/// multiple instances of the same standard multi exist in the world.
#[derive(Clone, Debug)]
pub struct MultiDef {
    /// Per-tile index: relative `(x, y)` offset → parts on that tile.
    tiles: HashMap<(i16, i16), Vec<PartEntry>>,
    /// Bounding box of the multi (relative to origin).
    pub extent: MultiExtent,
}

impl MultiDef {
    /// Build a `MultiDef` from a slice of [`MultiPart`]s.
    ///
    /// Parts with `flags == 0` (invisible / excluded) are filtered out.
    /// Used for both standard multis (parts from `MultiCollection`) and
    /// custom houses (parts parsed from packet `0xD8`).
    pub fn from_parts(parts: &[MultiPart]) -> Self {
        let mut tiles: HashMap<(i16, i16), Vec<PartEntry>> = HashMap::new();
        let mut extent = MultiExtent::default();
        let mut has_any = false;

        for part in parts {
            if part.flags == 0 {
                continue;
            }

            let entry = PartEntry {
                tile_id: part.tile_id,
                z: part.z,
            };

            tiles.entry((part.x, part.y)).or_default().push(entry);

            if has_any {
                extent.x_min = extent.x_min.min(part.x);
                extent.x_max = extent.x_max.max(part.x);
                extent.y_min = extent.y_min.min(part.y);
                extent.y_max = extent.y_max.max(part.y);
            } else {
                extent.x_min = part.x;
                extent.x_max = part.x;
                extent.y_min = part.y;
                extent.y_max = part.y;
                has_any = true;
            }
        }

        Self { tiles, extent }
    }

    /// Get the parts at a relative offset `(dx, dy)` from the multi origin.
    ///
    /// Returns an empty slice if no parts exist at that offset.
    #[inline]
    pub fn parts_at(&self, dx: i16, dy: i16) -> &[PartEntry] {
        match self.tiles.get(&(dx, dy)) {
            Some(v) => v,
            None => &[],
        }
    }

    /// Quick bounding-box check for a relative offset.
    #[inline]
    pub fn contains(&self, dx: i16, dy: i16) -> bool {
        self.extent.contains(dx, dy)
    }

    /// Number of unique tiles occupied by this multi.
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Total number of part entries (across all tiles).
    pub fn part_count(&self) -> usize {
        self.tiles.values().map(|v| v.len()).sum()
    }
}
