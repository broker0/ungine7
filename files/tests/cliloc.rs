use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use u_io::{BinaryWriter, LE};

use files::cliloc::ClilocTable;

// ── Helpers ────────────────────────────────────────────────────────────────

/// Build a synthetic plain-format cliloc byte stream.
fn write_cliloc(entries: &[(u32, &str)]) -> Vec<u8> {
    let mut w = BinaryWriter::<LE>::new();

    // Header: magic1 (u32) + magic2 (u16)
    w.put_u32(0x0000_0001); // typical magic1
    w.put_u16(0x0000);      // typical magic2

    for &(id, text) in entries {
        w.put_u32(id);
        w.put_u8(0); // flag
        w.put_u16(text.len() as u16);
        w.put_slice(text.as_bytes());
    }

    w.finish().to_vec()
}

// ── Parsing ────────────────────────────────────────────────────────────────

#[test]
fn parse_basic_entries() {
    let data = write_cliloc(&[
        (1000001, "Hello, world!"),
        (1000002, "A ~1_val~ sword"),
        (1000003, ""),
    ]);

    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();

    assert_eq!(table.len(), 3);
    assert_eq!(table.get(1000001), Some("Hello, world!"));
    assert_eq!(table.get(1000002), Some("A ~1_val~ sword"));
    assert_eq!(table.get(1000003), Some(""));
    assert_eq!(table.get(9999999), None);
}

#[test]
fn parse_empty_file() {
    // Header only, no entries.
    let data = write_cliloc(&[]);
    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);
}

#[test]
fn parse_duplicate_ids_last_wins() {
    let data = write_cliloc(&[
        (42, "first"),
        (42, "second"),
    ]);

    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table.get(42), Some("second"));
}

#[test]
fn header_too_small() {
    // Only 4 bytes — not enough for the 6-byte header.
    let data = vec![0u8; 4];
    let result = ClilocTable::from_stream(Cursor::new(data));
    assert!(result.is_err());
}

// ── Interpolation ──────────────────────────────────────────────────────────

#[test]
fn format_single_arg() {
    let result = ClilocTable::format("~1_val~", &["500"]);
    assert_eq!(result, "500");
}

#[test]
fn format_multiple_args() {
    let result = ClilocTable::format(
        "~1_NAME~ hits ~2_NAME~ for ~3_val~ damage",
        &["You", "orc", "15"],
    );
    assert_eq!(result, "You hits orc for 15 damage");
}

#[test]
fn format_no_placeholders() {
    let result = ClilocTable::format("plain text", &["unused"]);
    assert_eq!(result, "plain text");
}

#[test]
fn format_no_args() {
    let result = ClilocTable::format("~1_val~ gold", &[]);
    // Arg out of range — placeholder left as-is.
    assert_eq!(result, "~1_val~ gold");
}

#[test]
fn format_out_of_range_arg() {
    let result = ClilocTable::format("~3_val~", &["a", "b"]);
    // ~3~ requires index 3, but only 2 args provided.
    assert_eq!(result, "~3_val~");
}

#[test]
fn format_unclosed_tilde() {
    let result = ClilocTable::format("value is ~1_val", &["100"]);
    // No closing `~` — treated literally.
    assert_eq!(result, "value is ~1_val");
}

#[test]
fn format_empty_between_tildes() {
    let result = ClilocTable::format("~~ hello", &["x"]);
    // Empty placeholder — no digit, left as-is.
    assert_eq!(result, "~~ hello");
}

#[test]
fn format_adjacent_placeholders() {
    let result = ClilocTable::format("~1_a~~2_b~", &["foo", "bar"]);
    assert_eq!(result, "foobar");
}

#[test]
fn format_repeated_placeholder() {
    let result = ClilocTable::format("~1_val~ and ~1_val~", &["ok"]);
    assert_eq!(result, "ok and ok");
}

// ── get_formatted / get_formatted_raw ──────────────────────────────────────

#[test]
fn get_formatted_works() {
    let mut map = HashMap::new();
    map.insert(1050045u32, "~1_val~".to_string());
    let table = ClilocTable::from_entries(map);

    assert_eq!(
        table.get_formatted(1050045, &["a magic longsword"]),
        Some("a magic longsword".to_string()),
    );
    assert_eq!(table.get_formatted(9999, &["x"]), None);
}

#[test]
fn get_formatted_raw_tab_separated() {
    let mut map = HashMap::new();
    map.insert(100u32, "~1_a~ and ~2_b~".to_string());
    let table = ClilocTable::from_entries(map);

    assert_eq!(
        table.get_formatted_raw(100, "hello\tworld"),
        Some("hello and world".to_string()),
    );
}

#[test]
fn get_formatted_raw_empty_args() {
    let mut map = HashMap::new();
    map.insert(100u32, "static text".to_string());
    let table = ClilocTable::from_entries(map);

    assert_eq!(
        table.get_formatted_raw(100, ""),
        Some("static text".to_string()),
    );
}

