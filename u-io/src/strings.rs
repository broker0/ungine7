//! String wire-format types: fixed-length, null-terminated ASCII and Unicode.

use std::ops::Deref;

use crate::endian::ByteOrder;
use crate::error::DecodeError;
use crate::reader::ReadPrimitives;
use crate::traits::{Decode, Encode};
use crate::writer::BinaryWriter;

/// Implement `new`, `into_inner`, `Deref<str>`, `From<String>`, `From<&str>`
/// for a `String` newtype.
///
/// Accepts two forms:
/// - Plain:         `impl_string_newtype!(NullString);`
/// - Const-generic: `impl_string_newtype!(FixedString<const N: usize>);`
macro_rules! impl_string_newtype {
    ($Name:ident < const $N:ident : usize >) => {
        impl<const $N: usize> $Name<$N> {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn into_inner(self) -> String { self.0 }
        }
        impl<const $N: usize> Deref for $Name<$N> {
            type Target = str;
            fn deref(&self) -> &str { &self.0 }
        }
        impl<const $N: usize> From<String> for $Name<$N> {
            fn from(s: String) -> Self { Self(s) }
        }
        impl<const $N: usize> From<&str> for $Name<$N> {
            fn from(s: &str) -> Self { Self(s.to_string()) }
        }
    };
    ($Name:ident) => {
        impl $Name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn into_inner(self) -> String { self.0 }
        }
        impl Deref for $Name {
            type Target = str;
            fn deref(&self) -> &str { &self.0 }
        }
        impl From<String> for $Name {
            fn from(s: String) -> Self { Self(s) }
        }
        impl From<&str> for $Name {
            fn from(s: &str) -> Self { Self(s.to_string()) }
        }
    };
}

// ── FixedString<N> ─────────────────────────────────────────────────────────

/// A fixed-length null-padded ASCII string.
///
/// On the wire it always occupies exactly `N` bytes, padded with `\0`.
/// When read, trailing nulls are stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct FixedString<const N: usize>(pub String);

impl_string_newtype!(FixedString<const N: usize>);

impl<const N: usize, E: ByteOrder> Decode<E> for FixedString<N> {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let mut buf = [0u8; N];
        reader.read_bytes(&mut buf)?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(N);
        let s = std::str::from_utf8(&buf[..end])
            .map_err(|_| DecodeError::InvalidString)?
            .to_owned();
        Ok(Self(s))
    }
}

impl<const N: usize, E: ByteOrder> Encode<E> for FixedString<N> {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        let bytes = self.0.as_bytes();
        let len = bytes.len().min(N);
        writer.put_slice(&bytes[..len]);
        if len < N {
            writer.put_bytes(0, N - len);
        }
    }
}

// ── RawBytes<N> ────────────────────────────────────────────────────────────

/// A fixed-length raw byte array stored and transmitted verbatim.
///
/// Unlike [`FixedString<N>`], no null-stripping or UTF-8 interpretation is
/// performed. All `N` bytes are read and written exactly as they appear on
/// the wire, preserving any padding or quirk bytes that a real server may
/// write after the meaningful content.
///
/// Useful where the protocol field is nominally a null-padded ASCII string
/// but real implementations sometimes place non-zero bytes in the padding
/// region (e.g. `GameServerEntry::name`).
#[derive(Clone, PartialEq, Eq)]
pub struct RawBytes<const N: usize>(pub [u8; N]);

// ── Serde: serialize as hex string, deserialize from hex string ────────────

#[cfg(feature = "serde")]
impl<const N: usize> serde::Serialize for RawBytes<N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Encode each byte as two lowercase hex digits.
        let mut hex = String::with_capacity(N * 2);
        for &b in &self.0 {
            use std::fmt::Write;
            write!(hex, "{b:02x}").unwrap();
        }
        serializer.serialize_str(&hex)
    }
}

