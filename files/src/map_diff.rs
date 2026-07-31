//! Map & statics diff reader (`mapdif{N}.mul`, `mapdifl{N}.mul`,
//! `stadif{N}.mul`, `stadifi{N}.mul`, `stadifl{N}.mul`).
//!
//! When the server sends `0xBF` sub-command `0x0018` (`EnableMapDiff`)
//! the client is expected to load diff/patch files from the data
//! directory and use them to **override** specific 8×8 blocks in the
//! base map and statics data.
//!
//! # File sets
//!
//! ## Map diffs
//!
//! | File | Format | Content |
//! |------|--------|---------|
//! | `mapdifl{N}.mul` | Array of `u32` | Block indices that are overridden |
//! | `mapdif{N}.mul`  | Array of `MapBlock` (196 bytes each) | Replacement blocks |
//!
//! The number of entries to read is `map_patches` from the
//! `EnableMapDiff` packet.
//!
//! ## Static diffs
//!
//! | File | Format | Content |
//! |------|--------|---------|
//! | `stadifl{N}.mul` | Array of `u32` | Block indices that are overridden |
//! | `stadifi{N}.mul` | Standard MUL index (12-byte records) | Per-patch offset/length into `stadif{N}.mul` |
//! | `stadif{N}.mul`  | Flat array of 7-byte `StaticTile` records | Replacement tiles |
//!
//! The number of entries to read is `static_patches` from the
//! `EnableMapDiff` packet.
//!
//! # Usage
//!
//! ```ignore
//! use files::map_diff;
//!
//! // From the EnableMapDiff packet:
//! let map_patches = 42;
//! let static_patches = 17;
//!
//! let map_diff = map_diff::read_map_diff(data_dir, 0, map_patches)?;
//! let static_diff = map_diff::read_static_diff(data_dir, 0, static_patches)?;
//!
//! // Query overrides by block index:
//! if let Some(block) = map_diff.get(block_index) { /* use patched block */ }
//! if let Some(tiles) = static_diff.get(block_index) { /* use patched tiles */ }
//! ```

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use log::debug;
use u_io::{Decode, StreamReader, LE};

use crate::map::MapBlock;
use crate::mul::MulIndex;
use crate::statics::{StaticTile, TILE_DISK_SIZE};

// ── MapDiffData ───────────────────────────────────────────────────────────

/// Loaded map diff data — overrides specific blocks in the base map.
///
/// Immutable after construction.  Use [`read_map_diff`] to load from disk.
#[derive(Debug, Clone)]
pub struct MapDiffData {
    overrides: HashMap<usize, MapBlock>,
}

impl MapDiffData {
    /// Create an empty diff (no overrides).
    pub fn empty() -> Self {
        Self {
            overrides: HashMap::new(),
        }
    }

    /// Number of overridden blocks.
    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    /// Whether there are no overrides.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    /// Get a replacement block by its linear block index.
    ///
    /// Returns `None` if this block is not overridden.
    pub fn get(&self, block_index: usize) -> Option<&MapBlock> {
        self.overrides.get(&block_index)
    }

    /// Get a replacement block by block coordinates.
    ///
    /// `y_blocks` is the height of the map in blocks (needed for linear
    /// index calculation: `block_index = bx * y_blocks + by`).
    pub fn get_at(&self, bx: usize, by: usize, y_blocks: usize) -> Option<&MapBlock> {
        self.overrides.get(&(bx * y_blocks + by))
    }

    /// Iterate over all overrides as `(block_index, &MapBlock)`.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &MapBlock)> {
        self.overrides.iter().map(|(&idx, block)| (idx, block))
    }
}

// ── StaticDiffData ────────────────────────────────────────────────────────

/// Loaded statics diff data — overrides specific blocks in the base statics.
///
/// Tiles within each overridden block are sorted by `(x, y, z)` at load
/// time, matching the convention of [`StaticData`](crate::statics::StaticData).
///
/// Immutable after construction.  Use [`read_static_diff`] to load from disk.
#[derive(Debug, Clone)]
pub struct StaticDiffData {
    overrides: HashMap<usize, Vec<StaticTile>>,
}

impl StaticDiffData {
    /// Create an empty diff (no overrides).
    pub fn empty() -> Self {
        Self {
            overrides: HashMap::new(),
        }
    }

    /// Number of overridden blocks.
    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    /// Whether there are no overrides.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    /// Get replacement tiles for a block by its linear block index.
    ///
    /// Returns `None` if this block is not overridden.
    /// An overridden block may have an empty tile list (meaning the
    /// server cleared all statics from that block).
    pub fn get(&self, block_index: usize) -> Option<&[StaticTile]> {
        self.overrides.get(&block_index).map(|v| v.as_slice())
    }

    /// Get replacement tiles by block coordinates.
    pub fn get_at(&self, bx: usize, by: usize, y_blocks: usize) -> Option<&[StaticTile]> {
        self.overrides
            .get(&(bx * y_blocks + by))
            .map(|v| v.as_slice())
    }

    /// Get tiles at a specific cell `(ox, oy)` within an overridden block.
    ///
    /// Returns `None` if the block is not overridden.
    /// Returns an empty slice if the block is overridden but has no tiles
    /// at the given cell.
    pub fn get_tile(&self, block_index: usize, ox: u8, oy: u8) -> Option<&[StaticTile]> {
        let tiles = self.overrides.get(&block_index)?;
        if tiles.is_empty() {
            return Some(&[]);
        }
        // Tiles are sorted by (x, y, z) — binary search for the cell range.
        let key = (ox, oy);
        let cmp = |t: &StaticTile| (t.x, t.y).cmp(&key);
        let left = tiles.partition_point(|t| cmp(t) == std::cmp::Ordering::Less);
        let rest = &tiles[left..];
        let count = rest.partition_point(|t| cmp(t) == std::cmp::Ordering::Equal);
        Some(&rest[..count])
    }