// ── Merge ──────────────────────────────────────────────────────────────────

#[test]
fn merge_combines_tables() {
    let data_a = write_cliloc(&[(1, "one"), (2, "two")]);
    let data_b = write_cliloc(&[(2, "TWO"), (3, "three")]);

    let mut table = ClilocTable::from_stream(Cursor::new(data_a)).unwrap();
    let other = ClilocTable::from_stream(Cursor::new(data_b)).unwrap();
    table.merge(other);

    assert_eq!(table.len(), 3);
    assert_eq!(table.get(1), Some("one"));
    assert_eq!(table.get(2), Some("TWO"));  // overwritten
    assert_eq!(table.get(3), Some("three"));
}

#[test]
fn merge_empty_into_populated() {
    let data = write_cliloc(&[(10, "ten")]);
    let mut table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    table.merge(ClilocTable::empty());

    assert_eq!(table.len(), 1);
    assert_eq!(table.get(10), Some("ten"));
}

// ── Insert ─────────────────────────────────────────────────────────────────

#[test]
fn insert_and_overwrite() {
    let mut table = ClilocTable::empty();

    assert_eq!(table.insert(1, "first".into()), None);
    assert_eq!(table.insert(1, "second".into()), Some("first".into()));
    assert_eq!(table.get(1), Some("second"));
    assert_eq!(table.len(), 1);
}

// ── Iter ───────────────────────────────────────────────────────────────────

#[test]
fn iter_yields_all_entries() {
    let data = write_cliloc(&[(5, "five"), (10, "ten")]);
    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();

    let mut collected: Vec<_> = table.iter().collect();
    collected.sort_by_key(|&(id, _)| id);

    assert_eq!(collected, vec![(5, "five"), (10, "ten")]);
}

// ── Truncated files ────────────────────────────────────────────────────────

#[test]
fn truncated_after_id_returns_preceding_entries() {
    let mut data = write_cliloc(&[(1, "ok"), (2, "also ok")]);
    // Append a partial entry: just an id (4 bytes), no flag/length/text.
    let mut w = BinaryWriter::<LE>::new();
    w.put_u32(999);
    data.extend_from_slice(&w.finish());

    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    assert_eq!(table.len(), 2);
    assert_eq!(table.get(1), Some("ok"));
    assert_eq!(table.get(2), Some("also ok"));
    assert_eq!(table.get(999), None); // truncated entry not included
}

#[test]
fn truncated_after_flag_returns_preceding_entries() {
    let mut data = write_cliloc(&[(1, "ok")]);
    // Partial entry: id + flag, but no length or text.
    let mut w = BinaryWriter::<LE>::new();
    w.put_u32(999);
    w.put_u8(0);
    data.extend_from_slice(&w.finish());

    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table.get(1), Some("ok"));
}

#[test]
fn truncated_in_text_body_returns_preceding_entries() {
    let mut data = write_cliloc(&[(1, "ok")]);
    // Partial entry: id + flag + length=100, but only 5 bytes of text.
    let mut w = BinaryWriter::<LE>::new();
    w.put_u32(999);
    w.put_u8(0);
    w.put_u16(100); // claims 100 bytes
    w.put_slice(b"short"); // only 5 bytes
    data.extend_from_slice(&w.finish());

    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table.get(1), Some("ok"));
}

// ── Non-UTF-8 tolerance ────────────────────────────────────────────────────

#[test]
fn non_utf8_bytes_replaced_with_replacement_char() {
    let mut w = BinaryWriter::<LE>::new();
    // Header
    w.put_u32(0x01);
    w.put_u16(0x00);
    // Entry with invalid UTF-8 bytes
    w.put_u32(42);
    w.put_u8(0);
    let bad_bytes: &[u8] = &[b'h', b'e', 0xFF, b'l', b'o'];
    w.put_u16(bad_bytes.len() as u16);
    w.put_slice(bad_bytes);
    let data = w.finish().to_vec();

    let table = ClilocTable::from_stream(Cursor::new(data)).unwrap();
    assert_eq!(table.len(), 1);
    let text = table.get(42).unwrap();
    assert!(text.contains('\u{FFFD}')); // replacement character
    assert!(text.starts_with("he"));
    assert!(text.ends_with("lo"));
}

// ── Format detection (read_file) ───────────────────────────────────────────

#[test]
fn read_file_form_returns_invalid_input_error() {
    // IFF/FORM file should be rejected with InvalidInput.
    let form_path = Path::new("cliloc/3+/Chat.enu");
    if !form_path.exists() {
        // Running from workspace root or files/ — try both.
        let alt = Path::new("files/cliloc/3+/Chat.enu");
        if !alt.exists() {
            return; // skip if test data not available
        }
        let err = ClilocTable::read_file(alt).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        return;
    }
    let err = ClilocTable::read_file(form_path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
