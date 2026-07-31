//! UOP container reader.
//!
//! UOP (Ultima Online Package) is a generic container format used by newer
//! UO clients for map data, art, gumps, and other assets.  Each `.uop` file
//! contains a header, a linked list of entry blocks, and entry data scattered
//! throughout the file.  Entries are identified by a hash of their logical
//! name (e.g. `build/map0legacymul/00000000.dat`).
//!
//! # File structure
//!
//! ```text
//! UopHeader  (28 bytes)
//!   ├─ magic            u32   (must be 0x0050594D = "MYP\0")
//!   ├─ version          u32
//!   ├─ timestamp        u32
//!   ├─ next_block_offset u64  (offset to first entry block)
//!   ├─ block_size       u32   (max entries per block)
//!   └─ entry_count      u32   (total entries in file)
//!
//! EntryBlock  (at next_block_offset):
//!   BlockHeader (12 bytes)
//!     ├─ entry_count      u32
//!     └─ next_block_offset u64  (0 = last block)
//!   followed by entry_count × UopEntry (34 bytes each)
//! ```
//!
//! # Hash function
//!
//! Entry lookup uses a Jenkins-style hash (see [`uop_hash`]).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::num::Wrapping;
use std::path::Path;

use log::debug;
use u_io::{Decode, DecodeError, StreamReader};
use macros::{Decode, Encode};

// ── Magic ─────────────────────────────────────────────────────────────────

/// UOP file signature: `"MYP\0"` as little-endian u32.
pub const UOP_MAGIC: u32 = 0x0050594D;

// ── Wire structures ───────────────────────────────────────────────────────

/// File-level header at offset 0 of a `.uop` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode, Encode)]
#[binary(endian = "le")]
pub struct UopHeader {
    pub magic: u32,
    pub version: u32,
    pub timestamp: u32,
    pub next_block_offset: u64,
    pub block_size: u32,
    pub entry_count: u32,
}

/// Header of an entry block in the linked list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode, Encode)]
#[binary(endian = "le")]
pub struct UopBlockHeader {
    pub entry_count: u32,
    pub next_block_offset: u64,
}

/// A single entry descriptor within an entry block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode, Encode)]
#[binary(endian = "le")]
pub struct UopEntry {
    pub data_offset: u64,
    pub header_length: u32,
    pub compressed_length: u32,
    pub decompressed_length: u32,
    pub entry_hash: u64,
    pub crc: u32,
    pub is_compressed: u16,
}

impl UopEntry {
    /// Absolute offset of the actual data (past the per-entry header).
    pub fn content_offset(&self) -> u64 {
        self.data_offset + self.header_length as u64
    }

    /// Size of the uncompressed data.
    pub fn content_length(&self) -> u32 {
        self.decompressed_length
    }
}

// ── UopIndex ──────────────────────────────────────────────────────────────

/// Parsed UOP file index — maps entry hashes to [`UopEntry`] descriptors.
///
/// Only the index (header + entry table) is parsed and stored in memory.
/// The actual data remains in the file and must be read via seek.
#[derive(Debug)]
pub struct UopIndex {
    entries: HashMap<u64, UopEntry>,
}

impl UopIndex {
    /// Parse a UOP index from a `.uop` file on disk.
    pub fn read(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let buf = BufReader::new(file);
        Ok(Self::from_stream(buf)?)
    }

    /// Parse a UOP index from any readable + seekable stream.
    pub fn from_stream<S: Read + Seek>(stream: S) -> Result<Self, DecodeError> {
        let mut reader = StreamReader::<_, u_io::LE>::new(stream);
        Self::parse(&mut reader)
    }

    fn parse<R: Read + Seek>(
        reader: &mut StreamReader<R, u_io::LE>,
    ) -> Result<Self, DecodeError> {
        let header = UopHeader::decode(reader)?;

        if header.magic != UOP_MAGIC {
            return Err(DecodeError::Other(format!(
                "invalid UOP magic: 0x{:08X} (expected 0x{:08X})",
                header.magic, UOP_MAGIC,
            )));
        }

        debug!(
            "uop: version={}, entries={}, block_size={}",
            header.version, header.entry_count, header.block_size,
        );

        let mut entries = HashMap::with_capacity(header.entry_count as usize);
        let mut block_offset = header.next_block_offset;

        while block_offset != 0 {
            reader.seek_to(block_offset)?;
            let block_header = UopBlockHeader::decode(reader)?;

            for _ in 0..block_header.entry_count {
                let entry = UopEntry::decode(reader)?;
                if entry.data_offset != 0 {
                    entries.insert(entry.entry_hash, entry);
                }
            }

            block_offset = block_header.next_block_offset;
        }

        Ok(Self { entries })
    }

    /// Look up an entry by its hash.
    pub fn get(&self, hash: u64) -> Option<&UopEntry> {
        self.entries.get(&hash)
    }

    /// Look up an entry by the logical file name (computes hash internally).
    pub fn get_by_name(&self, name: &str) -> Option<&UopEntry> {
        let hash = uop_hash(name.as_bytes());
        self.entries.get(&hash)
    }

    /// Number of entries in the index.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all `(hash, entry)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &UopEntry)> {
        self.entries.iter()
    }
}

// ── Hash function ─────────────────────────────────────────────────────────

/// Compute the UOP entry hash for a logical file name.
///
/// This is a Jenkins-style hash used to identify entries within a `.uop`
/// container.  The input should be the entry's logical path, e.g.
/// `b"build/map0legacymul/00000000.dat"`.
pub fn uop_hash(mut src: &[u8]) -> u64 {
    let mut a = Wrapping((src.len() as u32).wrapping_add(0xdeadbeef));
    let mut b = a;
    let mut c = a;

    while src.len() > 12 {
        a += partial_read_u32(src);
        b += partial_read_u32(&src[4..]);
        c += partial_read_u32(&src[8..]);

        a = (a - c) ^ ((c << 4) | (c >> 28));
        c += b;
        b = (b - a) ^ ((a << 6) | (a >> 26));
        a += c;
        c = (c - b) ^ ((b << 8) | (b >> 24));
        b += a;
        a = (a - c) ^ ((c << 16) | (c >> 16));
        c += b;
        b = (b - a) ^ ((a << 19) | (a >> 13));
        a += c;
        c = (c - b) ^ ((b << 4) | (b >> 28));
        b += a;

        src = &src[12..];
    }

    if !src.is_empty() {
        a += partial_read_u32(src);
        b += partial_read_u32(&src[4..]);
        c += partial_read_u32(&src[8..]);

        c = (c ^ b) - ((b << 14) | (b >> 18));
        a = (a ^ c) - ((c << 11) | (c >> 21));
        b = (b ^ a) - ((a << 25) | (a >> 7));
        c = (c ^ b) - ((b << 16) | (b >> 16));
        a = (a ^ c) - ((c << 4) | (c >> 28));
        b = (b ^ a) - ((a << 14) | (a >> 18));
        c = (c ^ b) - ((b << 24) | (b >> 8));
    }

    ((b.0 as u64) << 32) | (c.0 as u64)
}

/// Read up to 4 bytes from a slice as a little-endian u32,
/// padding with zeroes for shorter slices.
fn partial_read_u32(s: &[u8]) -> Wrapping<u32> {
    let a = *s.first().unwrap_or(&0) as u32;
    let b = *s.get(1).unwrap_or(&0) as u32;
    let c = *s.get(2).unwrap_or(&0) as u32;
    let d = *s.get(3).unwrap_or(&0) as u32;

    Wrapping(a | (b << 8) | (c << 16) | (d << 24))
}
