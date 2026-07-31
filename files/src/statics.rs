//! World statics reader (`staidx{N}.mul` + `statics{N}.mul`).
//!
//! Static objects (trees, rocks, walls, etc.) are overlaid on top of the
//! terrain map.  They are stored per-block (same 8×8 grid as the map),
//! with each block containing zero or more static tiles.
//!
//! # File pair
//!
//! | File | Format | Content |
//! |------|--------|---------|
//! | `staidx{N}.mul` | Standard MUL index (12-byte records) | Per-block offset/length into data file |
//! | `statics{N}.mul` | Flat array of 7-byte tile records | All static tiles, referenced by index |
//!
//! # Tile layout (7 bytes)
//!
//! ```text
//! tile_id : u16   — static tile graphic ID
//! x       : u8    — offset within block (0..7)
//! y       : u8    — offset within block (0..7)
//! z       : i8    — elevation
//! hue     : u16   — color / hue
//! ```
//!
//! Tiles within each block are sorted by `(x, y, z)` at load time for
//! efficient lookup via [`StaticData::block_tile`].

use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, StreamReader, LE};
use macros::{Decode, Encode};

use crate::map::default_dimensions;
use crate::mul::MulIndex;

// ── Constants ─────────────────────────────────────────────────────────────

/// Size of a single static tile on disk (7 bytes).
pub const TILE_DISK_SIZE: usize = 7;

// ── StaticTile ────────────────────────────────────────────────────────────

/// A single static object placed in the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode, Encode)]
#[binary(endian = "le")]
pub struct StaticTile {
    pub tile_id: u16,
    pub x: u8,
    pub y: u8,
    pub z: i8,
    pub hue: u16,
}

// ── StaticData ────────────────────────────────────────────────────────────

/// Loaded world statics — all tiles in a flat array, indexed by block.
///
/// Tiles within each block are sorted by `(x, y, z)` for efficient
/// coordinate lookup.
#[derive(Debug)]
pub struct StaticData {
    tiles: Vec<StaticTile>,
    /// Per-block: `Some((start, count))` into `tiles`, or `None` for empty blocks.
    blocks: Vec<Option<(usize, usize)>>,
    x_blocks: usize,
    y_blocks: usize,
}

