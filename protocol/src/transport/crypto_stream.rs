//! Encryption/decryption middleware for the transport pipeline.
//!
//! [`CryptoStream`] wraps any [`ProtocolStream`] and applies encryption on
//! writes and decryption on reads. Seed data (the first bytes of every UO
//! connection, always plaintext) is passed through without transformation
//! via the [`read_seed`](ProtocolStream::read_seed) /
//! [`write_seed`](ProtocolStream::write_seed) methods.
//!
//! # Architecture
//!
//! ```text
//! TcpByteStream (ProtocolStream — raw TCP bytes)
//!   └─ CryptoStream (ProtocolStream — plaintext bytes)
//!        ├─ read()      → inner.read() then decrypt
//!        ├─ read_into() → inner.read() then decrypt directly into dst (zero-copy)
//!        ├─ write_all() → encrypt then inner.write_all()
//!        ├─ read_seed() → inner.read_seed() (bypass cipher)
//!        └─ write_seed()→ inner.write_seed() (bypass cipher)
//! ```
//!
//! # Observability
//!
//! Because `CryptoStream` implements `ProtocolStream`, you can wrap it
//! in any `ProtocolStream` middleware to observe plaintext data:
//!
//! ```text
//! TcpByteStream
//!   └─ LoggingStream (sees ciphertext — RawRead / RawWrite)
//!        └─ CryptoStream (decrypt / encrypt)
//!             └─ LoggingStream (sees plaintext — Decrypted / PreEncrypt)
//!                  └─ CodecTransport (packet framing)
//! ```

use std::fmt::Debug;
use bytes::BytesMut;
use log::{trace, warn};

use crate::codec::encryption::{Decryptor, Encryptor};
use crate::logs;
use super::protocol_stream::ProtocolStream;


/// Encrypting/decrypting middleware that implements [`ProtocolStream`].
///
/// Wraps an inner `ProtocolStream` and applies a stateful
/// [`Encryptor`]/[`Decryptor`] pair to all data flowing through.
/// Seed data is forwarded to the inner stream without transformation.
///
/// # Zero-copy reads
///
/// [`read_into()`](ProtocolStream::read_into) decrypts directly into the
/// caller's `BytesMut` buffer when the internal plaintext buffer is empty,
/// avoiding an intermediate copy. This is the path used by
/// [`CodecTransport`](super::codec_transport::CodecTransport).
pub struct CryptoStream<S: ProtocolStream> {
    stream: S,
    encryptor: Box<dyn Encryptor>,
    decryptor: Box<dyn Decryptor>,
    /// Temporary buffer for reading ciphertext from the inner stream.
    raw_buf: Vec<u8>,
    /// Accumulated plaintext from decryption that hasn't been consumed yet.
    /// Used when `read()` or `read_into()` cannot deliver all decrypted
    /// bytes in a single call.
    plaintext_buf: BytesMut,
    /// Temporary buffer for encrypted output before writing to the inner stream.
    encrypt_buf: BytesMut,
}

impl<S: ProtocolStream> Debug for CryptoStream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoStream")
            .field("stream", &self.stream)
            .field("encryptor", &self.encryptor)
            .field("decryptor", &self.decryptor)
            .field("plaintext_buf_len", &self.plaintext_buf.len())
            .finish()
    }
}

impl<S: ProtocolStream> CryptoStream<S> {
    /// Create a new `CryptoStream` over the given inner stream.
    pub fn new(
        stream: S,
        encryptor: Box<dyn Encryptor>,
        decryptor: Box<dyn Decryptor>,
    ) -> Self {
        Self {
            stream,
            encryptor,
            decryptor,
            raw_buf: vec![0u8; 4096],
            plaintext_buf: BytesMut::new(),
            encrypt_buf: BytesMut::new(),
        }
    }

    /// Drain any buffered plaintext into `buf`. Returns bytes copied.
    fn drain_plaintext(&mut self, buf: &mut [u8]) -> usize {
        if self.plaintext_buf.is_empty() {
            return 0;
        }
        let n = buf.len().min(self.plaintext_buf.len());
        buf[..n].copy_from_slice(&self.plaintext_buf[..n]);
        let _ = self.plaintext_buf.split_to(n);
        n
    }

    /// Drain any buffered plaintext into a `BytesMut`. Returns bytes moved.
    fn drain_plaintext_into(&mut self, dst: &mut BytesMut) -> usize {
        if self.plaintext_buf.is_empty() {
            return 0;
        }
        let n = self.plaintext_buf.len();
        dst.extend_from_slice(&self.plaintext_buf);
        self.plaintext_buf.clear();
        n
    }
}

impl<S: ProtocolStream> ProtocolStream for CryptoStream<S> {
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // First, drain any leftover plaintext from a previous read
        let drained = self.drain_plaintext(buf);
        if drained > 0 {
            return Ok(drained);
        }

        // Read ciphertext from the inner stream
        let n = self.stream.read(&mut self.raw_buf).await?;
        if n == 0 {
            return Ok(0);
        }

        trace!(target: logs::TRANSPORT_RAW, "raw read {} bytes: {:02X?}", n, &self.raw_buf[..n]);

        // Decrypt into plaintext_buf
        self.decryptor.decrypt(&self.raw_buf[..n], &mut self.plaintext_buf);

        trace!(target: logs::TRANSPORT_RAW, "after decrypt: {} bytes in buffer {:02X?}", self.plaintext_buf.len(), &self.plaintext_buf[..]);

        // Drain what fits into buf
        Ok(self.drain_plaintext(buf))
    }

    async fn read_into(&mut self, dst: &mut BytesMut) -> std::io::Result<usize> {
        // First, drain any leftover plaintext
        let drained = self.drain_plaintext_into(dst);
        if drained > 0 {
            return Ok(drained);
        }

        // Read ciphertext from the inner stream
        let n = self.stream.read(&mut self.raw_buf).await?;
        if n == 0 {
            return Ok(0);
        }

        trace!(target: logs::TRANSPORT_RAW, "raw read {} bytes: {:02X?}", n, &self.raw_buf[..n]);

        // Zero-copy path: decrypt directly into the caller's buffer
        let before = dst.len();
        self.decryptor.decrypt(&self.raw_buf[..n], dst);
        let decrypted = dst.len() - before;

        trace!(target: logs::TRANSPORT_RAW, "after decrypt: {} bytes {:02X?}", decrypted, &dst[before..]);

        Ok(decrypted)
    }

    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.encrypt_buf.clear();
        self.encryptor.encrypt(data, &mut self.encrypt_buf);
        trace!(target: logs::TRANSPORT_RAW, "after encrypt: {} bytes {:02X?}", self.encrypt_buf.len(), &self.encrypt_buf[..]);
        self.stream.write_all(&self.encrypt_buf).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush().await
    }

    async fn shutdown(&mut self) {
        if !self.plaintext_buf.is_empty() {
            let len = self.plaintext_buf.len();
            let preview = &self.plaintext_buf[..len.min(128)];
            warn!(target: logs::TRANSPORT_RAW,
                "closing CryptoStream with {len} unconsumed plaintext bytes: {preview:02X?}");
        }
        self.stream.shutdown().await;
    }

    /// Read seed bytes, bypassing the cipher.
    ///
    /// Forwards directly to the inner stream's `read_seed`, ensuring
    /// that seed data is never passed through the decryptor.
    async fn read_seed(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.stream.read_seed(buf).await
    }

    /// Write seed bytes, bypassing the cipher.
    ///
    /// Forwards directly to the inner stream's `write_seed`, ensuring
    /// that seed data is never passed through the encryptor.
    async fn write_seed(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_seed(data).await
    }
}
