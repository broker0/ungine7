use crate::encoding::blowfish::Blowfish;
use crate::encoding::blowfish_game_consts::*;
use crate::encoding::md5::compute;
use crate::encoding::twofish::Twofish;
use bytes::{BufMut, BytesMut};

#[derive(Debug)]
pub struct MD5GameCrypt {
    table: [u8; 16],
    offset: usize,
}

impl MD5GameCrypt {
    pub fn new(key: u32) -> MD5GameCrypt {
        let tf = TwofishGameCrypt::new(key);
        let digest = compute(tf.table);

        Self {
            table: digest.0,
            offset: 0,
        }
    }

    pub fn crypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        for b in src {
            dst.put_u8(b ^ self.table[self.offset]);
            self.offset = (self.offset + 1) % self.table.len();
        }
    }
}

#[derive(Debug)]
pub struct TwofishGameCrypt {
    twofish: Twofish,
    offset: usize,
    table: [u8; 256],
}

impl TwofishGameCrypt {
    pub fn new(key: u32) -> TwofishGameCrypt {
        let mut twofish = Twofish::new();
        let key = key.to_be_bytes();
        let key = [key, key, key, key].concat();

        twofish.key_schedule(&key);

        let mut table = [0; 256];
        for (idx, x) in table.iter_mut().enumerate() {
            *x = idx as u8;
        }

        for chunk in table.chunks_mut(16) {
            twofish.encrypt(chunk);
        }

        Self {
            twofish,
            offset: 0,
            table,
        }
    }

    pub fn crypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        for b in src {
            if self.offset >= self.table.len() {
                for chunk in self.table.chunks_mut(16) {
                    self.twofish.encrypt(chunk);
                }
                self.offset = 0;
            }

            dst.put_u8(b ^ self.table[self.offset]);
            self.offset += 1;
        }
    }
}

// ── Blowfish-based game encryption (clients < 2.0.3) ──────────────────────

/// UO-specific stream cipher based on the Blowfish block cipher.
///
/// Uses 25 pre-computed Blowfish instances (one per key in `KEY_TABLE`),
/// a seed table with periodic switching every `GAME_TABLE_TRIGGER` bytes,
/// and an XOR stream with cipher-feedback.
///
/// Unlike `TwofishGameCrypt`, encrypt and decrypt are **not** the same
/// operation — the feedback value differs (encrypted byte vs original byte).
#[derive(Debug)]
pub struct BlowfishGameCrypt {
    /// 25 Blowfish cipher instances, one per key in KEY_TABLE.
    ciphers: Vec<Blowfish>,
    /// Current index into the seed/key tables.
    seed_table_index: usize,
    /// Bytes processed since last seed switch.
    byte_counter: usize,
    /// Current 8-byte seed (XOR keystream, re-encrypted periodically).
    seed: [u8; GAME_SEED_LENGTH],
    /// Current position within the seed.
    seed_pos: usize,
}

impl BlowfishGameCrypt {
    /// Create a new BlowfishGameCrypt instance.
    ///
    /// The `_key` parameter is accepted for API consistency with other
    /// game crypt types but is not used — Blowfish game encryption uses
    /// fixed keys from `KEY_TABLE`.
    pub fn new(_key: u32) -> Self {
        let ciphers: Vec<Blowfish> = KEY_TABLE.iter().map(|k| Blowfish::new(k)).collect();

        Self {
            ciphers,
            seed_table_index: GAME_TABLE_START,
            byte_counter: 0,
            seed_pos: 0,
            seed: SEED_TABLE[0][GAME_TABLE_START][0],
        }
    }

    /// Encrypt plaintext bytes (client→server direction).
    ///
    /// Feedback uses the **encrypted** (output) byte.
    pub fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.process(src, dst, true);
    }

    /// Decrypt ciphertext bytes (server→client direction for decoding client data).
    ///
    /// Feedback uses the **original** (input) byte.
    pub fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut) {
        self.process(src, dst, false);
    }

    fn process(&mut self, src: &[u8], dst: &mut BytesMut, encrypting: bool) {
        for &b in src {
            // Check if we need to switch to the next seed
            if self.byte_counter >= GAME_TABLE_TRIGGER {
                self.byte_counter = 0;
                self.seed_table_index =
                    (self.seed_table_index + GAME_TABLE_STEP) % GAME_TABLE_MODULO;
                self.seed = SEED_TABLE[1][self.seed_table_index][0];
                self.seed_pos = 0;
            }

            // Re-encrypt the seed when we start a new cycle through it
            if self.seed_pos == 0 {
                self.ciphers[self.seed_table_index].encrypt(&mut self.seed);
            }

            // XOR with the current seed byte
            let result = b ^ self.seed[self.seed_pos];
            dst.put_u8(result);

            // Update seed with feedback:
            // - encrypting: use the encrypted (output) value
            // - decrypting: use the original (input) value
            if encrypting {
                self.seed[self.seed_pos] = result;
            } else {
                self.seed[self.seed_pos] = b;
            }

            self.seed_pos = (self.seed_pos + 1) % GAME_SEED_LENGTH;
            self.byte_counter += 1;
        }
    }
}
