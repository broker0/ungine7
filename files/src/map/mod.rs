//! Map/terrain reader (`map{N}.mul` or `map{N}LegacyMUL.uop`).
//!
//! The world map is stored as a grid of 8×8 tile blocks.  Each block
//! contains a 4-byte header (unused) followed by 64 tile cells in
//! column-major order (`[x][y]`).
//!
//! # Tile layout
//!
//! Each tile is 3 bytes: `tile_id` (`u16`) + `z` (`i8`).
//! A block is therefore `4 + 64 * 3 = 196` bytes on disk.
//!
//! # File variants
//!
//! | Source | File pattern | Notes |
//! |--------|-------------|-------|
//! | MUL    | `map{N}.mul` | Sequential blocks, no index file |
//! | UOP    | `map{N}LegacyMUL.uop` | Blocks packed into UOP chunks |
//!
//! # Standard map dimensions (in 8×8 blocks)
//!
//! | Map | X blocks | Y blocks | Name |
//! |-----|----------|----------|------|
//! | 0   | 768      | 512      | Felucca / Trammel |
//! | 1   | 768      | 512      | (alt. Felucca) |
//! | 2   | 288      | 200      | Ilshenar |
//! | 3   | 320      | 256      | Malas |
//! | 4   | 181      | 181      | Tokuno Islands |
//! | 5   | 160      | 512      | Ter Mur |
//!
//! # Loading
//!
//! ```ignore
//! use files::map;
//!
//! // Auto-detect format (tries UOP first, falls back to MUL):
//! let data = map::read(dir, 0)?;
//!
//! // Explicit format:
//! let data = map::mul::read(dir, 0)?;
//! let data = map::uop::read(dir, 0)?;
//! ```

pub mod mul;
pub mod uop;

use std::io::{self, Read};
use std::path::Path;

use u_io::{Decode, DecodeError, ReadPrimitives, StreamReader, LE};
use macros::{Decode, Encode};

/// Resolve dimensions for a world index, returning an error if unknown.
pub(crate) fn resolve_dimensions(world: u8) -> io::Result<(usize, usize)> {
    default_dimensions(world).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown world index {world}, pass dimensions explicitly"),
        )
    })
}

// ── Constants ─────────────────────────────────────────────────────────────

/// Tiles per block axis.
const BLOCK_DIM: usize = 8;

/// Block header size in bytes (u32, unused).
const BLOCK_HEADER_SIZE: usize = 4;

/// Single tile size on disk: tile_id(2) + z(1) = 3 bytes.
const TILE_DISK_SIZE: usize = 3;

/// Entire block size on disk: header(4) + 64 tiles × 3 bytes = 196 bytes.
pub(crate) const BLOCK_DISK_SIZE: usize =
    BLOCK_HEADER_SIZE + BLOCK_DIM * BLOCK_DIM * TILE_DISK_SIZE;

// ── MapTile ───────────────────────────────────────────────────────────────

/// A single map tile: land graphic ID and elevation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode, Encode)]
#[binary(endian = "le")]
pub struct MapTile {
    pub tile_id: u16,
    pub z: i8,
}

// ── MapBlock ──────────────────────────────────────────────────────────────

/// An 8×8 block of map tiles, stored column-major: `cells[x][y]`.
#[derive(Debug, Clone)]
pub struct MapBlock {
    pub cells: [[MapTile; BLOCK_DIM]; BLOCK_DIM],
}

impl MapBlock {
    const EMPTY_TILE: MapTile = MapTile { tile_id: 0, z: 0 };

    fn new_empty() -> Self {
        Self {
            cells: [[Self::EMPTY_TILE; BLOCK_DIM]; BLOCK_DIM],
        }
    }

    /// Decode a block from a reader (reads 4-byte header + 64 tiles).
    pub(crate) fn decode_from<R: ReadPrimitives<LE>>(reader: &mut R) -> Result<Self, DecodeError> {
        let _header = u32::decode(reader)?;

        let mut block = Self::new_empty();
        for y in 0..BLOCK_DIM {
            for x in 0..BLOCK_DIM {
                block.cells[x][y] = MapTile::decode(reader)?;
            }
        }
        Ok(block)
    }
}

// ── Standard map dimensions ───────────────────────────────────────────────

