//! Wire-format wrapper types with `Decode<E>` / `Encode<E>` implementations.

use std::net::Ipv4Addr;
use std::ops::Deref;

use crate::endian::ByteOrder;
use crate::error::DecodeError;
use crate::reader::ReadPrimitives;
use crate::traits::{Decode, Encode};
use crate::writer::BinaryWriter;

/// Implement `new`, `into_inner`, `Deref<Vec<T>>` for a `Vec<T>` newtype.
macro_rules! impl_vec_newtype {
    ($Name:ident) => {
        impl<T> $Name<T> {
            pub fn new(items: Vec<T>) -> Self { Self(items) }
            pub fn into_inner(self) -> Vec<T> { self.0 }
        }
        impl<T> Deref for $Name<T> {
            type Target = Vec<T>;
            fn deref(&self) -> &Vec<T> { &self.0 }
        }
    };
}

// ── Ipv4Addr ───────────────────────────────────────────────────────────────

impl<E: ByteOrder> Decode<E> for Ipv4Addr {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let mut o = [0u8; 4];
        reader.read_bytes(&mut o)?;
        Ok(Ipv4Addr::from(o))
    }
}

impl<E: ByteOrder> Encode<E> for Ipv4Addr {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_slice(&self.octets());
    }
}

// ── Pad<N> ─────────────────────────────────────────────────────────────────

/// Skips `N` bytes on read, writes `N` zero bytes on write.
///
/// Used for padding, unknown fields, and reserved space in wire formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad<const N: usize>;

impl<const N: usize, E: ByteOrder> Decode<E> for Pad<N> {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        reader.skip(N)?;
        Ok(Pad)
    }
}

impl<const N: usize, E: ByteOrder> Encode<E> for Pad<N> {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_bytes(0, N);
    }
}

// ── ListU16<T> ─────────────────────────────────────────────────────────────

/// A list of items prefixed by a `u16` count on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ListU16<T>(pub Vec<T>);

impl_vec_newtype!(ListU16);

impl<T: Decode<E>, E: ByteOrder> Decode<E> for ListU16<T> {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let count = reader.read_u16()? as usize;
        let mut items = Vec::with_capacity(count.min(128));
        for _ in 0..count {
            items.push(T::decode(reader)?);
        }
        Ok(Self(items))
    }
}

impl<T: Encode<E>, E: ByteOrder> Encode<E> for ListU16<T> {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        debug_assert!(self.0.len() <= u16::MAX as usize, "ListU16 overflow: {} items", self.0.len());
        writer.put_u16(self.0.len() as u16);
        for item in &self.0 {
            item.encode(writer);
        }
    }
}

// ── ListU8<T> ──────────────────────────────────────────────────────────────

/// A list of items prefixed by a `u8` count on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ListU8<T>(pub Vec<T>);

impl_vec_newtype!(ListU8);

impl<T: Decode<E>, E: ByteOrder> Decode<E> for ListU8<T> {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let count = reader.read_u8()? as usize;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(T::decode(reader)?);
        }
        Ok(Self(items))
    }
}

impl<T: Encode<E>, E: ByteOrder> Encode<E> for ListU8<T> {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        debug_assert!(self.0.len() <= u8::MAX as usize, "ListU8 overflow: {} items", self.0.len());
        writer.put_u8(self.0.len() as u8);
        for item in &self.0 {
            item.encode(writer);
        }
    }
}
