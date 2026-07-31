//! Byte order (endianness) abstraction.
//!
//! [`BE`] and [`LE`] are zero-sized marker types that implement [`ByteOrder`].
//! They are used as type parameters for [`BinaryReader`](crate::BinaryReader)
//! and [`BinaryWriter`](crate::BinaryWriter) to select the wire byte order.

/// Trait abstracting byte-order conversions for multibyte integers.
///
/// Implemented by [`BE`] (big-endian / network order) and [`LE`] (little-endian).
pub trait ByteOrder: sealed::Sealed + Send + Sync + 'static {
    fn read_u16(buf: &[u8; 2]) -> u16;
    fn read_i16(buf: &[u8; 2]) -> i16;
    fn read_u32(buf: &[u8; 4]) -> u32;
    fn read_i32(buf: &[u8; 4]) -> i32;
    fn read_u64(buf: &[u8; 8]) -> u64;

    fn write_u16(v: u16) -> [u8; 2];
    fn write_i16(v: i16) -> [u8; 2];
    fn write_u32(v: u32) -> [u8; 4];
    fn write_i32(v: i32) -> [u8; 4];
    fn write_u64(v: u64) -> [u8; 8];
}

/// Big-endian (network) byte order.
pub enum BE {}

/// Little-endian byte order.
pub enum LE {}

impl ByteOrder for BE {
    #[inline] fn read_u16(buf: &[u8; 2]) -> u16 { u16::from_be_bytes(*buf) }
    #[inline] fn read_i16(buf: &[u8; 2]) -> i16 { i16::from_be_bytes(*buf) }
    #[inline] fn read_u32(buf: &[u8; 4]) -> u32 { u32::from_be_bytes(*buf) }
    #[inline] fn read_i32(buf: &[u8; 4]) -> i32 { i32::from_be_bytes(*buf) }
    #[inline] fn read_u64(buf: &[u8; 8]) -> u64 { u64::from_be_bytes(*buf) }

    #[inline] fn write_u16(v: u16) -> [u8; 2] { v.to_be_bytes() }
    #[inline] fn write_i16(v: i16) -> [u8; 2] { v.to_be_bytes() }
    #[inline] fn write_u32(v: u32) -> [u8; 4] { v.to_be_bytes() }
    #[inline] fn write_i32(v: i32) -> [u8; 4] { v.to_be_bytes() }
    #[inline] fn write_u64(v: u64) -> [u8; 8] { v.to_be_bytes() }
}

impl ByteOrder for LE {
    #[inline] fn read_u16(buf: &[u8; 2]) -> u16 { u16::from_le_bytes(*buf) }
    #[inline] fn read_i16(buf: &[u8; 2]) -> i16 { i16::from_le_bytes(*buf) }
    #[inline] fn read_u32(buf: &[u8; 4]) -> u32 { u32::from_le_bytes(*buf) }
    #[inline] fn read_i32(buf: &[u8; 4]) -> i32 { i32::from_le_bytes(*buf) }
    #[inline] fn read_u64(buf: &[u8; 8]) -> u64 { u64::from_le_bytes(*buf) }

    #[inline] fn write_u16(v: u16) -> [u8; 2] { v.to_le_bytes() }
    #[inline] fn write_i16(v: i16) -> [u8; 2] { v.to_le_bytes() }
    #[inline] fn write_u32(v: u32) -> [u8; 4] { v.to_le_bytes() }
    #[inline] fn write_i32(v: i32) -> [u8; 4] { v.to_le_bytes() }
    #[inline] fn write_u64(v: u64) -> [u8; 8] { v.to_le_bytes() }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::BE {}
    impl Sealed for super::LE {}
}
