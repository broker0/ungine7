//! Cursor-based binary reader parameterized by byte order.

use std::marker::PhantomData;

use crate::endian::ByteOrder;
use crate::error::DecodeError;

// ── ReadPrimitives trait ───────────────────────────────────────────────────

/// Common interface for reading binary primitives with a given byte order.
///
/// Implemented by [`BinaryReader`] (in-memory slice) and
/// [`StreamReader`](crate::StreamReader) (any `std::io::Read`).
pub trait ReadPrimitives<E: ByteOrder> {
    fn read_u8(&mut self) -> Result<u8, DecodeError>;
    fn read_i8(&mut self) -> Result<i8, DecodeError>;
    fn read_u16(&mut self) -> Result<u16, DecodeError>;
    fn read_i16(&mut self) -> Result<i16, DecodeError>;
    fn read_u32(&mut self) -> Result<u32, DecodeError>;
    fn read_i32(&mut self) -> Result<i32, DecodeError>;
    fn read_u64(&mut self) -> Result<u64, DecodeError>;
    fn skip(&mut self, n: usize) -> Result<(), DecodeError>;

    /// Read exactly `buf.len()` bytes into the provided buffer.
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<(), DecodeError>;

    /// Returns the number of bytes remaining, if known.
    ///
    /// - [`BinaryReader`]: always returns `Some(n)` (known from slice length).
    /// - [`StreamReader`](crate::StreamReader): returns `None` (stream length
    ///   is generally unknown).
    fn remaining(&self) -> Option<usize>;
}

// ── BinaryReader ───────────────────────────────────────────────────────────

/// Cursor-based reader over a byte slice.
///
/// The type parameter `E` selects the byte order used for multibyte reads
/// ([`BE`](crate::BE) or [`LE`](crate::LE)).
pub struct BinaryReader<'a, E: ByteOrder> {
    data: &'a [u8],
    pos: usize,
    _endian: PhantomData<E>,
}

impl<'a, E: ByteOrder> BinaryReader<'a, E> {
    /// Create a new reader starting at position 0.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, _endian: PhantomData }
    }

    /// Current cursor position.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Number of bytes remaining after the cursor.
    pub fn remaining_len(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Read a raw byte slice of `n` bytes, advancing the cursor.
    ///
    /// This method is only available on `BinaryReader` (not on streams),
    /// because it returns a borrowed slice from the underlying buffer.
    pub fn read_slice(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining_len() < n {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }
}

impl<E: ByteOrder> ReadPrimitives<E> for BinaryReader<'_, E> {
    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        if self.remaining_len() < 1 {
            return Err(DecodeError::Truncated);
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_i8(&mut self) -> Result<i8, DecodeError> {
        self.read_u8().map(|v| v as i8)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let s = self.read_slice(2)?;
        Ok(E::read_u16(s.try_into().unwrap()))
    }

    fn read_i16(&mut self) -> Result<i16, DecodeError> {
        let s = self.read_slice(2)?;
        Ok(E::read_i16(s.try_into().unwrap()))
    }

    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let s = self.read_slice(4)?;
        Ok(E::read_u32(s.try_into().unwrap()))
    }

    fn read_i32(&mut self) -> Result<i32, DecodeError> {
        let s = self.read_slice(4)?;
        Ok(E::read_i32(s.try_into().unwrap()))
    }

    fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let s = self.read_slice(8)?;
        Ok(E::read_u64(s.try_into().unwrap()))
    }

    fn skip(&mut self, n: usize) -> Result<(), DecodeError> {
        if self.remaining_len() < n {
            return Err(DecodeError::Truncated);
        }
        self.pos += n;
        Ok(())
    }

    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<(), DecodeError> {
        let n = buf.len();
        if self.remaining_len() < n {
            return Err(DecodeError::Truncated);
        }
        buf.copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(())
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.remaining_len())
    }
}