#[cfg(feature = "serde")]
impl<'de, const N: usize> serde::Deserialize<'de> for RawBytes<N> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = <String as serde::Deserialize>::deserialize(deserializer)?;
        if hex.len() != N * 2 {
            return Err(serde::de::Error::invalid_length(
                hex.len(),
                &format!("hex string of length {}", N * 2).as_str(),
            ));
        }
        let mut buf = [0u8; N];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let hi = hex_digit(chunk[0]).ok_or_else(|| {
                serde::de::Error::custom(format!("invalid hex digit: {:?}", chunk[0] as char))
            })?;
            let lo = hex_digit(chunk[1]).ok_or_else(|| {
                serde::de::Error::custom(format!("invalid hex digit: {:?}", chunk[1] as char))
            })?;
            buf[i] = (hi << 4) | lo;
        }
        Ok(Self(buf))
    }
}

#[cfg(feature = "serde")]
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl<const N: usize> RawBytes<N> {
    /// Create from a string, UTF-8 encoded, zero-padded to `N` bytes.
    /// Panics if the string is longer than `N` bytes.
    pub fn from_str(s: &str) -> Self {
        let bytes = s.as_bytes();
        assert!(bytes.len() <= N, "string too long for RawBytes<{}>", N);
        let mut buf = [0u8; N];
        buf[..bytes.len()].copy_from_slice(bytes);
        Self(buf)
    }

    /// Create from a string, truncating to at most `N - 1` bytes to
    /// guarantee a null terminator at the end.
    ///
    /// This is the safe variant for UO protocol fields that require
    /// null-terminated names (e.g. 32-byte server/character name fields).
    /// Unlike [`from_str`](Self::from_str), this never panics.
    pub fn from_str_terminated(s: &str) -> Self {
        let bytes = s.as_bytes();
        let max = if N > 0 { N - 1 } else { 0 };
        let len = bytes.len().min(max);
        let mut buf = [0u8; N];
        buf[..len].copy_from_slice(&bytes[..len]);
        Self(buf)
    }

    /// Decode the content as a null-terminated UTF-8/ASCII string,
    /// stripping bytes from the first `\0` onward.
    pub fn as_str_lossy(&self) -> &str {
        let end = self.0.iter().position(|&b| b == 0).unwrap_or(N);
        std::str::from_utf8(&self.0[..end]).unwrap_or("")
    }
}

impl<const N: usize> std::fmt::Debug for RawBytes<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RawBytes({:?})", self.as_str_lossy())
    }
}

impl<const N: usize> From<&str> for RawBytes<N> {
    fn from(s: &str) -> Self { Self::from_str(s) }
}

impl<const N: usize> From<String> for RawBytes<N> {
    fn from(s: String) -> Self { Self::from_str(&s) }
}

impl<const N: usize, E: ByteOrder> Decode<E> for RawBytes<N> {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let mut buf = [0u8; N];
        reader.read_bytes(&mut buf)?;
        Ok(Self(buf))
    }
}

impl<const N: usize, E: ByteOrder> Encode<E> for RawBytes<N> {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_slice(&self.0);
    }
}

// ── NullString ─────────────────────────────────────────────────────────────

/// A null-terminated ASCII string.
///
/// On the wire the string is a variable number of bytes followed by a `0x00`
/// terminator.  The terminator is consumed on read and written on write, but
/// is **not** part of the stored value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct NullString(pub String);

impl_string_newtype!(NullString);

impl<E: ByteOrder> Decode<E> for NullString {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let mut bytes = Vec::new();
        loop {
            let b = reader.read_u8()?;
            if b == 0 { break; }
            bytes.push(b);
        }
        let s = String::from_utf8(bytes).map_err(|_| DecodeError::InvalidString)?;
        Ok(Self(s))
    }
}

impl<E: ByteOrder> Encode<E> for NullString {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_slice(self.0.as_bytes());
        writer.put_u8(0);
    }
}

// ── NullUnicodeString ──────────────────────────────────────────────────────

/// A null-terminated UTF-16 string.
///
/// On the wire the string is a variable number of `u16` code units (encoded
/// with byte order `E`) followed by a `0x0000` terminator.  The terminator is
/// consumed on read and written on write, but is **not** part of the stored
/// value.
///
/// UO network packets always use big-endian for Unicode text, but the
/// implementation is generic over byte order for consistency.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct NullUnicodeString(pub String);

