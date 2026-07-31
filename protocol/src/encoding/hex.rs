/// Simple hex encoding/decoding utilities.
/// Intentionally minimal and allocation-friendly.
use std::fmt::Write;

/// Encodes bytes as a lowercase hexadecimal string.
pub fn encode_hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for &byte in data {
        write!(&mut s, "{:02x}", byte).unwrap();
    }
    s
}

/// Decodes a hexadecimal string into bytes.
/// Returns `None` if the string contains invalid hex characters or has odd length.
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }

    let mut bytes = Vec::with_capacity(s.len() / 2);
    let mut chars = s.chars();

    while let (Some(c1), Some(c2)) = (chars.next(), chars.next()) {
        let high = hex_char_to_u8(c1)?;
        let low = hex_char_to_u8(c2)?;
        bytes.push((high << 4) | low);
    }

    Some(bytes)
}

fn hex_char_to_u8(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some((c as u8) - b'0'),
        'a'..='f' => Some((c as u8) - b'a' + 10),
        'A'..='F' => Some((c as u8) - b'A' + 10),
        _ => None,
    }
}
