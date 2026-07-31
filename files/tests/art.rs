mod common;

use std::io::Cursor;

use u_io::{BinaryWriter, Encode, LE};

use files::art::{ArtReader, LAND_TILE_LIMIT};
use files::color::Rgb;
use files::mul::{IndexEntry, INVALID_OFFSET};

#[test]
fn land_tile_diamond() {
    // Build a minimal land tile: 44x44 diamond of RGB555 pixels.
    // All opaque pixels set to 0x7FFF (white).
    let mut w = BinaryWriter::<LE>::new();

    let mut left: i32 = 21; // HALF - 1
    let mut right: i32 = 22; // HALF

    let mut pixel_count = 0u32;
    for y in 0..44i32 {
        for _x in left..=right {
            0x7FFFu16.encode(&mut w);
            pixel_count += 1;
        }
        if y < 21 {
            left -= 1;
            right += 1;
        } else if y > 21 {
            left += 1;
            right -= 1;
        }
    }

    let data = w.finish();
    let index = common::make_index(&[IndexEntry {
        offset: 0,
        length: data.len() as u32,
        extra: 0,
    }]);

    let mut reader = ArtReader::from_stream(index, Cursor::new(data.to_vec()));
    let sprite = reader.read_sprite(0).unwrap();

    assert_eq!(sprite.width, 44);
    assert_eq!(sprite.height, 44);
    assert!(sprite.is_land());

    // Center pixel should be opaque
    assert_eq!(sprite.pixel(22, 0), Some(Rgb { r: 255, g: 255, b: 255 }));

    // Corner should be transparent
    assert_eq!(sprite.pixel(0, 0), None);

    assert_eq!(sprite.opaque_count(), pixel_count as usize);
}

#[test]
fn static_sprite_rle() {
    // Build a 4x2 static sprite with simple RLE encoding.
    // Row 0: skip 1, run 2 pixels, end
    // Row 1: skip 0, run 3 pixels, end
    let mut w = BinaryWriter::<LE>::new();

    // flag
    0u32.encode(&mut w);
    // width, height
    4u16.encode(&mut w);
    2u16.encode(&mut w);

    // Row offsets (2 x u16) — relative to data_start in u16 units.
    // Row 0 starts at offset 0 from data_start
    0u16.encode(&mut w);
    // Row 1: row 0 has 3 u16s (xoff, run, pixel, pixel, 0, 0) = 6 u16s
    // Actually: xoff(1) + run(1) + 2 pixels + end(0,0) = 6 u16s
    6u16.encode(&mut w);

    // -- Row 0 data --
    // xoffset=1, run=2
    1u16.encode(&mut w);
    2u16.encode(&mut w);
    // 2 pixels (red)
    0x7C00u16.encode(&mut w); // red
    0x7C00u16.encode(&mut w); // red
    // end of row
    0u16.encode(&mut w);
    0u16.encode(&mut w);

    // -- Row 1 data --
    // xoffset=0, run=3
    0u16.encode(&mut w);
    3u16.encode(&mut w);
    // 3 pixels (green = 0b0_00000_11111_00000 = 0x03E0)
    0x03E0u16.encode(&mut w);
    0x03E0u16.encode(&mut w);
    0x03E0u16.encode(&mut w);
    // end of row
    0u16.encode(&mut w);
    0u16.encode(&mut w);

    let data = w.finish();

    // Art ID 0x4000 (first static)
    let index_id = LAND_TILE_LIMIT;
    let mut entries = vec![IndexEntry { offset: INVALID_OFFSET, length: 0, extra: 0 }; index_id as usize];
    entries.push(IndexEntry {
        offset: 0,
        length: data.len() as u32,
        extra: 0,
    });
    let index = common::make_index(&entries);

    let mut reader = ArtReader::from_stream(index, Cursor::new(data.to_vec()));
    let sprite = reader.read_sprite(index_id).unwrap();

    assert_eq!(sprite.width, 4);
    assert_eq!(sprite.height, 2);
    assert!(!sprite.is_land());

    // Row 0: transparent, red, red, transparent
    assert_eq!(sprite.pixel(0, 0), None);
    assert_eq!(sprite.pixel(1, 0), Some(Rgb { r: 255, g: 0, b: 0 }));
    assert_eq!(sprite.pixel(2, 0), Some(Rgb { r: 255, g: 0, b: 0 }));
    assert_eq!(sprite.pixel(3, 0), None);

    // Row 1: green, green, green, transparent
    assert_eq!(sprite.pixel(0, 1), Some(Rgb { r: 0, g: 255, b: 0 }));
    assert_eq!(sprite.pixel(2, 1), Some(Rgb { r: 0, g: 255, b: 0 }));
    assert_eq!(sprite.pixel(3, 1), None);

    assert_eq!(sprite.opaque_count(), 5);
}

#[test]
fn invalid_id_returns_error() {
    let index = common::make_index(&[]);
    let mut reader = ArtReader::from_stream(index, Cursor::new(vec![]));
    assert!(reader.read_sprite(9999).is_err());
}
