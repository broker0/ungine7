//! Custom house packets (0xD8, S→C).
//!
//! # 0xD8 SendCustomHouse
//!
//! Sent by the server when a player designs or views a custom house.  The
//! payload consists of one or more **planes**, each carrying a zlib-compressed
//! block of tile records whose format is determined by the plane's `mode`
//! field.
//!
//! ## Wire layout
//!
//! ```text
//! BYTE[1]  cmd              = 0xD8
//! BYTE[2]  length           (total packet length including cmd+length)
//! BYTE[1]  compression_type (0 = uncompressed outer, 3 = zlib outer — see below)
//! BYTE[1]  unknown          (always 0)
//! BYTE[4]  house_serial
//! BYTE[4]  revision
//! BYTE[2]  num_tiles        (informational, may be 0)
//! BYTE[2]  buffer_length    (informational)
//! --- payload (plane_count byte + planes) ---
//! BYTE[1]  plane_count
//! For each plane:
//!   BYTE[4]  header   (bitpacked big-endian u32)
//!     bits 31-28 : mode        (0/1/2)
//!     bits 27-24 : plane_z     (encoded Z, used in mode 1 & 2)
//!     bits 23-16 : unc_len[7:0]
//!     bits 15- 8 : cmp_len[7:0]
//!     bits  7- 4 : unc_len[11:8] (high nibble)
//!     bits  3- 0 : cmp_len[11:8] (high nibble)
//!   BYTE[cmp_len]  data  (zlib-compressed tile records)
//! ```
//!
//! ## Tile record formats (after per-plane zlib decompression)
//!
//! | mode | record size | fields                     |
//! |------|-------------|----------------------------|
//! | 0    | 5 bytes     | u16 id, i8 x, i8 y, i8 z  |
//! | 1    | 4 bytes     | u16 id, i8 x, i8 y         |
//! | 2    | 2 bytes     | u16 id                     |
//!
//! Decompression and tile interpretation are left to the application layer.
//!
//! ## `compression_type`
//!
//! Some documentation describes `compression_type == 3` as indicating that
//! the entire payload is outer-zlib-compressed.  In practice **no known
//! client implementation** (ClassicUO, OrionUO, etc.) actually applies
//! outer decompression — the field is read and ignored.  Only **per-plane**
//! zlib compression (inside each plane's `data` blob) is used.  This crate
//! therefore treats `compression_type` as an opaque informational byte that
//! is stored and round-tripped but does not affect parsing.

use u_io::{BE, BinaryReader, BinaryWriter, Decode, DecodeError, Encode, ReadPrimitives};

use crate::compress::zlib_decompress;
use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── HousePlane ─────────────────────────────────────────────────────────────

/// One plane of a custom house, as decoded from a [`SendCustomHouse`] packet.
///
/// The `data` field contains the **raw compressed bytes** for this plane
/// (still zlib-compressed per-plane).  Decompress with zlib/deflate and
/// interpret according to `mode` to obtain the individual tile records.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HousePlane {
    /// Tile record format:
    ///
    /// | value | record | fields                     |
    /// |-------|--------|----------------------------|
    /// | 0     | 5 B    | u16 id, i8 x, i8 y, i8 z  |
    /// | 1     | 4 B    | u16 id, i8 x, i8 y         |
    /// | 2     | 2 B    | u16 id                     |
    pub mode: u8,

    /// Encoded Z coordinate used in mode 1 and mode 2.
    ///
    /// Actual Z translation:
    /// - `plane_z == 0` → Z = 0
    /// - `plane_z > 0`  → Z = ((plane_z − 1) % 4) × 20 + 7  (i.e. 7, 27, 47, 67…)
    pub plane_z: u8,

    /// Uncompressed byte length of `data` (12-bit value, max 4095).
    pub uncompressed_len: u16,

    /// Raw zlib-compressed tile data for this plane.
    pub data: Vec<u8>,
}

