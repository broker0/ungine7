//! Binary writer parameterized by byte order.

use std::marker::PhantomData;

use bytes::{BufMut, Bytes, BytesMut};

use crate::endian::ByteOrder;

/// Writer for building binary data (thin wrapper around `BytesMut`).
///
/// The type parameter `E` selects the byte order used for multibyte writes
/// ([`BE`](crate::BE) or [`LE`](crate::LE)).
pub struct BinaryWriter<E: ByteOrder> {
    buf: BytesMut,
    _endian: PhantomData<E>,
}

impl<E: ByteOrder> Default for BinaryWriter<E> {
    fn default() -> Self {
        Self { buf: BytesMut::new(), _endian: PhantomData }
    }
}

impl<E: ByteOrder> BinaryWriter<E> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self { buf: BytesMut::with_capacity(capacity), _endian: PhantomData }
    }

    /// Consume the writer and return the accumulated bytes.
    pub fn finish(self) -> Bytes {
        self.buf.freeze()
    }

    /// Current number of bytes written.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the writer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Overwrite an u16 at a given offset (for length back-patching).
    /// Written in the byte order selected by `E`.
    pub fn set_u16_at(&mut self, offset: usize, value: u16) {
        self.buf[offset..offset + 2].copy_from_slice(&E::write_u16(value));
    }

    // ── Single-byte (endianness-independent) ───────────────────────────

    pub fn put_u8(&mut self, value: u8) {
        self.buf.put_u8(value);
    }

    pub fn put_i8(&mut self, value: i8) {
        self.buf.put_i8(value);
    }

    // ── Multi-byte (endianness-aware) ──────────────────────────────────

    pub fn put_u16(&mut self, value: u16) {
        self.buf.put_slice(&E::write_u16(value));
    }

    pub fn put_i16(&mut self, value: i16) {
        self.buf.put_slice(&E::write_i16(value));
    }

    pub fn put_u32(&mut self, value: u32) {
        self.buf.put_slice(&E::write_u32(value));
    }

    pub fn put_i32(&mut self, value: i32) {
        self.buf.put_slice(&E::write_i32(value));
    }

    pub fn put_u64(&mut self, value: u64) {
        self.buf.put_slice(&E::write_u64(value));
    }

    // ── Raw bytes ──────────────────────────────────────────────────────

    pub fn put_slice(&mut self, slice: &[u8]) {
        self.buf.put_slice(slice);
    }

    /// Write `len` copies of `value`.
    pub fn put_bytes(&mut self, value: u8, len: usize) {
        self.buf.put_bytes(value, len);
    }
}
