//! Middleware that logs raw byte traffic to the [`crate::session_registry::SessionRegistry`].
//!
//! [`LoggingStream`] wraps any [`ProtocolStream`] and records every
//! `read` / `write` / `read_seed` / `write_seed` to the registry with
//! the appropriate stage and direction:
//!
//! - reads      → `RawStage::RawRead`    / `RawStage::Decrypted`  with `read_dir`
//! - writes     → `RawStage::RawWrite`   / `RawStage::PreEncrypt` with `write_dir`
//! - read_seed  → `RawStage::RawRead`    with `read_dir`
//! - write_seed → `RawStage::RawWrite`   with `write_dir`
//!
//! # Usage
//!
//! Insert at different levels in the stream stack for different views:
//!
//! ```text
//! TcpByteStream
//!   └─ LoggingStream (read_stage=RawRead, write_stage=RawWrite)     ← sees ciphertext
//!        └─ CryptoStream (decrypt / encrypt)
//!             └─ LoggingStream (read_stage=Decrypted, write_stage=PreEncrypt) ← sees plaintext
//!                  └─ CodecTransport (packet framing)
//! ```

use bytes::BytesMut;
use protocol::transport::protocol_stream::ProtocolStream;

use crate::session_registry::{RawEntry, RawStage};
use crate::session_registry::{SessionId, SharedRegistry};

/// Passthrough [`ProtocolStream`] middleware that logs I/O to the [`crate::session_registry::SessionRegistry`].
pub struct LoggingStream<S: ProtocolStream> {
    inner: S,
    session_id: SessionId,
    registry: SharedRegistry,
    /// Direction label for bytes read from the peer (incoming).
    read_dir: &'static str,
    /// Direction label for bytes written to the peer (outgoing).
    write_dir: &'static str,
    /// Stage label for logged reads (e.g. `RawRead` or `Decrypted`).
    read_stage: RawStage,
    /// Stage label for logged writes (e.g. `RawWrite` or `PreEncrypt`).
    write_stage: RawStage,
}

impl<S: ProtocolStream> std::fmt::Debug for LoggingStream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingStream")
            .field("session_id", &self.session_id)
            .field("read_dir", &self.read_dir)
            .field("write_dir", &self.write_dir)
            .field("read_stage", &self.read_stage)
            .field("write_stage", &self.write_stage)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<S: ProtocolStream> LoggingStream<S> {
    /// Create a new logging stream.
    ///
    /// - `read_dir`    — direction label for reads (`"C→S"` or `"S→C"`)
    /// - `write_dir`   — direction label for writes (`"C→S"` or `"S→C"`)
    /// - `read_stage`  — stage for read logging (e.g. `RawRead` or `Decrypted`)
    /// - `write_stage` — stage for write logging (e.g. `RawWrite` or `PreEncrypt`)
    pub fn new(
        inner: S,
        session_id: SessionId,
        registry: SharedRegistry,
        read_dir: &'static str,
        write_dir: &'static str,
        read_stage: RawStage,
        write_stage: RawStage,
    ) -> Self {
        Self { inner, session_id, registry, read_dir, write_dir, read_stage, write_stage }
    }
}

impl<S: ProtocolStream> ProtocolStream for LoggingStream<S> {
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf).await?;
        if n > 0 {
            self.registry.push_raw(
                self.session_id,
                RawEntry::new(self.read_stage, self.read_dir, &buf[..n]),
            );
        }
        Ok(n)
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.inner.read_exact(buf).await?;
        if !buf.is_empty() {
            self.registry.push_raw(
                self.session_id,
                RawEntry::new(self.read_stage, self.read_dir, buf),
            );
        }
        Ok(())
    }

    async fn read_into(&mut self, dst: &mut BytesMut) -> std::io::Result<usize> {
        let before = dst.len();
        let n = self.inner.read_into(dst).await?;
        if n > 0 {
            self.registry.push_raw(
                self.session_id,
                RawEntry::new(self.read_stage, self.read_dir, &dst[before..]),
            );
        }
        Ok(n)
    }

    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        if !data.is_empty() {
            self.registry.push_raw(
                self.session_id,
                RawEntry::new(self.write_stage, self.write_dir, data),
            );
        }
        self.inner.write_all(data).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().await
    }

    async fn shutdown(&mut self) {
        self.inner.shutdown().await;
    }

    async fn read_seed(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.inner.read_seed(buf).await?;
        if !buf.is_empty() {
            self.registry.push_raw(
                self.session_id,
                RawEntry::new(self.read_stage, self.read_dir, buf),
            );
        }
        Ok(())
    }

    async fn write_seed(&mut self, data: &[u8]) -> std::io::Result<()> {
        if !data.is_empty() {
            self.registry.push_raw(
                self.session_id,
                RawEntry::new(self.write_stage, self.write_dir, data),
            );
        }
        self.inner.write_seed(data).await
    }
}
