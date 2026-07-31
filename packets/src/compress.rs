//! Minimal zlib compress / decompress helpers.
//!
//! Thin wrappers around [`miniz_oxide`] used by packets that carry
//! zlib-compressed payloads (0xD8 `SendCustomHouse`, 0xDD
//! `SendCompressedGump`, etc.).

use u_io::DecodeError;

/// Default zlib compression level (matches the "default" quality of most
/// UO server implementations).
const DEFAULT_LEVEL: u8 = 6;

/// Decompress a zlib-compressed buffer.
///
/// Returns the decompressed bytes or a [`DecodeError`] if the data is
/// malformed.
pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    miniz_oxide::inflate::decompress_to_vec_zlib(data)
        .map_err(|e| DecodeError::Other(format!("zlib decompress failed: {e:?}")))
}

/// Compress a buffer with zlib (default compression level).
///
/// The returned bytes include the standard 2-byte zlib header and trailing
/// Adler-32 checksum, matching what the UO client expects.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, DEFAULT_LEVEL)
}
