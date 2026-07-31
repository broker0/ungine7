//! Hue table reader (`hues.mul`).
//!
//! `hues.mul` defines color transformation palettes (hues) used to
//! recolor items and characters in the game.  Each hue entry contains
//! a 32-color gradient stored as RGB555 values, a name, and a
//! start/end range indicating which source color indices the gradient
//! maps to.
//!
//! The file is organized as groups of 8 entries, each group preceded
//! by a 4-byte header (unused).
//!
//! # Wire layout
//!
//! ```text
//! HueGroup (708 bytes):
//!   header:    u32           (unused)
//!   entries:   [HueEntry; 8]
//!
//! HueEntry (88 bytes):
//!   colors:      [u16; 32]   (RGB555 gradient, 64 bytes)
//!   table_start: u16
//!   table_end:   u16
//!   name:        [u8; 20]    (null-padded ASCII)
//! ```

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, FixedString, ReadPrimitives, StreamReader, LE};

use crate::color::{rgb555_to_rgb, Rgb};

/// Number of colors in each hue gradient.
pub const PALETTE_SIZE: usize = 32;

/// Number of entries per group in `hues.mul`.
pub const ENTRIES_PER_GROUP: usize = 8;

/// Byte size of one group: header(4) + 8 * entry(88) = 708.
pub const GROUP_DISK_SIZE: usize = 4 + ENTRIES_PER_GROUP * ENTRY_DISK_SIZE;

/// Byte size of one entry: colors(64) + table_start(2) + table_end(2) + name(20) = 88.
pub const ENTRY_DISK_SIZE: usize = PALETTE_SIZE * 2 + 2 + 2 + 20;

/// A single hue entry — a named 32-color gradient palette.
#[derive(Debug, Clone)]
pub struct HueEntry {
    /// 32-color gradient (already converted to 8-bit RGB).
    pub colors: [Rgb; PALETTE_SIZE],
    /// Start index of the source color range this gradient maps to.
    pub table_start: u16,
    /// End index of the source color range this gradient maps to.
    pub table_end: u16,
    /// Human-readable name of the hue.
    pub name: String,
}

/// Complete hue table loaded from `hues.mul`.
#[derive(Debug, Clone)]
pub struct HueTable {
    entries: Vec<HueEntry>,
}

impl HueTable {
    /// Load from `hues.mul` in the given directory.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let path = dir.join("hues.mul");
        let file_len = fs::metadata(&path)?.len() as usize;

        if file_len % GROUP_DISK_SIZE != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "hues.mul size ({file_len}) is not a multiple of group size ({GROUP_DISK_SIZE})"
                ),
            ));
        }

        let group_count = file_len / GROUP_DISK_SIZE;

        let file = File::open(&path)?;
        let buf = BufReader::new(file);
        let result = Self::from_stream(buf, group_count)?;

        debug!(
            "hues.mul: {group_count} groups, {} entries ({file_len} bytes)",
            result.len()
        );

        Ok(result)
    }

    /// Parse from a [`Read`] stream containing `group_count` hue groups.
    pub fn from_stream<R: Read>(stream: R, group_count: usize) -> Result<Self, DecodeError> {
        let mut reader = StreamReader::<_, LE>::new(stream);
        let entries = parse(&mut reader, group_count)?;
        Ok(Self { entries })
    }

    /// Build from a pre-decoded entry list (for testing).
    pub fn from_entries(entries: Vec<HueEntry>) -> Self {
        Self { entries }
    }

    /// Total number of hue entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get a hue entry by index.
    pub fn get(&self, index: usize) -> Option<&HueEntry> {
        self.entries.get(index)
    }

    /// Iterate over all hue entries.
    pub fn iter(&self) -> impl Iterator<Item = &HueEntry> {
        self.entries.iter()
    }

    /// Number of entries that have a non-empty name.
    pub fn named_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.name.is_empty()).count()
    }
}

fn parse<R: ReadPrimitives<LE>>(
    reader: &mut R,
    group_count: usize,
) -> Result<Vec<HueEntry>, DecodeError> {
    let total = group_count * ENTRIES_PER_GROUP;
    let mut entries = Vec::with_capacity(total);

    for _ in 0..group_count {
        // Skip group header (4 bytes, unused).
        let _header = u32::decode(reader)?;

        for _ in 0..ENTRIES_PER_GROUP {
            let mut colors = [Rgb { r: 0, g: 0, b: 0 }; PALETTE_SIZE];
            for c in &mut colors {
                *c = rgb555_to_rgb(u16::decode(reader)?);
            }

            let table_start = u16::decode(reader)?;
            let table_end = u16::decode(reader)?;
            let name = FixedString::<20>::decode(reader)?;

            entries.push(HueEntry {
                colors,
                table_start,
                table_end,
                name: name.into_inner(),
            });
        }
    }

    Ok(entries)
}
