//! Login encryption/decryption.
//!
//! UO login protocol uses a simple XOR-based cipher derived from
//! the client version numbers and a seed value. The encryption is
//! asymmetric in the sense that client→server and server→client
//! directions use different key derivations.
//!
//! # Key derivation
//!
//! The client computes encryption keys from `(seed, major, minor, revision)`.
//! The server needs to know the client version to decrypt incoming traffic,
//! but encrypts outgoing traffic using only the seed.
//!
//! # Architecture
//!
//! ```text
//!   Client                           Server
//!   ──────                           ──────
//!   ClientLoginEncoder    ────►  ServerLoginDecoder
//!     (seed, ver)                  (seed, ver)
//!
//!   ClientLoginDecoder    ◄────  ServerLoginEncoder
//!     (seed)                       (seed)
//! ```

use super::{Decryptor, Encryptor};
use crate::encoding::encoders::{
    ClientLoginDecoder, ClientLoginEncoder, ServerLoginDecoder, ServerLoginEncoder,
};
use crate::protocol::LoginProtocolInfo;

/// Server-side encryptor/decryptor pair for login phase.
///
/// When `info.encrypted` is true, uses the XOR login cipher.
/// Otherwise, uses no-op passthrough.
pub fn server_pair(info: &LoginProtocolInfo) -> (Box<dyn Encryptor>, Box<dyn Decryptor>) {
    if info.encrypted {
        (server_encryptor(info), server_decryptor(info))
    } else {
        (
            super::no_encryption_encryptor(),
            super::no_encryption_decryptor(),
        )
    }
}

/// Client-side encryptor/decryptor pair for login phase.
pub fn client_pair(info: &LoginProtocolInfo) -> (Box<dyn Encryptor>, Box<dyn Decryptor>) {
    if info.encrypted {
        (client_encryptor(info), client_decryptor(info))
    } else {
        (
            super::no_encryption_encryptor(),
            super::no_encryption_decryptor(),
        )
    }
}

// ---- Internal factory functions (used by server_pair/client_pair) ----

pub(crate) fn server_encryptor(info: &LoginProtocolInfo) -> Box<dyn Encryptor> {
    Box::new(ServerLoginEncoder::new(info.seed))
}

pub(crate) fn server_decryptor(info: &LoginProtocolInfo) -> Box<dyn Decryptor> {
    let v = info.client_version;
    Box::new(ServerLoginDecoder::new(info.seed, v))
}

pub(crate) fn client_encryptor(info: &LoginProtocolInfo) -> Box<dyn Encryptor> {
    let v = info.client_version;
    Box::new(ClientLoginEncoder::new(info.seed, v))
}

pub(crate) fn client_decryptor(info: &LoginProtocolInfo) -> Box<dyn Decryptor> {
    Box::new(ClientLoginDecoder::new(info.seed))
}
