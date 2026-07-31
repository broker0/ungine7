use crate::codec::encryption::{Decryptor, Encryptor};
use crate::encoding::game::{BlowfishGameCrypt, MD5GameCrypt, TwofishGameCrypt};
use crate::encoding::huffman::{HuffmanCompress, HuffmanDecompress};
use crate::encoding::login::XorLoginCrypt;
use bytes::BytesMut;
use u_core::ProtocolVersion;
/**************************************************************************************************/
// login encoder/decoder structs
// client/server pair of coders
/// client login encoder - encode outgoing packets with XorLoginCrypt method
#[derive(Debug)]
pub struct ClientLoginEncoder {
    xor_login_crypt: XorLoginCrypt,
}

impl ClientLoginEncoder {
    pub fn new(seed: u32, version: ProtocolVersion) -> Self {
        Self {
            xor_login_crypt: XorLoginCrypt::new(seed, version),
        }
    }
}

impl Encryptor for ClientLoginEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.xor_login_crypt.encode(src, dst);
    }
}

/// server login decoder - decode incoming packets with XorLoginCrypt method
#[derive(Debug)]
pub struct ServerLoginDecoder {
    xor_login_crypt: XorLoginCrypt,
}

impl ServerLoginDecoder {
    pub fn new(seed: u32, version: ProtocolVersion) -> Self {
        Self {
            xor_login_crypt: XorLoginCrypt::new(seed, version),
        }
    }
}

impl Decryptor for ServerLoginDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.xor_login_crypt.encode(src, dst);
    }
}

// server/client pair of coders
/// server login encoder - no outgoing encoder, just copy incoming packets to outgoing buffer
#[derive(Debug)]
pub struct ServerLoginEncoder {}

impl ServerLoginEncoder {
    pub fn new(_seed: u32) -> Self {
        Self {}
    }
}

impl Encryptor for ServerLoginEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.extend_from_slice(src);
    }
}

/// client login decoder - no incoming decoder, just copy incoming packets to outgoing buffer
#[derive(Debug)]
pub struct ClientLoginDecoder {}

impl ClientLoginDecoder {
    pub fn new(_seed: u32) -> Self {
        Self {}
    }
}

impl Decryptor for ClientLoginDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.extend_from_slice(src);
    }
}

/**************************************************************************************************/
// game encoder/decoder structs
/// client game encoder - encode outgoing packets with TwofishGameCrypt method
#[derive(Debug)]
pub struct ClientGameEncoder {
    twofish_crypt: TwofishGameCrypt,
}

impl ClientGameEncoder {
    pub fn new(seed: u32) -> Self {
        Self {
            twofish_crypt: TwofishGameCrypt::new(seed),
        }
    }
}

impl Encryptor for ClientGameEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.twofish_crypt.crypt(src, dst);
    }
}

/// server game decoder - decode incoming packets with TwofishGameCrypt method
#[derive(Debug)]
pub struct ServerGameDecoder {
    twofish_crypt: TwofishGameCrypt,
}

impl ServerGameDecoder {
    pub fn new(seed: u32) -> Self {
        Self {
            twofish_crypt: TwofishGameCrypt::new(seed),
        }
    }
}

impl Decryptor for ServerGameDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.twofish_crypt.crypt(src, dst);
    }
}

/// server game encoder - encode outgoing packets with HuffmanCompress+MD5GameCrypt methods
#[derive(Debug)]
pub struct ServerGameEncoder {
    huffman_enc: HuffmanCompress,
    md5_enc: MD5GameCrypt,

    buffer: BytesMut,
}

impl ServerGameEncoder {
    pub fn new(seed: u32) -> Self {
        Self {
            huffman_enc: HuffmanCompress::new(),
            md5_enc: MD5GameCrypt::new(seed),

            buffer: BytesMut::with_capacity(8 * 1024),
        }
    }
}

impl Encryptor for ServerGameEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.buffer.clear();
        self.huffman_enc.compress(src, &mut self.buffer);
        self.md5_enc.crypt(&self.buffer, dst);
    }
}

/// client game decoder - decode incoming packets with MD5GameCrypt+HuffmanCompress methods
#[derive(Debug)]
pub struct ClientGameDecoder {
    huffman_dec: HuffmanDecompress,
    md5_dec: MD5GameCrypt,

    buffer: BytesMut,
}

impl ClientGameDecoder {
    pub fn new(seed: u32) -> Self {
        Self {
            huffman_dec: HuffmanDecompress::new(),
            md5_dec: MD5GameCrypt::new(seed),

            buffer: BytesMut::with_capacity(8 * 1024),
        }
    }
}

impl Decryptor for ClientGameDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.buffer.clear();
        self.md5_dec.crypt(src, &mut self.buffer);
        self.huffman_dec.decompress(&self.buffer, dst);
    }
}

