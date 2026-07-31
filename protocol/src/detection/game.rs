use bytes::BytesMut;
use log::{debug, trace};
use u_core::ProtocolVersion;
use super::strategy::DetectionStrategy;
use super::DetectionError;
use crate::logs;
use crate::binder::ConnectionBinder;
use crate::codec::encryption::Decryptor;
use crate::encoding::encoders::ServerGameBlowfishTwofishDecoder;
use crate::encoding::game::{BlowfishGameCrypt, TwofishGameCrypt};
use crate::protocol::{GameProtocolInfo, Protocol, LEGACY_SEED_SIZE};
use crate::packets::traits::BasicPacket;
use crate::packets::seed::Seed;
use crate::packets::login::GameLogin;

pub struct GameDetectionStrategy {
    expected_version: Option<ProtocolVersion>,
    binder: Option<ConnectionBinder>,
}

impl GameDetectionStrategy {
    pub fn new() -> Self {
        Self {
            expected_version: None,
            binder: None,
        }
    }

    pub fn with_version(mut self, version: ProtocolVersion) -> Self {
        self.expected_version = Some(version);
        self
    }

    pub fn with_binder(mut self, binder: ConnectionBinder) -> Self {
        self.binder = Some(binder);
        self
    }
}

impl Default for GameDetectionStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GameDetectionStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameDetectionStrategy")
            .field("expected_version", &self.expected_version)
            .finish()
    }
}

impl GameDetectionStrategy {
    /// Try to resolve the version from the binder using the auth key.
    fn resolve_version(&self, auth_key: u32) -> Option<ProtocolVersion> {
        self.binder.as_ref().and_then(|b| b.peek_version(auth_key))
    }
}

const LOGIN_SIZE: usize = GameLogin::SIZE.fixed_len().unwrap();

impl DetectionStrategy for GameDetectionStrategy {
    fn name(&self) -> &str {
        "game_phase"
    }
    fn min_bytes(&self) -> usize {
        LEGACY_SEED_SIZE + LOGIN_SIZE
    }

    fn detect(&self, buf: &[u8]) -> Result<Option<Protocol>, DetectionError> {
        if buf.len() < self.min_bytes() {
            return Err(DetectionError::InsufficientData {
                received: buf.len(),
                minimum: self.min_bytes(),
            });
        }

        let s = Seed::decode_raw(buf)
            .map_err(|_| DetectionError::InsufficientData {
                received: buf.len(),
                minimum: LEGACY_SEED_SIZE,
            })?;
        let seed = s.value;

        if seed == 0 {
            return Ok(None);
        }

        let payload = &buf[LEGACY_SEED_SIZE..];

        let found = |auth_key, version: ProtocolVersion, encrypted| {
            Ok(Some(Protocol::Game(GameProtocolInfo {
                seed,
                seed_size: LEGACY_SEED_SIZE,
                auth_key,
                client_version: version,
                encrypted,
            })))
        };

        if is_valid_game_login(payload) {
            let auth_key = extract_auth_key(payload);
            let version = self.resolve_version(auth_key)
                .unwrap_or(self.expected_version.unwrap_or(ProtocolVersion::SA_CLIENT));
            trace!(target: logs::DETECTION, "game phase detected (unencrypted): seed=0x{seed:08X}, key=0x{auth_key:08X}");
            return found(auth_key, version, false);
        }

        if let Some(version) = self.expected_version {
            let decrypted = game_decrypt_with_version(seed, version, payload);
            if is_valid_game_login(&decrypted) {
                let auth_key = extract_auth_key(&decrypted);
                trace!(target: logs::DETECTION, "game phase detected (encrypted, known version): seed=0x{seed:08X}, key=0x{auth_key:08X}");
                return found(auth_key, version, true);
            }
        } else {
            if let Some((decrypted, detected_version)) =
                game_decrypt_any(seed, payload, is_valid_game_login)
            {
                let auth_key = extract_auth_key(&decrypted);
                // If a binder is available, prefer its version over the brute-forced one
                let version = self.resolve_version(auth_key)
                    .unwrap_or(detected_version);
                if version != detected_version {
                    debug!(target: logs::DETECTION,
                        "game phase: binder resolved version {version} (brute-force had {detected_version})");
                }
                trace!(target: logs::DETECTION, "game phase detected (encrypted, probed): seed=0x{seed:08X}, key=0x{auth_key:08X}, version={version}");
                return found(auth_key, version, true);
            }
        }

        Ok(None)
    }
}

fn game_decrypt(seed: u32, encrypted_payload: &[u8]) -> BytesMut {
    let mut crypt = TwofishGameCrypt::new(seed);
    let mut dst = BytesMut::with_capacity(encrypted_payload.len());
    crypt.crypt(encrypted_payload, &mut dst);
    dst
}

fn game_decrypt_any(
    seed: u32,
    encrypted_payload: &[u8],
    validator: impl Fn(&[u8]) -> bool,
) -> Option<(BytesMut, ProtocolVersion)> {
    let blowfish_twofish = ProtocolVersion::BLOWFISH_TWOFISH_CLIENT;

    let decrypted = game_decrypt(seed, encrypted_payload);
    if validator(&decrypted) {
        return Some((decrypted, ProtocolVersion::DEFAULT));
    }

    {
        let mut crypt = BlowfishGameCrypt::new(seed);
        let mut dst = BytesMut::with_capacity(encrypted_payload.len());
        crypt.decrypt(encrypted_payload, &mut dst);
        if validator(&dst) {
            return Some((dst, ProtocolVersion::MINIMAL_SUPPORTED_CLIENT));
        }
    }

    {
        let mut twofish = TwofishGameCrypt::new(seed);
        let mut blowfish = BlowfishGameCrypt::new(seed);
        let mut intermediate = BytesMut::with_capacity(encrypted_payload.len());
        twofish.crypt(encrypted_payload, &mut intermediate);
        let mut dst = BytesMut::with_capacity(intermediate.len());
        blowfish.decrypt(&intermediate, &mut dst);
        if validator(&dst) {
            return Some((dst, blowfish_twofish));
        }
    }

    None
}

fn game_decrypt_with_version(
    seed: u32,
    version: ProtocolVersion,
    encrypted_payload: &[u8],
) -> BytesMut {
    let blowfish_twofish = ProtocolVersion::BLOWFISH_TWOFISH_CLIENT;

    if version < blowfish_twofish {
        let mut crypt = BlowfishGameCrypt::new(seed);
        let mut dst = BytesMut::with_capacity(encrypted_payload.len());
        crypt.decrypt(encrypted_payload, &mut dst);
        dst
    } else if version == blowfish_twofish {
        let mut decoder = ServerGameBlowfishTwofishDecoder::new(seed);
        let mut dst = BytesMut::new();
        decoder.decrypt(encrypted_payload, &mut dst);
        dst
    } else {
        game_decrypt(seed, encrypted_payload)
    }
}

fn is_valid_game_login(payload: &[u8]) -> bool {
    payload.len() >= LOGIN_SIZE
        && payload[0] == GameLogin::ID
        && payload[5..=64]
            .iter()
            .all(|&b| b == 0 || (32..128).contains(&b))
}

fn extract_auth_key(payload: &[u8]) -> u32 {
    u32::from_be_bytes([payload[1], payload[2], payload[3], payload[4]])
}
