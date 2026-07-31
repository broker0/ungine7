//! Tile data reader (`tiledata.mul`).
//!
//! `tiledata.mul` contains metadata for every land tile and static object
//! graphic in the game — flags, height, name, animation ID, etc.
//!
//! # Format versions
//!
//! Two layouts exist, distinguished by file size:
//!
//! | Version      | Flags field | File size   | Notes                          |
//! |--------------|-------------|-------------|--------------------------------|
//! | Legacy       | u32 (4 B)  | varies      | Pre-High Seas                  |
//! | 7090+ (HS)   | u64 (8 B)  | 3,188,736 B | High Seas extended flags       |
//!
//! Both versions store tiles in groups of 32, each group prefixed by a
//! 4-byte header (unused).  The first half of the file contains land tiles
//! (always 512 groups = 16,384 tiles), the second half contains static
//! object tiles (group count derived from remaining file size).
//!
//! # Land tile group header quirk (7090)
//!
//! In the legacy format, every 32nd tile is preceded by a group header.
//! In the 7090 format, tile index 0 has no header, but tile index 1 gets
//! an extra header, and then every 32nd tile from there on.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, FixedString, ReadPrimitives, StreamReader, LE};

// ── Constants ──────────────────────────────────────────────────────────────

/// Number of land tile groups (512 groups x 32 tiles = 16,384 land tiles).
pub const LAND_GROUPS: usize = 512;

/// Tiles per group.
pub const TILES_PER_GROUP: usize = 32;

/// Total land tiles.
pub const LAND_TILE_COUNT: usize = LAND_GROUPS * TILES_PER_GROUP;

/// File size that uniquely identifies the 7090 (High Seas) format.
const HS_FILE_SIZE: u64 = 3_188_736;

// Per-tile sizes on disk (for reference):
//   Legacy land:   flags(4) + texture_id(2) + name(20) = 26 bytes
//   HS land:       flags(8) + texture_id(2) + name(20) = 30 bytes
//   Legacy static: flags(4) + fields(13) + name(20)    = 37 bytes
//   HS static:     flags(8) + fields(13) + name(20)    = 41 bytes
// Each group: header(4) + 32 tiles

// ── TileDataFormat ─────────────────────────────────────────────────────────

/// Detected format version of `tiledata.mul`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileDataFormat {
    /// Pre-High Seas: 4-byte flags.
    Legacy,
    /// High Seas (7.0.9.0+): 8-byte flags.
    HighSeas,
}

// ── TileFlags ──────────────────────────────────────────────────────────────

/// Tile property flags.
///
/// Stored as `u64` regardless of format version — legacy `u32` values are
/// zero-extended at load time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileFlags(pub u64);

impl TileFlags {
    pub const BACKGROUND:  u64 = 0x0000_0001;
    pub const WEAPON:      u64 = 0x0000_0002;
    pub const TRANSPARENT: u64 = 0x0000_0004;
    pub const TRANSLUCENT: u64 = 0x0000_0008;
    pub const WALL:        u64 = 0x0000_0010;
    pub const DAMAGING:    u64 = 0x0000_0020;
    pub const IMPASSABLE:  u64 = 0x0000_0040;
    pub const WET:         u64 = 0x0000_0080;
    pub const SURFACE:     u64 = 0x0000_0200;
    pub const BRIDGE:      u64 = 0x0000_0400;
    pub const STACKABLE:   u64 = 0x0000_0800;
    pub const WINDOW:      u64 = 0x0000_1000;
    pub const NO_SHOOT:    u64 = 0x0000_2000;
    pub const PREFIX_A:    u64 = 0x0000_4000;
    pub const PREFIX_AN:   u64 = 0x0000_8000;
    pub const INTERNAL:    u64 = 0x0001_0000;
    pub const FOLIAGE:     u64 = 0x0002_0000;
    pub const PARTIAL_HUE: u64 = 0x0004_0000;
    pub const MAP:         u64 = 0x0010_0000;
    pub const CONTAINER:   u64 = 0x0020_0000;
    pub const WEARABLE:    u64 = 0x0040_0000;
    pub const LIGHT_SOURCE: u64 = 0x0080_0000;
    pub const ANIMATED:    u64 = 0x0100_0000;
    pub const HOVER_OVER:  u64 = 0x0200_0000;
    pub const NO_DIAGONAL: u64 = 0x0400_0000;
    pub const ARMOR:       u64 = 0x0800_0000;
    pub const ROOF:        u64 = 0x1000_0000;
    pub const DOOR:        u64 = 0x2000_0000;
    pub const STAIR_BACK:  u64 = 0x4000_0000;
    pub const STAIR_RIGHT: u64 = 0x8000_0000;

    /// Check whether a specific flag bit is set.
    pub fn has(self, flag: u64) -> bool {
        self.0 & flag != 0
    }

    /// Raw value.
    pub fn raw(self) -> u64 {
        self.0
    }
}

// ── LandTile ───────────────────────────────────────────────────────────────

/// A land tile entry from `tiledata.mul`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandTile {
    pub flags: TileFlags,
    pub texture_id: u16,
    pub name: FixedString<20>,
}

// ── StaticTileDef ──────────────────────────────────────────────────────────

/// A static object tile definition from `tiledata.mul`.
///
/// This describes the *metadata* for a static object type (flags, height,
/// name, etc.), as opposed to [`statics::StaticTile`](crate::statics::StaticTile)
/// which represents a *placed instance* in the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticTileDef {
    pub flags: TileFlags,
    pub weight: u8,
    pub quality: u8,
    pub anim_id: u16,
    pub height: u8,
    pub hue: u8,
    pub name: FixedString<20>,
}

