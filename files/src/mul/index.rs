//! MUL index file (`.idx`) parser.
//!
//! Each `.idx` file is an array of 12-byte records (offset, length, extra)
//! that describe where each entry lives in the corresponding `.mul` file.
//!
//! An offset of `0xFFFF_FFFF` marks an unused/empty slot.

use std::fs;
use std::io;
use std::path::Path;

use u_io::{BinaryReader, Decode, ReadPrimitives, LE};

/// Size of a single index record on disk (3 x u32 = 12 bytes).
pub const INDEX_RECORD_SIZE: usize = 12;

/// Sentinel offset value meaning "no data" / empty slot.
pub const INVALID_OFFSET: u32 = 0xFFFF_FFFF;

/// A single record from an `.idx` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    /// Byte offset into the `.mul` file, or [`INVALID_OFFSET`] if unused.
    pub offset: u32,
    /// Length of the entry data in bytes.
    pub length: u32,
    /// Extra/flags field (meaning varies by file type).
    pub extra: u32,
}

impl IndexEntry {
    /// Whether this slot points to valid data.
    pub fn is_valid(&self) -> bool {
        self.offset != INVALID_OFFSET
    }
}

impl Decode<LE> for IndexEntry {
    fn decode<R: ReadPrimitives<LE>>(reader: &mut R) -> Result<Self, u_io::DecodeError> {
        Ok(Self {
            offset: u32::decode(reader)?,
            length: u32::decode(reader)?,
            extra: u32::decode(reader)?,
        })
    }
}

/// Parsed `.idx` file — a flat array of [`IndexEntry`] records.
#[derive(Debug, Clone)]
pub struct MulIndex {
    entries: Vec<IndexEntry>,
}

impl MulIndex {
    /// Read and parse an `.idx` file from disk.
    pub fn read(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        Ok(Self::from_bytes(&data)?)
    }

    /// Parse from a raw byte slice.
    pub fn from_bytes(data: &[u8]) -> Result<Self, u_io::DecodeError> {
        let count = data.len() / INDEX_RECORD_SIZE;
        let mut reader = BinaryReader::<LE>::new(data);
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(IndexEntry::decode(&mut reader)?);
        }
        Ok(Self { entries })
    }

    /// Number of index entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get an entry by its ID (index position).
    pub fn get(&self, id: usize) -> Option<&IndexEntry> {
        self.entries.get(id)
    }

    /// Iterate over all entries.
    pub fn iter(&self) -> impl Iterator<Item = &IndexEntry> {
        self.entries.iter()
    }

    /// Iterate over `(id, entry)` pairs.
    pub fn iter_enumerated(&self) -> impl Iterator<Item = (usize, &IndexEntry)> {
        self.entries.iter().enumerate()
    }
}
