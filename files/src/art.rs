//! Art sprite reader (`artidx.mul` + `art.mul`).
//!
//! Art sprites come in two flavours, distinguished by index ID:
//!
//! | Range             | Type   | Format                              |
//! |-------------------|--------|-------------------------------------|
//! | 0 .. 0x3FFF       | Land   | Fixed 44x44 diamond, raw pixels     |
//! | 0x4000 ..         | Static | Variable size, RLE-compressed       |
//!
//! Pixels are stored as 16-bit RGB555 values (5 bits per channel).
//! A zero pixel value means transparent.
//!
//! # Usage
//!
//! ```ignore
//! let mut reader = ArtReader::read(Path::new("data"))?;
//! let sprite = reader.read_sprite(0x4000)?;
//! println!("{}x{}", sprite.width, sprite.height);
//! ```

use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, StreamReader, LE};

use crate::mul::MulIndex;

// Re-export color types so existing `art::Rgb` / `art::rgb555_to_rgb` imports keep working.
#[doc(inline)]
pub use crate::color::{rgb555_to_rgb, Rgb};

// ── Constants ──────────────────────────────────────────────────────────────

/// Land tile IDs are in the range `0 .. LAND_TILE_LIMIT`.
pub const LAND_TILE_LIMIT: u32 = 0x4000;

/// Fixed dimensions of a land tile sprite.
pub const LAND_TILE_SIZE: u16 = 44;

// ── ArtSprite ──────────────────────────────────────────────────────────────

/// A decoded art sprite.
///
/// Pixels are stored row-major.  `None` means transparent.
#[derive(Debug, Clone)]
pub struct ArtSprite {
    pub width: u16,
    pub height: u16,
    pixels: Vec<Option<Rgb>>,
}

impl ArtSprite {
    /// Get a pixel at `(x, y)`.  Returns `None` for transparent or out-of-bounds.
    pub fn pixel(&self, x: u16, y: u16) -> Option<Rgb> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    /// Total number of non-transparent pixels.
    pub fn opaque_count(&self) -> usize {
        self.pixels.iter().filter(|p| p.is_some()).count()
    }

    /// Whether this is a land tile (44x44 diamond).
    pub fn is_land(&self) -> bool {
        self.width == LAND_TILE_SIZE && self.height == LAND_TILE_SIZE
    }
}

// ── ArtReader ──────────────────────────────────────────────────────────────

/// Lazy reader for art sprites.
///
/// Opens `artidx.mul` (parsed fully into memory) and keeps `art.mul`
/// open for on-demand seeking.  Individual sprites are decoded via
/// [`read_sprite`](ArtReader::read_sprite).
pub struct ArtReader<R: Read + Seek> {
    index: MulIndex,
    reader: StreamReader<R, LE>,
}

impl ArtReader<BufReader<File>> {
    /// Load `artidx.mul` + `art.mul` from a directory.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let index = MulIndex::read(&dir.join("artidx.mul"))?;

        debug!(
            "art: index_entries={}",
            index.len(),
        );

        let file = File::open(dir.join("art.mul"))?;
        let buf = BufReader::new(file);
        Ok(Self::from_stream(index, buf))
    }
}

impl<R: Read + Seek> ArtReader<R> {
    /// Create from a pre-loaded index and a readable+seekable stream.
    pub fn from_stream(index: MulIndex, stream: R) -> Self {
        let reader = StreamReader::<_, LE>::new(stream);
        Self { index, reader }
    }

    /// The parsed index.
    pub fn index(&self) -> &MulIndex {
        &self.index
    }