    /// Iterate over all overrides as `(block_index, &[StaticTile])`.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &[StaticTile])> {
        self.overrides
            .iter()
            .map(|(&idx, tiles)| (idx, tiles.as_slice()))
    }
}

// ── Loaders ───────────────────────────────────────────────────────────────

/// Load map diff files for the given world.
///
/// Reads `map_patches` entries from `mapdifl{world}.mul` (block indices)
/// and the corresponding replacement blocks from `mapdif{world}.mul`.
///
/// Returns [`MapDiffData::empty()`] if `map_patches` is 0.
pub fn read_map_diff(dir: &Path, world: u8, map_patches: u32) -> io::Result<MapDiffData> {
    if map_patches == 0 {
        debug!("mapdifl{world}.mul: 0 patches, skipping");
        return Ok(MapDiffData::empty());
    }

    let difl_path = dir.join(format!("mapdifl{world}.mul"));
    let dif_path = dir.join(format!("mapdif{world}.mul"));

    debug!(
        "mapdifl{world}.mul: loading {} patches from {}",
        map_patches,
        difl_path.display(),
    );

    // Read block indices from mapdifl.
    let difl_file = File::open(&difl_path)?;
    let mut difl_reader = StreamReader::<_, LE>::new(BufReader::new(difl_file));

    let mut block_indices = Vec::with_capacity(map_patches as usize);
    for _ in 0..map_patches {
        let idx: u32 = Decode::decode(&mut difl_reader)?;
        block_indices.push(idx as usize);
    }

    // Read replacement blocks from mapdif.
    let dif_file = File::open(&dif_path)?;
    let mut dif_reader = StreamReader::<_, LE>::new(BufReader::new(dif_file));

    let mut overrides = HashMap::with_capacity(map_patches as usize);
    for block_index in block_indices {
        let block = MapBlock::decode_from(&mut dif_reader)?;
        overrides.insert(block_index, block);
    }

    debug!(
        "mapdifl{world}.mul + mapdif{world}.mul: {map_patches} patches, \
         {} unique blocks",
        overrides.len(),
    );

    Ok(MapDiffData { overrides })
}

/// Load static diff files for the given world.
///
/// Reads `static_patches` block indices from `stadifl{world}.mul`, the
/// corresponding MUL index from `stadifi{world}.mul`, and replacement
/// tile data from `stadif{world}.mul`.
///
/// Returns [`StaticDiffData::empty()`] if `static_patches` is 0.
pub fn read_static_diff(dir: &Path, world: u8, static_patches: u32) -> io::Result<StaticDiffData> {
    if static_patches == 0 {
        debug!("stadifl{world}.mul: 0 patches, skipping");
        return Ok(StaticDiffData::empty());
    }

    let difl_path = dir.join(format!("stadifl{world}.mul"));
    let difi_path = dir.join(format!("stadifi{world}.mul"));
    let dif_path = dir.join(format!("stadif{world}.mul"));

    debug!(
        "stadifl{world}.mul: loading {} patches from {}",
        static_patches,
        difl_path.display(),
    );

    // 1. Read block indices from stadifl.
    let difl_file = File::open(&difl_path)?;
    let mut difl_reader = StreamReader::<_, LE>::new(BufReader::new(difl_file));

    let mut block_indices = Vec::with_capacity(static_patches as usize);
    for _ in 0..static_patches {
        let idx: u32 = Decode::decode(&mut difl_reader)?;
        block_indices.push(idx as usize);
    }

    // 2. Read MUL index from stadifi.
    let difi_index = MulIndex::read(&difi_path)?;

    // 3. Read replacement tiles from stadif using the index.
    let dif_file = File::open(&dif_path)?;
    let mut dif_reader = StreamReader::<_, LE>::new(BufReader::new(dif_file));

    let mut overrides = HashMap::with_capacity(static_patches as usize);
    for (i, &block_index) in block_indices.iter().enumerate() {
        let entry = match difi_index.get(i) {
            Some(e) if e.is_valid() => e,
            _ => {
                // Invalid index entry — treat as empty block (all statics cleared).
                overrides.insert(block_index, Vec::new());
                continue;
            }
        };

        let count = entry.length as usize / TILE_DISK_SIZE;
        if count == 0 {
            overrides.insert(block_index, Vec::new());
            continue;
        }

        dif_reader.seek_to(entry.offset as u64)?;

        let mut tiles = Vec::with_capacity(count);
        for _ in 0..count {
            tiles.push(StaticTile::decode(&mut dif_reader)?);
        }

        // Sort by (x, y, z) to match StaticData convention.
        tiles.sort_by(|a, b| (a.x, a.y, a.z).cmp(&(b.x, b.y, b.z)));

        overrides.insert(block_index, tiles);
    }

    let total_tiles: usize = overrides.values().map(|v| v.len()).sum();
    debug!(
        "stadifl{world}.mul + stadifi{world}.mul + stadif{world}.mul: \
         {static_patches} patches, {} unique blocks, {} total tiles",
        overrides.len(),
        total_tiles,
    );

    Ok(StaticDiffData { overrides })
}
