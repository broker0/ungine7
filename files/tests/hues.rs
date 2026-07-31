use std::io::Cursor;

use u_io::{BinaryWriter, Encode, FixedString, LE};

use files::color::Rgb;
use files::hues::{
    HueEntry, HueTable, ENTRIES_PER_GROUP, GROUP_DISK_SIZE, PALETTE_SIZE,
};

/// Write one complete group (header + 8 entries) into a writer.
fn write_group(w: &mut BinaryWriter<LE>, entries: &[(u16, &str)]) {
    // Group header
    0u32.encode(w);

    for (i, &(base_color, name)) in entries.iter().enumerate().take(ENTRIES_PER_GROUP) {
        // 32 colors — simple gradient: base_color + offset
        for j in 0..PALETTE_SIZE as u16 {
            (base_color + j).encode(w);
        }
        // table_start, table_end
        (i as u16).encode(w);
        ((i + 10) as u16).encode(w);
        // name (20 bytes, null-padded)
        FixedString::<20>::new(name).encode(w);
    }

    // Pad remaining entries if fewer than 8 provided
    for _ in entries.len()..ENTRIES_PER_GROUP {
        for _ in 0..PALETTE_SIZE {
            0u16.encode(w);
        }
        0u16.encode(w);
        0u16.encode(w);
        FixedString::<20>::new("").encode(w);
    }
}

#[test]
fn parse_single_group() {
    let mut w = BinaryWriter::<LE>::new();
    let test_entries = [
        (0x7C00u16, "Red Hue"),
        (0x03E0u16, "Green Hue"),
    ];
    write_group(&mut w, &test_entries);

    let data = w.finish().to_vec();
    assert_eq!(data.len(), GROUP_DISK_SIZE);

    let table = HueTable::from_stream(Cursor::new(data), 1).unwrap();

    assert_eq!(table.len(), ENTRIES_PER_GROUP);

    // First entry: red-based gradient
    let e0 = table.get(0).unwrap();
    assert_eq!(e0.name, "Red Hue");
    assert_eq!(e0.table_start, 0);
    assert_eq!(e0.table_end, 10);
    // First color is rgb555_to_rgb(0x7C00) = pure red
    assert_eq!(e0.colors[0], Rgb { r: 255, g: 0, b: 0 });

    // Second entry: green-based
    let e1 = table.get(1).unwrap();
    assert_eq!(e1.name, "Green Hue");
    assert_eq!(e1.colors[0], Rgb { r: 0, g: 255, b: 0 });

    // Remaining entries should be unnamed
    assert!(table.get(2).unwrap().name.is_empty());
}

#[test]
fn named_count() {
    let entries = vec![
        HueEntry {
            colors: [Rgb { r: 0, g: 0, b: 0 }; PALETTE_SIZE],
            table_start: 0,
            table_end: 0,
            name: "Test".into(),
        },
        HueEntry {
            colors: [Rgb { r: 0, g: 0, b: 0 }; PALETTE_SIZE],
            table_start: 0,
            table_end: 0,
            name: String::new(),
        },
        HueEntry {
            colors: [Rgb { r: 0, g: 0, b: 0 }; PALETTE_SIZE],
            table_start: 0,
            table_end: 0,
            name: "Another".into(),
        },
    ];
    let table = HueTable::from_entries(entries);
    assert_eq!(table.len(), 3);
    assert_eq!(table.named_count(), 2);
}

#[test]
fn empty_table() {
    let table = HueTable::from_entries(vec![]);
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);
    assert_eq!(table.named_count(), 0);
    assert!(table.get(0).is_none());
}
