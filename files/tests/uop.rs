use std::io::Cursor;

use u_io::{BinaryWriter, Encode, LE};

use files::uop::{uop_hash, UopEntry, UopIndex, UOP_MAGIC};

#[test]
fn hash_known_value() {
    // Verify the hash for a known map chunk name.
    let name = b"build/map0legacymul/00000000.dat";
    let hash = uop_hash(name);
    // The hash should be deterministic and non-zero.
    assert_ne!(hash, 0);
    // Same input should produce the same hash.
    assert_eq!(hash, uop_hash(name));
}

#[test]
fn hash_different_inputs_differ() {
    let h1 = uop_hash(b"build/map0legacymul/00000000.dat");
    let h2 = uop_hash(b"build/map0legacymul/00000001.dat");
    assert_ne!(h1, h2);
}

#[test]
fn hash_empty_input() {
    let h = uop_hash(b"");
    // Empty input should still produce a deterministic result.
    assert_eq!(h, uop_hash(b""));
}

/// Build a minimal valid UOP file in memory with a single entry block.
fn build_test_uop(entries: &[UopEntry]) -> Vec<u8> {
    let mut w = BinaryWriter::<LE>::new();

    // Header (28 bytes)
    UOP_MAGIC.encode(&mut w);        // magic
    5u32.encode(&mut w);             // version
    0u32.encode(&mut w);             // timestamp
    28u64.encode(&mut w);            // next_block_offset (right after header)
    100u32.encode(&mut w);           // block_size
    (entries.len() as u32).encode(&mut w); // entry_count

    // Entry block header (12 bytes)
    (entries.len() as u32).encode(&mut w); // entry_count in this block
    0u64.encode(&mut w);                   // next_block_offset (0 = last)

    // Entries (34 bytes each)
    for e in entries {
        e.encode(&mut w);
    }

    w.finish().to_vec()
}

#[test]
fn parse_single_entry() {
    let entry = UopEntry {
        data_offset: 200,
        header_length: 8,
        compressed_length: 100,
        decompressed_length: 100,
        entry_hash: 0xDEAD_BEEF_CAFE_BABE,
        crc: 0,
        is_compressed: 0,
    };

    let data = build_test_uop(&[entry]);
    let index = UopIndex::from_stream(Cursor::new(&data)).unwrap();

    assert_eq!(index.len(), 1);
    let found = index.get(0xDEAD_BEEF_CAFE_BABE).unwrap();
    assert_eq!(found.data_offset, 200);
    assert_eq!(found.header_length, 8);
    assert_eq!(found.content_offset(), 208);
    assert_eq!(found.content_length(), 100);
}

#[test]
fn invalid_magic_rejected() {
    let mut data = build_test_uop(&[]);
    // Corrupt the magic bytes
    data[0] = 0xFF;
    let result = UopIndex::from_stream(Cursor::new(&data));
    assert!(result.is_err());
}

#[test]
fn empty_uop() {
    let data = build_test_uop(&[]);
    let index = UopIndex::from_stream(Cursor::new(&data)).unwrap();
    assert_eq!(index.len(), 0);
    assert!(index.is_empty());
}