impl HousePlane {
    /// Decode the bitpacked 4-byte plane header.
    ///
    /// Returns `(mode, plane_z, unc_len, cmp_len)`.
    ///
    /// Bit layout (big-endian u32):
    /// ```text
    /// 31-28  mode
    /// 27-24  plane_z
    /// 23-16  unc_len[7:0]
    /// 15- 8  cmp_len[7:0]
    ///  7- 4  unc_len[11:8]
    ///  3- 0  cmp_len[11:8]
    /// ```
    pub fn decode_header(h: u32) -> (u8, u8, u16, u16) {
        let mode    = ((h >> 28) & 0xF) as u8;
        let plane_z = ((h >> 24) & 0xF) as u8;
        let unc_len = (((h >> 16) & 0xFF) | ((h & 0xF0) << 4)) as u16;
        let cmp_len = (((h >>  8) & 0xFF) | ((h & 0x0F) << 8)) as u16;
        (mode, plane_z, unc_len, cmp_len)
    }

    /// Encode `(mode, plane_z, unc_len, cmp_len)` back into a 4-byte header.
    pub fn encode_header(mode: u8, plane_z: u8, unc_len: u16, cmp_len: u16) -> u32 {
        ((mode    as u32) << 28)
        | ((plane_z as u32) << 24)
        | (((unc_len & 0x0FF) as u32) << 16)
        | (((cmp_len & 0x0FF) as u32) <<  8)
        | (((unc_len >>    4) & 0xF0) as u32)
        | (((cmp_len >>    8) & 0x0F) as u32)
    }

    /// Compute the actual Z value from the encoded `plane_z` field.
    ///
    /// - `plane_z == 0` → 0
    /// - `plane_z > 0`  → ((plane_z − 1) % 4) × 20 + 7
    pub fn actual_z(plane_z: u8) -> i8 {
        if plane_z == 0 {
            0
        } else {
            (((plane_z - 1) % 4) * 20 + 7) as i8
        }
    }
}

// ── HouseTile ─────────────────────────────────────────────────────────────

/// A single tile from a custom house, after decompression and interpretation
/// of a [`HousePlane`]'s data.
///
/// Coordinates (`x`, `y`) are relative to the multi's origin.
/// `z` is an absolute offset computed from the plane's mode and `plane_z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HouseTile {
    /// Static tile graphic ID.
    pub tile_id: u16,
    /// X offset relative to multi origin.
    pub x: i16,
    /// Y offset relative to multi origin.
    pub y: i16,
    /// Z offset (absolute plane Z, or per-tile Z for mode 0).
    pub z: i16,
}

