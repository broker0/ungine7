mod common;

use std::io::Cursor;

use u_io::{BinaryWriter, Encode, LE};

use files::mul::{IndexEntry, INVALID_OFFSET};
use files::multi::{MultiCollection, MultiFormat};

/// Build a legacy multi part as raw bytes.
fn write_legacy_part(w: &mut BinaryWriter<LE>, tile_id: u16, x: i16, y: i16, z: i16, flags: u32) {
    tile_id.encode(w);
    x.encode(w);
    y.encode(w);
    z.encode(w);
    flags.encode(w);
}

/// Build an HS multi part as raw bytes (legacy + 4-byte clilocs).
fn write_hs_part(w: &mut BinaryWriter<LE>, tile_id: u16, x: i16, y: i16, z: i16, flags: u32, clilocs: u32) {
    tile_id.encode(w);
    x.encode(w);
    y.encode(w);
    z.encode(w);
    flags.encode(w);
    clilocs.encode(w);
}

#[test]
fn legacy_format_detection_and_read() {
    let mut w = BinaryWriter::<LE>::new();
    write_legacy_part(&mut w, 0x1234, 1, -1, 5, 0x01);
    write_legacy_part(&mut w, 0x5678, 2, 0, 10, 0x00);
    let mul_data = w.finish();

    let index = common::make_index(&[
        IndexEntry { offset: 0, length: 24, extra: 0 }, // 2 x 12
        IndexEntry { offset: INVALID_OFFSET, length: 0, extra: 0 },
    ]);

    let multi = MultiCollection::from_stream(&index, Cursor::new(&mul_data)).unwrap();
    assert_eq!(multi.format(), MultiFormat::Legacy);
    assert_eq!(multi.len(), 2);

    let parts = multi.parts(0);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].tile_id, 0x1234);
    assert_eq!(parts[0].x, 1);
    assert_eq!(parts[0].y, -1);
    assert_eq!(parts[0].z, 5);
    assert_eq!(parts[0].flags, 0x01);
    assert_eq!(parts[1].tile_id, 0x5678);

    // Empty slot
    assert!(multi.parts(1).is_empty());
}

#[test]
fn hs_format_detection_and_read() {
    let mut w = BinaryWriter::<LE>::new();
    write_hs_part(&mut w, 0xAAAA, 3, 4, -2, 0xFF, 12345);
    let mul_data = w.finish();

    let index = common::make_index(&[
        IndexEntry { offset: 0, length: 16, extra: 0 }, // 1 x 16
    ]);

    let multi = MultiCollection::from_stream(&index, Cursor::new(&mul_data)).unwrap();
    assert_eq!(multi.format(), MultiFormat::HighSeas);

    let parts = multi.parts(0);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].tile_id, 0xAAAA);
    assert_eq!(parts[0].x, 3);
    assert_eq!(parts[0].y, 4);
    assert_eq!(parts[0].z, -2);
    assert_eq!(parts[0].flags, 0xFF);
}

#[test]
fn out_of_range_id_returns_empty() {
    let index = common::make_index(&[]);
    let multi = MultiCollection::from_stream(&index, Cursor::new(&[])).unwrap();
    assert!(multi.parts(999).is_empty());
}

#[test]
fn iter_skips_empty_slots() {
    let mut w = BinaryWriter::<LE>::new();
    write_legacy_part(&mut w, 0x0001, 0, 0, 0, 0);
    write_legacy_part(&mut w, 0x0002, 1, 1, 1, 0);
    let mul_data = w.finish();

    let index = common::make_index(&[
        IndexEntry { offset: INVALID_OFFSET, length: 0, extra: 0 },
        IndexEntry { offset: 0, length: 12, extra: 0 },
        IndexEntry { offset: 12, length: 12, extra: 0 },
    ]);
    let multi = MultiCollection::from_stream(&index, Cursor::new(&mul_data)).unwrap();

    let ids: Vec<u16> = multi.iter().map(|(id, _)| id).collect();
    assert_eq!(ids, vec![1, 2]);
}
