// Adapted from the RustCrypto blowfish crate:
// https://github.com/RustCrypto/block-ciphers/tree/master/blowfish
//
// Copyright (c) 2016-2024 The RustCrypto Project Developers
// Licensed under the MIT License. The upstream project is available under
// Apache-2.0 OR MIT; this project distributes this adapted file under MIT.
// See THIRD_PARTY_NOTICES.md for the complete license text and provenance.

use super::blowfish_consts;

/// Blowfish block cipher (Big-Endian).
///
/// Operates on 8-byte blocks. Key length is 4 to 56 bytes.
#[derive(Clone)]
pub struct Blowfish {
    s: [[u32; 256]; 4],
    p: [u32; 18],
}

impl std::fmt::Debug for Blowfish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Blowfish { ... }")
    }
}

/// Read a big-endian u32 from the key buffer, wrapping around if the key
/// is shorter than 4 bytes. This matches the standard Blowfish key schedule.
fn next_u32_wrap(buf: &[u8], offset: &mut usize) -> u32 {
    let mut v = 0u32;
    for _ in 0..4 {
        if *offset >= buf.len() {
            *offset = 0;
        }
        v = (v << 8) | buf[*offset] as u32;
        *offset += 1;
    }
    v
}

impl Blowfish {
    /// Create a new Blowfish cipher with the given key.
    ///
    /// Key length must be between 4 and 56 bytes (inclusive).
    ///
    /// # Panics
    ///
    /// Panics if the key length is outside the valid range.
    pub fn new(key: &[u8]) -> Self {
        assert!(
            key.len() >= 4 && key.len() <= 56,
            "Blowfish key must be 4-56 bytes, got {}",
            key.len()
        );
        let mut bf = Self::init_state();
        bf.expand_key(key);
        bf
    }

    /// Initialize with the standard P and S constants (digits of pi).
    fn init_state() -> Self {
        Self {
            p: blowfish_consts::P,
            s: blowfish_consts::S,
        }
    }

    /// Expand the key into the P-array and S-boxes.
    fn expand_key(&mut self, key: &[u8]) {
        let mut key_pos = 0;
        for i in 0..18 {
            self.p[i] ^= next_u32_wrap(key, &mut key_pos);
        }

        let mut lr = [0u32; 2];
        for i in 0..9 {
            lr = self.encrypt_pair(lr);
            self.p[2 * i] = lr[0];
            self.p[2 * i + 1] = lr[1];
        }
        for i in 0..4 {
            for j in 0..128 {
                lr = self.encrypt_pair(lr);
                self.s[i][2 * j] = lr[0];
                self.s[i][2 * j + 1] = lr[1];
            }
        }
    }

    /// The Blowfish F function: split x into 4 bytes, look up S-boxes,
    /// combine with add and XOR.
    #[inline]
    fn round_function(&self, x: u32) -> u32 {
        let a = self.s[0][(x >> 24) as usize];
        let b = self.s[1][((x >> 16) & 0xff) as usize];
        let c = self.s[2][((x >> 8) & 0xff) as usize];
        let d = self.s[3][(x & 0xff) as usize];
        (a.wrapping_add(b) ^ c).wrapping_add(d)
    }

    /// Encrypt a pair of u32 values (internal, used during key expansion).
    fn encrypt_pair(&self, [mut l, mut r]: [u32; 2]) -> [u32; 2] {
        for i in 0..8 {
            l ^= self.p[2 * i];
            r ^= self.round_function(l);
            r ^= self.p[2 * i + 1];
            l ^= self.round_function(r);
        }
        l ^= self.p[16];
        r ^= self.p[17];
        [r, l]
    }

    /// Decrypt a pair of u32 values (internal).
    fn decrypt_pair(&self, [mut l, mut r]: [u32; 2]) -> [u32; 2] {
        for i in (1..9).rev() {
            l ^= self.p[2 * i + 1];
            r ^= self.round_function(l);
            r ^= self.p[2 * i];
            l ^= self.round_function(r);
        }
        l ^= self.p[1];
        r ^= self.p[0];
        [r, l]
    }

    /// Encrypt an 8-byte block in place (Big-Endian).
    pub fn encrypt(&self, block: &mut [u8; 8]) {
        let l = u32::from_be_bytes([block[0], block[1], block[2], block[3]]);
        let r = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
        let [rl, rr] = self.encrypt_pair([l, r]);
        block[0..4].copy_from_slice(&rl.to_be_bytes());
        block[4..8].copy_from_slice(&rr.to_be_bytes());
    }

    /// Decrypt an 8-byte block in place (Big-Endian).
    pub fn decrypt(&self, block: &mut [u8; 8]) {
        let l = u32::from_be_bytes([block[0], block[1], block[2], block[3]]);
        let r = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
        let [rl, rr] = self.decrypt_pair([l, r]);
        block[0..4].copy_from_slice(&rl.to_be_bytes());
        block[4..8].copy_from_slice(&rr.to_be_bytes());
    }
}