impl HousePlane {
    /// Decompress this plane's zlib data and decode the tile records.
    ///
    /// For **mode 2** (ID-only records), callers must supply the multi's
    /// foundation bounding box as **relative** offsets from the multi
    /// origin (`x_min`, `y_min`, `x_max`, `y_max` — the same values
    /// stored in `MultiExtent`).
    ///
    /// In the Delphi reference the foundation bbox is stored in
    /// world-absolute coordinates (`MultiPosX + XMin`, etc.), and the
    /// tile X/Y are then converted back to relative by subtracting
    /// `_XLoc`/`_YLoc`.  The world position cancels out, leaving the
    /// same result as using relative extent directly.
    ///
    /// For modes 0 and 1, the foundation arguments are ignored.
    ///
    /// Tile records inside the decompressed data are **big-endian** (the
    /// `u16` graphic ID is network-order, matching the rest of packet
    /// 0xD8).
    pub fn decode_tiles(
        &self,
        foundation_x_min: i16,
        foundation_y_min: i16,
        _foundation_x_max: i16,
        foundation_y_max: i16,
    ) -> Result<Vec<HouseTile>, DecodeError> {
        let decompressed = zlib_decompress(&self.data)?;
        let mut r = BinaryReader::<BE>::new(&decompressed);
        let mut tiles = Vec::new();

        match self.mode {
            0 => {
                // Mode 0: 5 bytes per record — u16 id (BE), i8 x, i8 y, i8 z
                while r.remaining_len() >= 5 {
                    let id: u16 = r.read_u16()?;
                    let x: i8 = r.read_i8()?;
                    let y: i8 = r.read_i8()?;
                    let z: i8 = r.read_i8()?;
                    if id != 0 {
                        tiles.push(HouseTile {
                            tile_id: id,
                            x: x as i16,
                            y: y as i16,
                            z: z as i16,
                        });
                    }
                }
            }
            1 => {
                // Mode 1: 4 bytes per record — u16 id (BE), i8 x, i8 y
                // Z is derived from the plane's `plane_z` field.
                let z = Self::actual_z(self.plane_z) as i16;
                while r.remaining_len() >= 4 {
                    let id: u16 = r.read_u16()?;
                    let x: i8 = r.read_i8()?;
                    let y: i8 = r.read_i8()?;
                    if id != 0 {
                        tiles.push(HouseTile {
                            tile_id: id,
                            x: x as i16,
                            y: y as i16,
                            z,
                        });
                    }
                }
            }
            2 => {
                // Mode 2: 2 bytes per record — u16 id (BE) only.
                // Z from plane_z; X and Y are computed sequentially to
                // fill the plane area, matching the Delphi reference.
                //
                // The Delphi reference uses world-absolute XMin/YMin
                // and subtracts _XLoc/_YLoc to produce relative coords.
                // Since those cancel, we use the relative foundation
                // extent directly.
                let z = Self::actual_z(self.plane_z) as i16;

                let (x_offs, y_offs, multi_height) = if self.plane_z == 0 {
                    (
                        foundation_x_min as i32,
                        foundation_y_min as i32,
                        (foundation_y_max as i32 - foundation_y_min as i32) + 2,
                    )
                } else if self.plane_z <= 4 {
                    (
                        foundation_x_min as i32 + 1,
                        foundation_y_min as i32 + 1,
                        (foundation_y_max as i32 - foundation_y_min as i32),
                    )
                } else {
                    (
                        foundation_x_min as i32,
                        foundation_y_min as i32,
                        (foundation_y_max as i32 - foundation_y_min as i32) + 1,
                    )
                };

                let mut j: i32 = 0;
                while r.remaining_len() >= 2 {
                    let id: u16 = r.read_u16()?;
                    let (x, y) = if multi_height > 0 {
                        (j / multi_height, j % multi_height)
                    } else {
                        (0, 0)
                    };
                    if id != 0 {
                        tiles.push(HouseTile {
                            tile_id: id,
                            x: (x_offs + x) as i16,
                            y: (y_offs + y) as i16,
                            z,
                        });
                    }
                    j += 1;
                }
            }
            _ => {
                // Unknown mode — skip silently (forward-compatible).
            }
        }

        Ok(tiles)
    }
}

impl SendCustomHouse {
    /// Decompress all planes and return the complete list of house tiles.
    ///
    /// The `foundation_*` parameters are the multi's relative extent
    /// (from `MultiExtent`), needed for mode-2 planes; see
    /// [`HousePlane::decode_tiles`] for details.
    pub fn decode_all_tiles(
        &self,
        foundation_x_min: i16,
        foundation_y_min: i16,
        foundation_x_max: i16,
        foundation_y_max: i16,
    ) -> Result<Vec<HouseTile>, DecodeError> {
        let mut all = Vec::new();
        for plane in &self.planes {
            let tiles = plane.decode_tiles(
                foundation_x_min, foundation_y_min,
                foundation_x_max, foundation_y_max,
            )?;
            all.extend_from_slice(&tiles);
        }
        Ok(all)
    }
}

// ── SendCustomHouse ────────────────────────────────────────────────────────

