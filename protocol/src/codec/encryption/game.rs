//! Game encryption/decryption.
//!
//! After the login phase, the client reconnects to the game server.
//! Game traffic uses a different encryption scheme based on the seed
//! value and client version.
//!
//! # Architecture
//!
//! ```text
//!   Client                           Server
//!   ──────                           ──────
//!   ClientGameEncoder     ────►  ServerGameDecoder
//!     (seed)                       (seed)
//!
//!   ClientGameDecoder     ◄────  ServerGameEncoder
//!     (seed)                       (seed)
//! ```
//!
//! # Encryption modes by client version
//!
//! | Version       | C→S encryption          | S→C encryption       |
//! |---------------|-------------------------|----------------------|
//! | `< 2.0.3`     | Blowfish                | Huffman              |
//! | `= 2.0.3`     | Blowfish + Twofish      | Huffman              |
//! | `> 2.0.3`     | Twofish                 | Huffman + MD5        |

use u_core::ProtocolVersion;
use super::{Decryptor, Encryptor};
use crate::encoding::encoders::{
    ClientGameBlowfishEncoder, ClientGameBlowfishTwofishEncoder, ClientGameDecoder,
    ClientGameEncoder, ClientGameHuffmanDecoder, ServerGameBlowfishDecoder,
    ServerGameBlowfishTwofishDecoder, ServerGameDecoder, ServerGameEncoder,
    ServerGameHuffmanEncoder,
};
use crate::protocol::GameProtocolInfo;

/// Server-side encryptor/decryptor pair for game phase.
///
/// When `info.encrypted` is true, uses version-appropriate ciphers.
/// When false, uses Huffman for S→C (matching real UO server behavior)
/// and no-op for C→S.
pub fn server_pair(info: &GameProtocolInfo) -> (Box<dyn Encryptor>, Box<dyn Decryptor>) {
    if info.encrypted {
        (server_encryptor(info), server_decryptor(info))
    } else {
        (
            Box::new(ServerGameHuffmanEncoder::new(info.seed)),
            super::no_encryption_decryptor(),
        )
    }
}

/// Client-side encryptor/decryptor pair for game phase.
///
/// When `info.encrypted` is true, uses version-appropriate ciphers.
/// When false, uses no-op for C→S and Huffman for S→C decompression.
pub fn client_pair(info: &GameProtocolInfo) -> (Box<dyn Encryptor>, Box<dyn Decryptor>) {
    if info.encrypted {
        (client_encryptor(info), client_decryptor(info))
    } else {
        (
            super::no_encryption_encryptor(),
            Box::new(ClientGameHuffmanDecoder::new(info.seed)),
        )
    }
}

/// The boundary version between Blowfish and Twofish encryption modes.
const BLOWFISH_TWOFISH: ProtocolVersion = ProtocolVersion::BLOWFISH_TWOFISH_CLIENT;

// ---- Internal factory functions (used by server_pair/client_pair) ----

pub(crate) fn server_encryptor(info: &GameProtocolInfo) -> Box<dyn Encryptor> {
    if info.client_version <= BLOWFISH_TWOFISH {
        Box::new(ServerGameHuffmanEncoder::new(info.seed))
    } else {
        Box::new(ServerGameEncoder::new(info.seed))
    }
}

pub(crate) fn server_decryptor(info: &GameProtocolInfo) -> Box<dyn Decryptor> {
    if info.client_version < BLOWFISH_TWOFISH {
        Box::new(ServerGameBlowfishDecoder::new(info.seed))
    } else if info.client_version == BLOWFISH_TWOFISH {
        Box::new(ServerGameBlowfishTwofishDecoder::new(info.seed))
    } else {
        Box::new(ServerGameDecoder::new(info.seed))
    }
}

pub(crate) fn client_encryptor(info: &GameProtocolInfo) -> Box<dyn Encryptor> {
    if info.client_version < BLOWFISH_TWOFISH {
        Box::new(ClientGameBlowfishEncoder::new(info.seed))
    } else if info.client_version == BLOWFISH_TWOFISH {
        Box::new(ClientGameBlowfishTwofishEncoder::new(info.seed))
    } else {
        Box::new(ClientGameEncoder::new(info.seed))
    }
}

pub(crate) fn client_decryptor(info: &GameProtocolInfo) -> Box<dyn Decryptor> {
    if info.client_version <= BLOWFISH_TWOFISH {
        Box::new(ClientGameHuffmanDecoder::new(info.seed))
    } else {
        Box::new(ClientGameDecoder::new(info.seed))
    }
}
