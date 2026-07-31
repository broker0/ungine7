use std::io::Cursor;

use u_io::{BinaryWriter, Encode, LE};

use files::map::{default_dimensions, MapData};

/// Write a single map block (header + 64 tiles) to a writer.
fn write_block(w: &mut BinaryWriter<LE>, base_tile_id: u16, base_z: i8) {
    0u32.encode(w);
    for y in 0..8u16 {
        for x in 0..8u16 {
            let tile_id = base_tile_id + x + y * 8;
            tile_id.encode(w);
            (base_z + x as i8).encode(w);
        }
    }
}

#[test]
fn parse_single_block() {
    let mut w = BinaryWriter::<LE>::new();
    write_block(&mut w, 100, 5);
    let data = w.finish().to_vec();

    let map = MapData::from_stream(Cursor::new(&data), 1, 1).unwrap();

    assert_eq!(map.x_blocks(), 1);
    assert_eq!(map.y_blocks(), 1);
    assert_eq!(map.total_blocks(), 1);
    assert_eq!(map.width(), 8);
    assert_eq!(map.height(), 8);

    let t = map.tile_at(0, 0);
    assert_eq!(t.tile_id, 100);
    assert_eq!(t.z, 5);

    // tile at (3, 2): tile_id = 100 + 3 + 2*8 = 119, z = 5 + 3 = 8
    let t = map.tile_at(3, 2);
    assert_eq!(t.tile_id, 119);
    assert_eq!(t.z, 8);
}

#[test]
fn parse_2x2_grid() {
    let mut w = BinaryWriter::<LE>::new();
    // Column-major: (0,0), (0,1), (1,0), (1,1)
    write_block(&mut w, 0, 0);
    write_block(&mut w, 10, 1);
    write_block(&mut w, 20, 2);
    write_block(&mut w, 30, 3);
    let data = w.finish().to_vec();

    let map = MapData::from_stream(Cursor::new(&data), 2, 2).unwrap();

    assert_eq!(map.x_blocks(), 2);
    assert_eq!(map.y_blocks(), 2);
    assert_eq!(map.width(), 16);
    assert_eq!(map.height(), 16);

    assert_eq!(map.block_at(0, 0).cells[0][0].tile_id, 0);
    assert_eq!(map.block_at(1, 0).cells[0][0].tile_id, 20);
    assert_eq!(map.block_at(0, 1).cells[0][0].tile_id, 10);
    assert_eq!(map.block_at(1, 1).cells[0][0].tile_id, 30);

    assert_eq!(map.tile_at(8, 0).tile_id, 20);
    assert_eq!(map.tile_at(0, 8).tile_id, 10);
}

#[test]
fn default_dimensions_known_worlds() {
    assert_eq!(default_dimensions(0), Some((768, 512)));
    assert_eq!(default_dimensions(1), Some((768, 512)));
    assert_eq!(default_dimensions(2), Some((288, 200)));
    assert_eq!(default_dimensions(3), Some((320, 256)));
    assert_eq!(default_dimensions(4), Some((181, 181)));
    assert_eq!(default_dimensions(5), Some((160, 512)));
    assert_eq!(default_dimensions(6), None);
}