/**************************************************************************************************/
// Blowfish-only game encoder/decoder (clients < 2.0.3)
//
// C→S: BlowfishGameCrypt encrypt
// S→C: Huffman only (no MD5)

/// Client game encoder for Blowfish-only mode (client version < 2.0.3).
///
/// Encrypts outgoing client→server traffic with Blowfish stream cipher.
#[derive(Debug)]
pub struct ClientGameBlowfishEncoder {
    blowfish: BlowfishGameCrypt,
}

impl ClientGameBlowfishEncoder {
    pub fn new(seed: u32) -> Self {
        Self {
            blowfish: BlowfishGameCrypt::new(seed),
        }
    }
}

impl Encryptor for ClientGameBlowfishEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.blowfish.encrypt(src, dst);
    }
}

/// Server game decoder for Blowfish-only mode (client version < 2.0.3).
///
/// Decrypts incoming client→server traffic with Blowfish stream cipher.
#[derive(Debug)]
pub struct ServerGameBlowfishDecoder {
    blowfish: BlowfishGameCrypt,
}

impl ServerGameBlowfishDecoder {
    pub fn new(seed: u32) -> Self {
        Self {
            blowfish: BlowfishGameCrypt::new(seed),
        }
    }
}

impl Decryptor for ServerGameBlowfishDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.blowfish.decrypt(src, dst);
    }
}

/// Server game encoder for Blowfish/BlowfishTwofish modes (client version <= 2.0.3).
///
/// Encodes outgoing server→client traffic with Huffman compression only (no MD5).
#[derive(Debug)]
pub struct ServerGameHuffmanEncoder {
    huffman_enc: HuffmanCompress,
}

impl ServerGameHuffmanEncoder {
    pub fn new(_seed: u32) -> Self {
        Self {
            huffman_enc: HuffmanCompress::new(),
        }
    }
}

impl Encryptor for ServerGameHuffmanEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.huffman_enc.compress(src, dst);
    }
}

/// Client game decoder for Blowfish/BlowfishTwofish modes (client version <= 2.0.3).
///
/// Decodes incoming server→client traffic with Huffman decompression only (no MD5).
#[derive(Debug)]
pub struct ClientGameHuffmanDecoder {
    huffman_dec: HuffmanDecompress,
}

impl ClientGameHuffmanDecoder {
    pub fn new(_seed: u32) -> Self {
        Self {
            huffman_dec: HuffmanDecompress::new(),
        }
    }
}

impl Decryptor for ClientGameHuffmanDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.huffman_dec.decompress(src, dst);
    }
}

/**************************************************************************************************/
// Blowfish+Twofish game encoder/decoder (client version = 2.0.3)
//
// C→S: BlowfishGameCrypt encrypt → TwofishGameCrypt crypt (chain)
// S→C: TwofishGameCrypt crypt → BlowfishGameCrypt decrypt (reverse chain)

/// Client game encoder for Blowfish+Twofish mode (client version = 2.0.3).
///
/// Encrypts outgoing client→server traffic: Blowfish encrypt, then Twofish.
#[derive(Debug)]
pub struct ClientGameBlowfishTwofishEncoder {
    blowfish: BlowfishGameCrypt,
    twofish: TwofishGameCrypt,
    buffer: BytesMut,
}

impl ClientGameBlowfishTwofishEncoder {
    pub fn new(seed: u32) -> Self {
        Self {
            blowfish: BlowfishGameCrypt::new(seed),
            twofish: TwofishGameCrypt::new(seed),
            buffer: BytesMut::with_capacity(8 * 1024),
        }
    }
}

impl Encryptor for ClientGameBlowfishTwofishEncoder {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        // Step 1: Blowfish encrypt
        self.buffer.clear();
        self.blowfish.encrypt(src, &mut self.buffer);
        // Step 2: Twofish encrypt
        self.twofish.crypt(&self.buffer, dst);
    }
}

/// Server game decoder for Blowfish+Twofish mode (client version = 2.0.3).
///
/// Decrypts incoming client→server traffic: Twofish first (undo outer layer),
/// then Blowfish decrypt (undo inner layer). Reverse of the encoding chain.
#[derive(Debug)]
pub struct ServerGameBlowfishTwofishDecoder {
    blowfish: BlowfishGameCrypt,
    twofish: TwofishGameCrypt,
    buffer: BytesMut,
}

impl ServerGameBlowfishTwofishDecoder {
    pub fn new(seed: u32) -> Self {
        Self {
            blowfish: BlowfishGameCrypt::new(seed),
            twofish: TwofishGameCrypt::new(seed),
            buffer: BytesMut::with_capacity(8 * 1024),
        }
    }
}

impl Decryptor for ServerGameBlowfishTwofishDecoder {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        // Reverse of encode: Twofish first (undo outer), then Blowfish decrypt (undo inner)
        self.buffer.clear();
        self.twofish.crypt(src, &mut self.buffer);
        self.blowfish.decrypt(&self.buffer, dst);
    }
}
