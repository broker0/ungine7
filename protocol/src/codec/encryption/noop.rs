use super::{Decryptor, Encryptor};
use bytes::BytesMut;

/// Transparent encryptor — passes data through unchanged.
/// Used for testing, unencrypted connections, and seed packets.
#[derive(Debug, Clone)]
pub struct NoopEncryptor;

impl Encryptor for NoopEncryptor {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.extend_from_slice(src);
    }
}

/// Transparent decryptor — passes data through unchanged.
#[derive(Debug, Clone)]
pub struct NoopDecryptor;

impl Decryptor for NoopDecryptor {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.extend_from_slice(src);
    }
}