    /// Number of index entries (land + static art IDs).
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Read and decode a sprite by its art ID.
    ///
    /// - IDs `0 .. 0x3FFF` are land tiles (44x44 diamond).
    /// - IDs `0x4000 ..` are static item sprites (variable size, RLE).
    pub fn read_sprite(&mut self, id: u32) -> Result<ArtSprite, DecodeError> {
        let entry = self.index.get(id as usize)
            .filter(|e| e.is_valid())
            .ok_or(DecodeError::Other(format!("art ID {id} (0x{id:04X}): no data")))?;

        self.reader.seek_to(entry.offset as u64)?;

        if id < LAND_TILE_LIMIT {
            self.decode_land()
        } else {
            self.decode_static()
        }
    }

    /// Decode a land tile (44x44 diamond, raw RGB555 pixels).
    fn decode_land(&mut self) -> Result<ArtSprite, DecodeError> {
        const SIZE: u16 = LAND_TILE_SIZE;
        const HALF: u16 = SIZE / 2;

        let mut pixels = vec![None; SIZE as usize * SIZE as usize];

        let mut left = HALF - 1;
        let mut right = HALF;

        for y in 0..SIZE {
            for x in left..=right {
                let color = u16::decode(&mut self.reader)?;
                pixels[y as usize * SIZE as usize + x as usize] = Some(rgb555_to_rgb(color));
            }

            if y < HALF - 1 {
                left -= 1;
                right += 1;
            } else if y > HALF - 1 {
                left += 1;
                right -= 1;
            }
        }

        Ok(ArtSprite {
            width: SIZE,
            height: SIZE,
            pixels,
        })
    }

    /// Decode a static item sprite (variable size, RLE-compressed).
    fn decode_static(&mut self) -> Result<ArtSprite, DecodeError> {
        let _flag = u32::decode(&mut self.reader)?;
        let width = u16::decode(&mut self.reader)?;
        let height = u16::decode(&mut self.reader)?;

        if width == 0 || width >= 1024 || height == 0 || height >= 1024 {
            return Err(DecodeError::Other(format!(
                "invalid static sprite dimensions: {width}x{height}"
            )));
        }

        // Per-row offsets (in u16 units, relative to the start of pixel data).
        let mut row_offsets = Vec::with_capacity(height as usize);
        for _ in 0..height {
            row_offsets.push(u16::decode(&mut self.reader)?);
        }

        // Record the start of pixel data — row offsets are relative to here.
        // We need an absolute stream position, which requires Seek.
        // StreamReader wraps a Read+Seek, but doesn't expose position().
        // Instead, track the data-start offset: it's right after the header.
        //
        // Header: flag(4) + width(2) + height(2) + row_offsets(height*2)
        // But we already consumed all of that. The current position IS data_start.
        // We'll use seek_to() with computed absolute offsets.
        //
        // To get the current position, we need the entry offset + header size.
        // Simpler: compute data_start from what we know.

        // Actually, we can just ask the inner stream for its position.
        let data_start = self.reader.inner_mut().stream_position()
            .map_err(|e| DecodeError::Io(e.to_string()))?;

        let mut pixels = vec![None; width as usize * height as usize];

        for y in 0..height {
            let row_offset = row_offsets[y as usize] as u64 * 2;
            self.reader.seek_to(data_start + row_offset)?;

            let mut x: u16 = 0;
            loop {
                let x_offset = u16::decode(&mut self.reader)?;
                let run = u16::decode(&mut self.reader)?;

                if x_offset == 0 && run == 0 {
                    break; // end of row
                }

                if x_offset + run >= 2048 {
                    return Err(DecodeError::Other(format!(
                        "art RLE: suspicious offset+run={}+{}",
                        x_offset, run
                    )));
                }

                x += x_offset;
                for _ in 0..run {
                    let color = u16::decode(&mut self.reader)?;
                    if (x as usize) < width as usize {
                        let idx = y as usize * width as usize + x as usize;
                        if color != 0 {
                            pixels[idx] = Some(rgb555_to_rgb(color));
                        }
                    }
                    x += 1;
                }
            }
        }

        Ok(ArtSprite {
            width,
            height,
            pixels,
        })
    }
}
