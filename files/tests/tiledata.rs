use std::io::Cursor;

use u_io::{BinaryWriter, Encode, LE};

use files::tiledata::{
    TileData, TileDataFormat, TileFlags, LAND_GROUPS, LAND_TILE_COUNT, TILES_PER_GROUP,
};

/// Write a group header.
fn write_header(w: &mut BinaryWriter<LE>) {
    0u32.encode(w);
}

/// Write a legacy land tile.
fn write_legacy_land(w: &mut BinaryWriter<LE>, flags: u32, tex: u16, name: &str) {
    flags.encode(w);
    tex.encode(w);
    let mut buf = [0u8; 20];
    let bytes = name.as_bytes();
    buf[..bytes.len().min(20)].copy_from_slice(&bytes[..bytes.len().min(20)]);
    w.put_slice(&buf);
}

/// Write a legacy static tile.
fn write_legacy_static(w: &mut BinaryWriter<LE>, flags: u32, height: u8, name: &str) {
    flags.encode(w);
    // weight, quality
    0u8.encode(w); 0u8.encode(w);
    // unk1(u16), unk2(u8), quantity(u8)
    0u16.encode(w); 0u8.encode(w); 0u8.encode(w);
    // anim_id(u16)
    0u16.encode(w);
    // unk3(u8), hue(u8)
    0u8.encode(w); 0u8.encode(w);
    // unk4(u16), height(u8)
    0u16.encode(w); height.encode(w);
    // name
    let mut buf = [0u8; 20];
    let bytes = name.as_bytes();
    buf[..bytes.len().min(20)].copy_from_slice(&bytes[..bytes.len().min(20)]);
    w.put_slice(&buf);
}

#[test]
fn legacy_land_tiles_parse() {
    let mut w = BinaryWriter::<LE>::new();

    // Write 512 groups x 32 land tiles
    for g in 0..LAND_GROUPS {
        write_header(&mut w);
        for t in 0..TILES_PER_GROUP {
            let idx = g * TILES_PER_GROUP + t;
            write_legacy_land(&mut w, idx as u32, idx as u16, "");
        }
    }

    // Write 1 group of 32 static tiles (to have some content)
    write_header(&mut w);
    for t in 0..TILES_PER_GROUP {
        write_legacy_static(&mut w, 0x40, t as u8, "item");
    }

    let data = w.finish();
    let len = data.len() as u64;
    let td = TileData::from_stream(Cursor::new(&data), len).unwrap();

    assert_eq!(td.format(), TileDataFormat::Legacy);
    assert_eq!(td.land_tiles().len(), LAND_TILE_COUNT);
    assert_eq!(td.static_tiles().len(), TILES_PER_GROUP);

    // Check a specific land tile
    let tile = td.land(100).unwrap();
    assert_eq!(tile.flags.raw(), 100);
    assert_eq!(tile.texture_id, 100);

    // Check a static tile
    let st = td.static_tile(0).unwrap();
    assert_eq!(st.flags.raw(), 0x40);
    assert!(st.flags.has(TileFlags::IMPASSABLE));
    assert_eq!(st.height, 0);

    let st5 = td.static_tile(5).unwrap();
    assert_eq!(st5.height, 5);
}

#[test]
fn named_tiles_preserved() {
    let mut w = BinaryWriter::<LE>::new();

    for _g in 0..LAND_GROUPS {
        write_header(&mut w);
        for _ in 0..TILES_PER_GROUP {
            write_legacy_land(&mut w, 0, 0, "grass");
        }
    }

    let data = w.finish();
    let len = data.len() as u64;
    let td = TileData::from_stream(Cursor::new(&data), len).unwrap();

    assert_eq!(&*td.land(0).unwrap().name, "grass");
}