impl_string_newtype!(NullUnicodeString);

impl<E: ByteOrder> Decode<E> for NullUnicodeString {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let mut units = Vec::new();
        loop {
            let u = reader.read_u16()?;
            if u == 0 { break; }
            units.push(u);
        }
        let s = String::from_utf16_lossy(&units).to_owned();
        Ok(Self(s))
    }
}

impl<E: ByteOrder> Encode<E> for NullUnicodeString {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        for unit in self.0.encode_utf16() {
            writer.put_u16(unit);
        }
        writer.put_u16(0);
    }
}

// ── LE UTF-16 helpers ──────────────────────────────────────────────────────
//
// Several UO packets embed **little-endian** UTF-16 strings inside an
// otherwise big-endian packet (e.g. cliloc arguments in 0xC1, buff args
// in 0xDF, tooltip args in 0xD6).  These helpers centralise the pattern
// so that callers don't have to hand-code LE byte manipulation inside a
// `BinaryWriter<BE>` / `BinaryReader<BE>` context.

/// Encode `s` as a null-terminated **little-endian** UTF-16 string into a
/// [`BinaryWriter`] of *any* byte order.
///
/// Each UTF-16 code unit is written as two raw bytes in LE order, followed
/// by a `0x0000` null terminator (also LE).
///
/// This is the encoding counterpart to [`decode_le_utf16_str`].
pub fn encode_le_utf16_str<E: ByteOrder>(s: &str, w: &mut BinaryWriter<E>) {
    for unit in s.encode_utf16() {
        w.put_slice(&unit.to_le_bytes());
    }
    w.put_slice(&[0x00, 0x00]); // null terminator
}

/// Decode a null-terminated **little-endian** UTF-16 string from a
/// [`BinaryReader`](crate::reader::BinaryReader) of *any* byte order.
///
/// Reads raw byte pairs and assembles them as LE `u16` code units until a
/// `0x0000` terminator is encountered (or the reader runs out of data).
/// Invalid surrogate pairs are replaced with U+FFFD.
///
/// This is the decoding counterpart to [`encode_le_utf16_str`].
pub fn decode_le_utf16_str<'a, E: ByteOrder>(
    r: &mut crate::reader::BinaryReader<'a, E>,
) -> Result<String, DecodeError> {
    let mut units = Vec::new();
    loop {
        if r.remaining_len() < 2 {
            break;
        }
        let raw = r.read_slice(2)?;
        let unit = u16::from_le_bytes([raw[0], raw[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    Ok(String::from_utf16_lossy(&units))
}

/// Decode a **length-prefixed** little-endian UTF-16 string from a reader
/// of *any* byte order.
///
/// Reads exactly `byte_len` raw bytes and interprets them as LE UTF-16
/// code units.  Unlike [`decode_le_utf16_str`] this does **not** look for
/// a null terminator — the length is determined entirely by `byte_len`.
///
/// Invalid surrogate pairs are replaced with U+FFFD.
pub fn decode_le_utf16_bytes<'a, E: ByteOrder>(
    r: &mut crate::reader::BinaryReader<'a, E>,
    byte_len: usize,
) -> Result<String, DecodeError> {
    let raw = r.read_slice(byte_len)?;
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

/// Encode `s` as little-endian UTF-16 **without** a null terminator
/// into a [`BinaryWriter`] of *any* byte order.
///
/// Returns the number of bytes written (always `2 × code_units`).
///
/// This is the encoding counterpart to [`decode_le_utf16_bytes`].
pub fn encode_le_utf16_bytes<E: ByteOrder>(s: &str, w: &mut crate::writer::BinaryWriter<E>) -> usize {
    let mut byte_count = 0usize;
    for unit in s.encode_utf16() {
        w.put_slice(&unit.to_le_bytes());
        byte_count += 2;
    }
    byte_count
}
