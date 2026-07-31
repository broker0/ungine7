//! Core serialization traits parameterized by byte order.

use crate::endian::ByteOrder;
use crate::error::DecodeError;
use crate::reader::ReadPrimitives;
use crate::writer::BinaryWriter;

/// Decode a value from any [`ReadPrimitives<E>`] source using byte order `E`.
pub trait Decode<E: ByteOrder>: Sized {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError>;
}

/// Encode a value into a [`BinaryWriter`] using byte order `E`.
pub trait Encode<E: ByteOrder> {
    fn encode(&self, writer: &mut BinaryWriter<E>);
}

// ── Primitive implementations (generic over byte order) ────────────────────

// u8 / i8 — endianness-independent, but generic for consistency.

impl<E: ByteOrder> Decode<E> for u8 {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        reader.read_u8()
    }
}

impl<E: ByteOrder> Encode<E> for u8 {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_u8(*self);
    }
}

impl<E: ByteOrder> Decode<E> for i8 {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        reader.read_i8()
    }
}

impl<E: ByteOrder> Encode<E> for i8 {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_i8(*self);
    }
}

// Multibyte — delegates to reader which use E via ReadPrimitives.

macro_rules! impl_multibyte {
    ($t:ty, $read:ident, $write:ident) => {
        impl<E: ByteOrder> Decode<E> for $t {
            fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
                reader.$read()
            }
        }
        impl<E: ByteOrder> Encode<E> for $t {
            fn encode(&self, writer: &mut BinaryWriter<E>) {
                writer.$write(*self);
            }
        }
    };
}

impl_multibyte!(u16, read_u16, put_u16);
impl_multibyte!(i16, read_i16, put_i16);
impl_multibyte!(u32, read_u32, put_u32);
impl_multibyte!(i32, read_i32, put_i32);
impl_multibyte!(u64, read_u64, put_u64);