impl StaticTileDef {
    /// Weight in 1/10ths of a stone, derived from tiledata's integer weight.
    ///
    /// `tiledata.mul` stores weight as whole stones (`u8`).  This method
    /// converts to tenths for use with the fractional weight system
    /// (e.g. `weight = 1` → `10` tenths = 1.0 stone).
    ///
    /// For sub-stone weights (reagents at 0.1, arrows at 0.3, etc.) the
    /// server must use `weight::item_weight_tenths` overrides instead —
    /// tiledata only provides the coarse integer value.
    pub fn weight_tenths(&self) -> u16 {
        self.weight as u16 * 10
    }
}

// ── TileData ───────────────────────────────────────────────────────────────

/// Complete parsed tile data from `tiledata.mul`.
#[derive(Debug)]
pub struct TileData {
    format: TileDataFormat,
    land: Vec<LandTile>,
    statics: Vec<StaticTileDef>,
}

impl TileData {
    /// Load from `tiledata.mul` in the given directory.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let path = dir.join("tiledata.mul");
        let file_len = fs::metadata(&path)?.len();

        debug!(
            "tiledata.mul: file_size={} bytes",
            file_len,
        );

        let file = File::open(path)?;
        let buf = BufReader::new(file);
        Ok(Self::from_stream(buf, file_len)?)
    }

    /// Parse from any readable + seekable stream.
    ///
    /// `file_len` is used for format detection and static group count.
    pub fn from_stream<S: Read + Seek>(stream: S, file_len: u64) -> Result<Self, DecodeError> {
        let format = if file_len == HS_FILE_SIZE {
            TileDataFormat::HighSeas
        } else {
            TileDataFormat::Legacy
        };

        let mut reader = StreamReader::<_, LE>::new(stream);
        Self::parse(&mut reader, format)
    }

    fn parse<R: ReadPrimitives<LE>>(
        reader: &mut R,
        format: TileDataFormat,
    ) -> Result<Self, DecodeError> {
        let is_hs = format == TileDataFormat::HighSeas;

        // ── Land tiles ─────────────────────────────────────────────────
        let mut land = Vec::with_capacity(LAND_TILE_COUNT);

        for i in 0..LAND_TILE_COUNT {
            // Group header logic:
            // - Legacy: every 32 tiles (i % 32 == 0)
            // - HS: skip tile 0, but tile 1 gets a header, then every 32 after that
            let need_header = if is_hs {
                i == 1 || (i > 0 && i % TILES_PER_GROUP == 0)
            } else {
                i % TILES_PER_GROUP == 0
            };

            if need_header {
                let _header = u32::decode(reader)?;
            }

            let flags = if is_hs {
                u64::decode(reader)?
            } else {
                u32::decode(reader)? as u64
            };
            let texture_id = u16::decode(reader)?;
            let name = FixedString::<20>::decode(reader)?;

            land.push(LandTile {
                flags: TileFlags(flags),
                texture_id,
                name,
            });
        }

        // ── Static tiles ───────────────────────────────────────────────
        //
        // Read groups of 32 static tiles until EOF.
        let mut statics = Vec::with_capacity(LAND_TILE_COUNT);

        let mut in_group_idx = 0usize;
        loop {
            // Group header at the start of each 32-tile group
            if in_group_idx % TILES_PER_GROUP == 0 {
                match u32::decode(reader) {
                    Ok(_header) => {}
                    Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => break,
                    Err(e) => return Err(e),
                }
            }

            let flags = if is_hs {
                match u64::decode(reader) {
                    Ok(v) => v,
                    Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => break,
                    Err(e) => return Err(e),
                }
            } else {
                match u32::decode(reader) {
                    Ok(v) => v as u64,
                    Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => break,
                    Err(e) => return Err(e),
                }
            };

            let weight = u8::decode(reader)?;
            let quality = u8::decode(reader)?;
            let _unk1 = u16::decode(reader)?;
            let _unk2 = u8::decode(reader)?;
            let _quantity = u8::decode(reader)?;
            let anim_id = u16::decode(reader)?;
            let _unk3 = u8::decode(reader)?;
            let hue = u8::decode(reader)?;
            let _unk4 = u16::decode(reader)?;
            let height = u8::decode(reader)?;
            let name = FixedString::<20>::decode(reader)?;

            statics.push(StaticTileDef {
                flags: TileFlags(flags),
                weight,
                quality,
                anim_id,
                height,
                hue,
                name,
            });

            in_group_idx += 1;
        }

        Ok(Self { format, land, statics })
    }

    /// Total number of tile entries (land + static).
    pub fn len(&self) -> usize {
        self.land.len() + self.statics.len()
    }

    /// Whether the tile data is empty.
    pub fn is_empty(&self) -> bool {
        self.land.is_empty() && self.statics.is_empty()
    }

    /// Detected format version.
    pub fn format(&self) -> TileDataFormat {
        self.format
    }

    /// All land tiles (always 16,384 entries if file is valid).
    pub fn land_tiles(&self) -> &[LandTile] {
        &self.land
    }

    /// All static object tiles.
    pub fn static_tiles(&self) -> &[StaticTileDef] {
        &self.statics
    }

    /// Get a land tile by its graphic ID.
    pub fn land(&self, id: u16) -> Option<&LandTile> {
        self.land.get(id as usize)
    }

    /// Get a static tile by its graphic ID.
    pub fn static_tile(&self, id: u16) -> Option<&StaticTileDef> {
        self.statics.get(id as usize)
    }
}
