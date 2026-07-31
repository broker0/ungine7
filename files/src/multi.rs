//! Multi-object collection reader (`multi.mul` + `multi.idx`).
//!
//! Multi objects (houses, boats, etc.) are stored as arrays of parts — each
//! part is a static tile placed at a relative (x, y, z) offset.
//!
//! # Format versions
//!
//! Two on-disk layouts exist, distinguished by entry size:
//!
//! | Version   | Entry size | Fields                                       |
//! |-----------|------------|----------------------------------------------|
//! | Legacy    | 12 bytes   | tile_id(u16) x(i16) y(i16) z(i16) flags(u32) |
//! | HS (7090) | 16 bytes   | same + clilocs(u32)                          |
//!
//! The reader auto-detects the format by checking which entry size divides
//! evenly into every valid index entry's length.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, ReadPrimitives, StreamReader, LE};
use macros::{Decode, Encode};

use crate::mul::{MulIndex, INVALID_OFFSET};

// ── Entry sizes ────────────────────────────────────────────────────────────

/// Legacy multi part: tile_id(2) + x(2) + y(2) + z(2) + flags(4) = 12 bytes.
const LEGACY_PART_SIZE: usize = 12;

/// High Seas multi part: legacy(12) + clilocs(4) = 16 bytes.
const HS_PART_SIZE: usize = 16;

// ── MultiFormat ────────────────────────────────────────────────────────────

/// Detected format version of `multi.mul`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiFormat {
    /// Pre-High Seas: 12-byte entries, no clilocs field.
    Legacy,
    /// High Seas (7.0.9.0+): 16-byte entries with clilocs field.
    HighSeas,
}

impl MultiFormat {
    /// Size of a single part entry on disk.
    pub fn part_size(self) -> usize {
        match self {
            Self::Legacy => LEGACY_PART_SIZE,
            Self::HighSeas => HS_PART_SIZE,
        }
    }
}

// ── MultiPart ──────────────────────────────────────────────────────────────

/// A single component of a multi object.
///
/// Fields: `tile_id` (static tile graphic ID), `x`/`y`/`z` (offset from
/// multi origin), `flags` (visibility, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode, Encode)]
#[binary(endian = "le")]
pub struct MultiPart {
    pub tile_id: u16,
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub flags: u32,
}

// ── MultiCollection ────────────────────────────────────────────────────────

/// Loaded multi-object collection.
///
/// Stores all parts in a single flat `Vec` and keeps per-multi offsets
/// into that array for O(1) lookup by multi ID.
#[derive(Debug)]
pub struct MultiCollection {
    format: MultiFormat,
    parts: Vec<MultiPart>,
    /// For each multi ID: `Some((start, count))` into `parts`, or `None`.
    entries: Vec<Option<(usize, usize)>>,
}

impl MultiCollection {
    /// Load from `multi.idx` + `multi.mul` in the given directory.
    ///
    /// Reads the index file fully, then streams the data file entry by entry
    /// using seek — no intermediate full-file buffer.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let index = MulIndex::read(&dir.join("multi.idx"))?;

        debug!(
            "multi.mul: index_entries={}",
            index.len(),
        );

        let file = File::open(dir.join("multi.mul"))?;
        let buf = BufReader::new(file);
        Ok(Self::from_stream(&index, buf)?)
    }

    /// Build from a pre-loaded index and a readable+seekable stream.
    ///
    /// This is the generic entry point — works with any `Read + Seek` source.
    pub fn from_stream<S: io::Read + io::Seek>(
        index: &MulIndex,
        stream: S,
    ) -> Result<Self, DecodeError> {
        let format = detect_format(index)?;
        let part_size = format.part_size();

        let mut reader = StreamReader::<_, LE>::new(stream);

        let mut parts = Vec::new();
        let mut entries = Vec::with_capacity(index.len());

        for entry in index.iter() {
            if !entry.is_valid() {
                entries.push(None);
                continue;
            }

            let count = entry.length as usize / part_size;
            reader.seek_to(entry.offset as u64)?;

            let start = parts.len();
            for _ in 0..count {
                let part = MultiPart::decode(&mut reader)?;
                if format == MultiFormat::HighSeas {
                    reader.skip(4)?;
                }
                parts.push(part);
            }

            entries.push(Some((start, count)));
        }

        Ok(Self { format, parts, entries })
    }

    /// Detected file format.
    pub fn format(&self) -> MultiFormat {
        self.format
    }

    /// Number of multi IDs (including empty/unused slots).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the collection has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get the parts for a multi by its ID.
    ///
    /// Returns an empty slice for unused IDs or out-of-range IDs.
    pub fn parts(&self, multi_id: u16) -> &[MultiPart] {
        match self.entries.get(multi_id as usize) {
            Some(Some((start, count))) => &self.parts[*start..*start + *count],
            _ => &[],
        }
    }

    /// Total number of parts across all multis.
    pub fn total_parts(&self) -> usize {
        self.parts.len()
    }

    /// Number of multi IDs that actually contain data (non-empty slots).
    pub fn valid_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }

    /// Iterate over all `(multi_id, &[MultiPart])` pairs that have data.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &[MultiPart])> {
        self.entries.iter().enumerate().filter_map(|(id, slot)| {
            slot.map(|(start, count)| (id as u16, &self.parts[start..start + count]))
        })
    }
}

// ── Format detection ───────────────────────────────────────────────────────

/// Detect whether the multi file uses legacy (12-byte) or HS (16-byte) entries
/// by checking divisibility of every valid index entry's length.
fn detect_format(index: &MulIndex) -> Result<MultiFormat, DecodeError> {
    let mut fits_legacy = true;
    let mut fits_hs = true;
    let mut has_valid = false;

    for entry in index.iter() {
        if entry.offset == INVALID_OFFSET {
            continue;
        }
        has_valid = true;
        let len = entry.length as usize;
        fits_legacy &= len % LEGACY_PART_SIZE == 0;
        fits_hs &= len % HS_PART_SIZE == 0;
    }

    if !has_valid {
        // No valid entries — default to legacy
        return Ok(MultiFormat::Legacy);
    }

    match (fits_legacy, fits_hs) {
        (true, false) => Ok(MultiFormat::Legacy),
        (false, true) => Ok(MultiFormat::HighSeas),
        // Both 12 and 16 divide evenly — ambiguous. Since 48 is the LCM,
        // this can only happen if every valid entry length is a multiple of 48.
        // Prefer HS as the more modern format.
        (true, true) => Ok(MultiFormat::HighSeas),
        (false, false) => Err(DecodeError::Other(
            "multi.mul: entry sizes are inconsistent — \
             not divisible by 12 (legacy) or 16 (HS)"
                .into(),
        )),
    }
}
