//! Radar color table reader (`Radarcol.mul`).
//!
//! `Radarcol.mul` is a flat file of 16-bit RGB555 color values.
//! The first 16 384 entries correspond to land tile IDs,
//! the remaining entries correspond to static tile IDs.
//!
//! These colors are used by the in-game radar/minimap to represent
//! each tile type as a single pixel.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, StreamReader, LE};

use crate::color::{rgb555_to_rgb, Rgb, Rgba};

/// Number of land tile color entries (tile IDs `0..0x3FFF`).
pub const LAND_ENTRIES: usize = 0x4000; // 16384

/// Radar color lookup table.
///
/// Provides the minimap color for any land or static tile ID.
#[derive(Debug)]
pub struct RadarColors {
    colors: Vec<Rgb>,
}

impl RadarColors {
    /// Load from `Radarcol.mul` in the given directory.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let path = dir.join("Radarcol.mul");
        let file_len = fs::metadata(&path)?.len() as usize;
        let entry_count = file_len / 2;

        let file = File::open(&path)?;
        let buf = BufReader::new(file);
        let result = Self::from_stream(buf, entry_count)?;

        debug!("Radarcol.mul: {entry_count} entries ({file_len} bytes)");

        Ok(result)
    }

    /// Parse from a [`Read`] stream containing `entry_count` RGB555 values.
    pub fn from_stream<R: Read>(stream: R, entry_count: usize) -> Result<Self, DecodeError> {
        let mut reader = StreamReader::<_, LE>::new(stream);
        let mut colors = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let raw = u16::decode(&mut reader)?;
            colors.push(rgb555_to_rgb(raw));
        }
        Ok(Self { colors })
    }

    /// Build from a pre-decoded color slice (for testing).
    pub fn from_colors(colors: Vec<Rgb>) -> Self {
        Self { colors }
    }

    /// Total number of color entries.
    pub fn len(&self) -> usize {
        self.colors.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }

    /// Number of land tile entries (always min of table size and 16384).
    pub fn land_count(&self) -> usize {
        self.colors.len().min(LAND_ENTRIES)
    }

    /// Number of static tile entries.
    pub fn static_count(&self) -> usize {
        self.colors.len().saturating_sub(LAND_ENTRIES)
    }

    /// Iterate over all color entries.
    pub fn iter(&self) -> impl Iterator<Item = &Rgb> {
        self.colors.iter()
    }

    /// Get the radar color for a land tile ID as [`Rgba`] (alpha = 255).
    ///
    /// Returns `None` if `tile_id` is out of range.
    pub fn land_color(&self, tile_id: u16) -> Option<Rgba> {
        let idx = tile_id as usize;
        if idx >= LAND_ENTRIES {
            return None;
        }
        self.colors.get(idx).map(|c| c.opaque())
    }

    /// Get the radar color for a static tile ID as [`Rgba`] (alpha = 255).
    ///
    /// Returns `None` if `tile_id` is out of range.
    pub fn static_color(&self, tile_id: u16) -> Option<Rgba> {
        let idx = LAND_ENTRIES + tile_id as usize;
        self.colors.get(idx).map(|c| c.opaque())
    }
}