impl StaticData {
    /// Load from `staidx{world}.mul` + `statics{world}.mul` using standard dimensions.
    pub fn read(dir: &Path, world: u8) -> io::Result<Self> {
        let (xb, yb) = default_dimensions(world).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown world index {world}, use read_with_size"),
            )
        })?;
        Self::read_with_size(dir, world, xb, yb)
    }

    /// Load with explicit block dimensions.
    pub fn read_with_size(
        dir: &Path,
        world: u8,
        x_blocks: usize,
        y_blocks: usize,
    ) -> io::Result<Self> {
        let idx_path = dir.join(format!("staidx{world}.mul"));
        let mul_path = dir.join(format!("statics{world}.mul"));

        let index = MulIndex::read(&idx_path)?;

        let file = File::open(&mul_path)?;
        let mul_size = file.metadata()?.len();
        let buf = BufReader::new(file);

        debug!(
            "statics{world}.mul: size={mul_size}, index_entries={}, \
             x_blocks={x_blocks}, y_blocks={y_blocks}",
            index.len(),
        );

        Ok(Self::from_stream(&index, buf, x_blocks, y_blocks)?)
    }

    /// Build from a preloaded index and a readable+seekable stream.
    pub fn from_stream<S: Read + Seek>(
        index: &MulIndex,
        stream: S,
        x_blocks: usize,
        y_blocks: usize,
    ) -> Result<Self, DecodeError> {
        let mut reader = StreamReader::<_, LE>::new(stream);
        Self::parse(&mut reader, index, x_blocks, y_blocks)
    }

    fn parse<R: Read + Seek>(
        reader: &mut StreamReader<R, LE>,
        index: &MulIndex,
        x_blocks: usize,
        y_blocks: usize,
    ) -> Result<Self, DecodeError> {
        let total_blocks = x_blocks * y_blocks;

        // Estimate total tiles from index for pre-allocation.
        let estimated_tiles: usize = index
            .iter()
            .filter(|e| e.is_valid())
            .map(|e| e.length as usize / TILE_DISK_SIZE)
            .sum();

        let mut tiles = Vec::with_capacity(estimated_tiles);
        let mut blocks = Vec::with_capacity(total_blocks);

        for i in 0..total_blocks {
            let entry = match index.get(i) {
                Some(e) if e.is_valid() => e,
                _ => {
                    blocks.push(None);
                    continue;
                }
            };

            let count = entry.length as usize / TILE_DISK_SIZE;
            if count == 0 {
                blocks.push(None);
                continue;
            }

            reader.seek_to(entry.offset as u64)?;

            let start = tiles.len();
            for _ in 0..count {
                tiles.push(StaticTile::decode(reader)?);
            }

            // Sort this block's tiles by (x, y, z) for binary search.
            tiles[start..].sort_by(|a, b| {
                (a.x, a.y, a.z).cmp(&(b.x, b.y, b.z))
            });

            blocks.push(Some((start, count)));
        }

        Ok(Self { tiles, blocks, x_blocks, y_blocks })
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// Total number of blocks (same as [`total_blocks`](Self::total_blocks)).
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the static data has no blocks.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Width of the map in 8×8 blocks.
    pub fn x_blocks(&self) -> usize {
        self.x_blocks
    }

    /// Height of the map in 8×8 blocks.
    pub fn y_blocks(&self) -> usize {
        self.y_blocks
    }

    /// Total number of blocks (including empty ones).
    pub fn total_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Total number of static tiles across all blocks.
    pub fn total_tiles(&self) -> usize {
        self.tiles.len()
    }

    /// Number of blocks that contain at least one static tile.
    pub fn non_empty_blocks(&self) -> usize {
        self.blocks.iter().filter(|b| b.is_some()).count()
    }

    /// Get all static tiles in a block by linear index.
    ///
    /// Returns an empty slice for empty or out-of-range blocks.
    pub fn block(&self, index: usize) -> &[StaticTile] {
        match self.blocks.get(index) {
            Some(Some((start, count))) => &self.tiles[*start..*start + *count],
            _ => &[],
        }
    }

    /// Get all static tiles in a block by block coordinates.
    pub fn block_at(&self, bx: usize, by: usize) -> &[StaticTile] {
        debug_assert!(bx < self.x_blocks && by < self.y_blocks);
        self.block(bx * self.y_blocks + by)
    }

    /// Get static tiles at a specific cell within a block.
    ///
    /// `ox` and `oy` are offsets within the block (0..7).
    /// Returns a sub-slice of the block's tiles matching that cell,
    /// leveraging the sorted order for fast lookup.
    pub fn block_tile(&self, index: usize, ox: u8, oy: u8) -> &[StaticTile] {
        let slice = self.block(index);
        if slice.is_empty() {
            return slice;
        }

        let key = (ox, oy);
        let cmp = |t: &StaticTile| (t.x, t.y).cmp(&key);

        // Find the first tile with (x, y) >= key.
        let left = slice.partition_point(|t| cmp(t) == Ordering::Less);
        let rest = &slice[left..];
        // Find where (x, y) > key.
        let count = rest.partition_point(|t| cmp(t) == Ordering::Equal);
        &rest[..count]
    }

    /// Iterate over all blocks, yielding `(block_index, tiles)` pairs.
    ///
    /// Empty blocks are included with an empty slice.
    pub fn iter_blocks(&self) -> impl Iterator<Item = (usize, &[StaticTile])> {
        self.blocks.iter().enumerate().map(move |(i, slot)| {
            let tiles = match slot {
                Some((start, count)) => &self.tiles[*start..*start + *count],
                None => &[],
            };
            (i, tiles)
        })
    }
}
