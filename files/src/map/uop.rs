//! UOP map loader (`map{N}LegacyMUL.uop`).
//!
//! In newer UO clients, map data is stored inside a UOP container.
//! The blocks are split into chunks identified by hash of
//! `build/map{N}legacymul/{entry:08}.dat`.  Within each chunk the
//! blocks are stored sequentially in the same format as the MUL file.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

use log::debug;
use u_io::{DecodeError, StreamReader, LE};

use super::{MapBlock, MapData, BLOCK_DISK_SIZE};
use crate::uop::{uop_hash, UopIndex};

/// Load from `map{world}LegacyMUL.uop` with the given block dimensions.
pub fn read(
    dir: &Path,
    world: u8,
    x_blocks: usize,
    y_blocks: usize,
) -> io::Result<MapData> {
    let path = dir.join(format!("map{world}LegacyMUL.uop"));

    let uop_index = UopIndex::read(&path)?;

    let file = File::open(&path)?;
    let buf = BufReader::new(file);
    let mut reader = StreamReader::<_, LE>::new(buf);

    debug!(
        "map{world}LegacyMUL.uop: x_blocks={x_blocks}, y_blocks={y_blocks}, \
         uop_entries={}",
        uop_index.len(),
    );

    Ok(parse(&mut reader, &uop_index, world, x_blocks, y_blocks)?)
}

fn parse<R: Read + Seek>(
    reader: &mut StreamReader<R, LE>,
    uop_index: &UopIndex,
    world: u8,
    x_blocks: usize,
    y_blocks: usize,
) -> Result<MapData, DecodeError> {
    let max_block = x_blocks * y_blocks;
    let mut blocks = Vec::with_capacity(max_block);

    while blocks.len() < max_block {
        let next_block = blocks.len();
        let entry_num = next_block >> 12;
        let chunk_name = format!("build/map{world}legacymul/{entry_num:08}.dat");
        let entry_hash = uop_hash(chunk_name.as_bytes());

        let entry = uop_index.get(entry_hash).ok_or_else(|| {
            DecodeError::Other(format!(
                "UOP chunk not found: {chunk_name} (hash=0x{entry_hash:016X})",
            ))
        })?;

        let data_offset = entry.content_offset();
        let data_size = entry.content_length() as usize;
        let blocks_in_chunk = data_size / BLOCK_DISK_SIZE;

        reader.seek_to(data_offset)?;

        let blocks_to_read = blocks_in_chunk.min(max_block - blocks.len());
        for _ in 0..blocks_to_read {
            blocks.push(MapBlock::decode_from(reader)?);
        }
    }

    Ok(MapData::new(blocks, x_blocks, y_blocks))
}
