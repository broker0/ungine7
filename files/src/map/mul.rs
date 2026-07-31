//! MUL map loader (`map{N}.mul`).
//!
//! MUL map files contain blocks sequentially — no index file needed.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::Path;

use log::debug;

use super::{MapData, BLOCK_DISK_SIZE};

/// Load from `map{world}.mul` with the given block dimensions.
pub fn read(
    dir: &Path,
    world: u8,
    x_blocks: usize,
    y_blocks: usize,
) -> io::Result<MapData> {
    let path = dir.join(format!("map{world}.mul"));

    debug!(
        "map{world}.mul: x_blocks={x_blocks}, y_blocks={y_blocks}, \
         expected_size={}",
        x_blocks * y_blocks * BLOCK_DISK_SIZE,
    );

    let file = File::open(&path)?;
    let buf = BufReader::new(file);
    Ok(MapData::from_stream(buf, x_blocks, y_blocks)?)
}

/// Calculate `x_blocks` from a MUL file size and known `y_blocks`.
pub fn calc_width(dir: &Path, world: u8, y_blocks: usize) -> io::Result<usize> {
    let path = dir.join(format!("map{world}.mul"));
    let file_size = fs::metadata(&path)?.len() as usize;
    let total_blocks = file_size / BLOCK_DISK_SIZE;
    Ok(total_blocks / y_blocks)
}
