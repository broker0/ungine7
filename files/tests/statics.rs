mod common;

use std::io::Cursor;

use u_io::{BinaryWriter, Encode, LE};

use files::mul::{IndexEntry, INVALID_OFFSET};
use files::statics::{StaticData, TILE_DISK_SIZE};

fn write_tile(w: &mut BinaryWriter<LE>, tile_id: u16, x: u8, y: u8, z: i8, hue: u16) {
    tile_id.encode(w);
    x.encode(w);
    y.encode(w);
    z.encode(w);
    hue.encode(w);
}

#[test]
fn parse_single_block_with_tiles() {
    let mut w = BinaryWriter::<LE>::new();
    // 3 tiles in block 0
    write_tile(&mut w, 0x100, 3, 5, 10, 0);
    write_tile(&mut w, 0x200, 1, 2, -5, 100);
    write_tile(&mut w, 0x300, 3, 5, 20, 0);
    let data = w.finish().to_vec();

    let index = common::make_index(&[
        IndexEntry { offset: 0, length: (3 * TILE_DISK_SIZE) as u32, extra: 0 },
    ]);

    let sd = StaticData::from_stream(&index, Cursor::new(&data), 1, 1).unwrap();

    assert_eq!(sd.total_tiles(), 3);
    assert_eq!(sd.total_blocks(), 1);
    assert_eq!(sd.non_empty_blocks(), 1);

    let block = sd.block(0);
    assert_eq!(block.len(), 3);

    // Tiles should be sorted by (x, y, z)
    // (1,2,-5), (3,5,10), (3,5,20)
    assert_eq!(block[0].tile_id, 0x200);
    assert_eq!(block[0].x, 1);
    assert_eq!(block[0].y, 2);
    assert_eq!(block[0].z, -5);

    assert_eq!(block[1].tile_id, 0x100);
    assert_eq!(block[2].tile_id, 0x300);
}

#[test]
fn block_tile_lookup() {
    let mut w = BinaryWriter::<LE>::new();
    // Tiles at different (x,y) positions
    write_tile(&mut w, 1, 0, 0, 0, 0);
    write_tile(&mut w, 2, 0, 0, 5, 0);
    write_tile(&mut w, 3, 3, 3, 0, 0);
    write_tile(&mut w, 4, 7, 7, 0, 0);
    let data = w.finish().to_vec();

    let index = common::make_index(&[
        IndexEntry { offset: 0, length: (4 * TILE_DISK_SIZE) as u32, extra: 0 },
    ]);

    let sd = StaticData::from_stream(&index, Cursor::new(&data), 1, 1).unwrap();

    // Two tiles at (0, 0)
    let at_00 = sd.block_tile(0, 0, 0);
    assert_eq!(at_00.len(), 2);
    assert_eq!(at_00[0].tile_id, 1);
    assert_eq!(at_00[1].tile_id, 2);

    // One tile at (3, 3)
    let at_33 = sd.block_tile(0, 3, 3);
    assert_eq!(at_33.len(), 1);
    assert_eq!(at_33[0].tile_id, 3);

    // No tiles at (5, 5)
    let at_55 = sd.block_tile(0, 5, 5);
    assert!(at_55.is_empty());
}

#[test]
fn empty_block_returns_empty_slice() {
    let index = common::make_index(&[
        IndexEntry { offset: INVALID_OFFSET, length: 0, extra: 0 },
    ]);

    let sd = StaticData::from_stream(&index, Cursor::new(&[]), 1, 1).unwrap();

    assert_eq!(sd.total_tiles(), 0);
    assert_eq!(sd.non_empty_blocks(), 0);
    assert!(sd.block(0).is_empty());
    assert!(sd.block_tile(0, 0, 0).is_empty());
}

#[test]
fn mixed_blocks() {
    let mut w = BinaryWriter::<LE>::new();
    // Block 0: 1 tile at offset 0
    write_tile(&mut w, 0xAA, 2, 3, 0, 0);
    // Block 2: 2 tiles at offset 7
    write_tile(&mut w, 0xBB, 0, 0, 0, 0);
    write_tile(&mut w, 0xCC, 1, 1, 5, 0);
    let data = w.finish().to_vec();

    let index = common::make_index(&[
        IndexEntry { offset: 0, length: TILE_DISK_SIZE as u32, extra: 0 },
        IndexEntry { offset: INVALID_OFFSET, length: 0, extra: 0 },           // empty
        IndexEntry { offset: TILE_DISK_SIZE as u32, length: (2 * TILE_DISK_SIZE) as u32, extra: 0 },
    ]);

    // 3 blocks in a 3x1 grid (or 1x3, doesn't matter for linear access)
    let sd = StaticData::from_stream(&index, Cursor::new(&data), 3, 1).unwrap();

    assert_eq!(sd.total_tiles(), 3);
    assert_eq!(sd.non_empty_blocks(), 2);

    assert_eq!(sd.block(0).len(), 1);
    assert_eq!(sd.block(0)[0].tile_id, 0xAA);

    assert!(sd.block(1).is_empty());

    assert_eq!(sd.block(2).len(), 2);
    assert_eq!(sd.block(2)[0].tile_id, 0xBB); // sorted: (0,0,0) first
    assert_eq!(sd.block(2)[1].tile_id, 0xCC); // (1,1,5) second
}