/// Standard map dimensions as `(x_blocks, y_blocks)`.
///
/// Returns `None` for unknown world indices.
pub fn default_dimensions(world: u8) -> Option<(usize, usize)> {
    match world {
        0 => Some((768, 512)),
        1 => Some((768, 512)),
        2 => Some((288, 200)),
        3 => Some((320, 256)),
        4 => Some((181, 181)),
        5 => Some((160, 512)),
        _ => None,
    }
}

/// Default map dimensions in **tiles** (pixels), `(width, height)`.
///
/// These are the fallback values used when the actual map data is not
/// loaded.  The values correspond to `default_dimensions(world) * 8`.
///
/// Returns `None` for unknown world indices.
pub fn default_tile_dimensions(world: u8) -> Option<(u16, u16)> {
    default_dimensions(world).map(|(xb, yb)| ((xb * 8) as u16, (yb * 8) as u16))
}

// ── MapData ───────────────────────────────────────────────────────────────

/// Loaded map data — all blocks in memory.
///
/// This is a pure data container with no knowledge of the source format.
/// Use [`mul::read`], [`uop::read`], or the convenience [`read`] function
/// to load from disk.
#[derive(Debug)]
pub struct MapData {
    x_blocks: usize,
    y_blocks: usize,
    blocks: Vec<MapBlock>,
}

impl MapData {
    /// Create from pre-parsed blocks.
    pub(crate) fn new(blocks: Vec<MapBlock>, x_blocks: usize, y_blocks: usize) -> Self {
        Self { x_blocks, y_blocks, blocks }
    }

    /// Parse from any readable stream (sequential MUL-style blocks).
    ///
    /// Useful for testing or when the data is already in memory.
    pub fn from_stream<S: Read>(
        stream: S,
        x_blocks: usize,
        y_blocks: usize,
    ) -> Result<Self, DecodeError> {
        let total = x_blocks * y_blocks;
        let mut reader = StreamReader::<_, LE>::new(stream);
        let mut blocks = Vec::with_capacity(total);

        for _ in 0..total {
            blocks.push(MapBlock::decode_from(&mut reader)?);
        }

        Ok(Self { x_blocks, y_blocks, blocks })
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// Total number of blocks (same as [`total_blocks`](Self::total_blocks)).
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the map has no blocks.
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

    /// Total number of blocks.
    pub fn total_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Width of the map in tiles.
    pub fn width(&self) -> usize {
        self.x_blocks * BLOCK_DIM
    }

    /// Height of the map in tiles.
    pub fn height(&self) -> usize {
        self.y_blocks * BLOCK_DIM
    }

    /// Get a block by its linear index (column-major: x varies first).
    pub fn block(&self, index: usize) -> &MapBlock {
        &self.blocks[index]
    }

    /// Get a block by its block coordinates.
    pub fn block_at(&self, bx: usize, by: usize) -> &MapBlock {
        debug_assert!(bx < self.x_blocks && by < self.y_blocks);
        &self.blocks[bx * self.y_blocks + by]
    }

    /// Get a single tile by its absolute tile coordinates.
    pub fn tile_at(&self, x: usize, y: usize) -> &MapTile {
        let bx = x / BLOCK_DIM;
        let by = y / BLOCK_DIM;
        let tx = x % BLOCK_DIM;
        let ty = y % BLOCK_DIM;
        &self.block_at(bx, by).cells[tx][ty]
    }

    /// Iterate over all blocks with their `(bx, by)` coordinates.
    pub fn iter_blocks(&self) -> impl Iterator<Item = (usize, usize, &MapBlock)> {
        self.blocks.iter().enumerate().map(move |(i, block)| {
            let bx = i / self.y_blocks;
            let by = i % self.y_blocks;
            (bx, by, block)
        })
    }
}

// ── Auto-detect loader ───────────────────────────────────────────────────

/// Load a map, auto-detecting the source format.
///
/// Tries UOP first (`map{world}LegacyMUL.uop`), falls back to MUL
/// (`map{world}.mul`).  Uses standard dimensions for the given world.
pub fn read(dir: &Path, world: u8) -> io::Result<MapData> {
    let (xb, yb) = resolve_dimensions(world)?;
    let uop_path = dir.join(format!("map{world}LegacyMUL.uop"));
    if uop_path.exists() {
        return uop::read(dir, world, xb, yb);
    }
    mul::read(dir, world, xb, yb)
}