/// Packet 0xD8 — Send Custom House (dynamic, S→C)
///
/// See the [module documentation](self) for the full wire layout and tile
/// format details.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SendCustomHouse {
    /// Informational compression byte.  Known values: `0` and `3`.
    /// Despite some documentation claiming `3` means outer zlib, no known
    /// client actually uses this field — it is stored for wire fidelity only.
    pub compression_type: u8,

    /// When `true` the server expects the client to send a response packet
    /// after processing the house data.
    pub enable_response: bool,

    /// Serial number of the house multi object.
    pub house_serial: u32,

    /// Revision / design state counter.
    pub revision: u32,

    /// Informational tile count in the packet header (may be 0).
    pub num_tiles: u16,

    /// Informational buffer length in the packet header (may be 0).
    pub buffer_length: u16,

    /// Decoded house planes.
    pub planes: Vec<HousePlane>,
}

impl ManualPacket for SendCustomHouse {
    const ID: u8 = 0xD8;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: cmd(1)+len(2)+comp(1)+unk(1)+serial(4)+rev(4)+ntiles(2)+buflen(2) = 17
        if data.len() < 17 {
            return Err(PacketError::Decode(DecodeError::Truncated));
        }
        if data[0] != 0xD8 {
            return Err(PacketError::BadId { expected: 0xD8, actual: data[0] });
        }

        let mut r = BinaryReader::<BE>::new(data);
        let _cmd:             u8  = Decode::decode(&mut r)?;
        let _len:             u16 = Decode::decode(&mut r)?;
        let compression_type: u8  = Decode::decode(&mut r)?;
        let enable_response:  bool = Decode::<BE>::decode(&mut r).map(|v: u8| v != 0)?;
        let house_serial:     u32 = Decode::decode(&mut r)?;
        let revision:         u32  = Decode::decode(&mut r)?;
        let num_tiles:        u16  = Decode::decode(&mut r)?;
        let buffer_length:    u16  = Decode::decode(&mut r)?;

        let planes = decode_planes(&mut r)?;

        Ok(Self {
            compression_type,
            enable_response,
            house_serial,
            revision,
            num_tiles,
            buffer_length,
            planes,
        })
    }
}

impl Encode<BE> for SendCustomHouse {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0u16); // length placeholder — back-patched by to_bytes()
        w.put_u8(self.compression_type);
        w.put_u8(self.enable_response as u8);
        w.put_u32(self.house_serial);
        w.put_u32(self.revision);
        w.put_u16(self.num_tiles);
        w.put_u16(self.buffer_length);

        // Plane payload.
        w.put_u8(self.planes.len() as u8);
        for plane in &self.planes {
            let h = HousePlane::encode_header(
                plane.mode,
                plane.plane_z,
                plane.uncompressed_len,
                plane.data.len() as u16,
            );
            w.put_u32(h);
            w.put_slice(&plane.data);
        }
    }
}

// ── decode_planes helper ───────────────────────────────────────────────────

fn decode_planes(r: &mut BinaryReader<'_, BE>) -> Result<Vec<HousePlane>, PacketError> {
    if r.remaining_len() == 0 {
        return Ok(Vec::new());
    }

    let plane_count: u8 = Decode::decode(r)?;
    let mut planes = Vec::with_capacity(plane_count as usize);

    for _ in 0..plane_count {
        let header: u32 = Decode::decode(r)?;
        let (mode, plane_z, unc_len, cmp_len) = HousePlane::decode_header(header);

        // Mirror C# behaviour: skip planes with no compressed data.
        if cmp_len == 0 {
            continue;
        }

        if r.remaining_len() < cmp_len as usize {
            return Err(PacketError::Decode(DecodeError::Truncated));
        }
        let data = r.read_slice(cmp_len as usize)?.to_vec();

        planes.push(HousePlane {
            mode,
            plane_z,
            uncompressed_len: unc_len,
            data,
        });
    }

    Ok(planes)
}
