//! Stream-based binary reader parameterized by byte order.
//!
//! Unlike [`BinaryReader`](crate::BinaryReader) which operates on an
//! in-memory `&[u8]` slice, `StreamReader` reads from any `std::io::Read`
//! source (files, `BufReader`, network sockets, etc.).

use std::io::{Read, Seek, SeekFrom};
use std::marker::PhantomData;

use crate::endian::ByteOrder;
use crate::error::DecodeError;
use crate::reader::ReadPrimitives;

/// Binary reader over a [`Read`] stream.
///
/// The type parameter `E` selects the byte order used for multibyte reads
/// ([`BE`](crate::BE) or [`LE`](crate::LE)).
pub struct StreamReader<R: Read, E: ByteOrder> {
    inner: R,
    _endian: PhantomData<E>,
}

impl<R: Read, E: ByteOrder> StreamReader<R, E> {
    /// Wrap a `Read` source.
    pub fn new(inner: R) -> Self {
        Self { inner, _endian: PhantomData }
    }

    /// Consume the reader and return the underlying stream.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Get a reference to the underlying stream.
    pub fn inner(&self) -> &R {
        &self.inner
    }

    /// Get a mutable reference to the underlying stream.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Read exactly `n` bytes into `buf`.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), DecodeError> {
        self.inner.read_exact(buf)?;
        Ok(())
    }
}

impl<R: Read + Seek, E: ByteOrder> StreamReader<R, E> {
    /// Seek to an absolute byte position in the underlying stream.
    pub fn seek_to(&mut self, offset: u64) -> Result<(), DecodeError> {
        self.inner.seek(SeekFrom::Start(offset))?;
        Ok(())
    }
}

impl<R: Read, E: ByteOrder> ReadPrimitives<E> for StreamReader<R, E> {
    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let mut buf = [0u8; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    fn read_i8(&mut self) -> Result<i8, DecodeError> {
        self.read_u8().map(|v| v as i8)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(E::read_u16(&buf))
    }

    fn read_i16(&mut self) -> Result<i16, DecodeError> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(E::read_i16(&buf))
    }

    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(E::read_u32(&buf))
    }

    fn read_i32(&mut self) -> Result<i32, DecodeError> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(E::read_i32(&buf))
    }

    fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(E::read_u64(&buf))
    }

    fn skip(&mut self, n: usize) -> Result<(), DecodeError> {
        // Read and discard `n` bytes in chunks
        let mut remaining = n;
        let mut buf = [0u8; 256];
        while remaining > 0 {
            let chunk = remaining.min(buf.len());
            self.read_exact(&mut buf[..chunk])?;
            remaining -= chunk;
        }
        Ok(())
    }

    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<(), DecodeError> {
        self.read_exact(buf)
    }

    fn remaining(&self) -> Option<usize> {
        None
    }
}
