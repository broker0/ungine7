use super::strategy::DetectionStrategy;
use super::version_db::VersionDatabase;
use super::DetectionError;
use crate::packets::seed::{ExtendedSeed, Seed};
use crate::packets::traits::BasicPacket;
use crate::encoding::login::XorLoginCrypt;
use crate::logs;
use crate::protocol::{LoginProtocolInfo, Protocol, EXT_SEED_SIZE, LEGACY_SEED_SIZE};
use bytes::BytesMut;
use log::{debug, trace};
use u_core::ProtocolVersion;
use crate::packets::login::AccountLogin;
use u_io::{Decode, BE};

#[derive(Debug)]
pub struct LoginDetectionStrategy {
    version_db: VersionDatabase,
    expected_version: Option<ProtocolVersion>,
}

impl LoginDetectionStrategy {
    pub fn new(version_db: VersionDatabase) -> Self {
        Self { version_db, expected_version: None }
    }

    /// Set the expected client version for unencrypted clients without an
    /// extended seed (< 6.0.5.0). When present, this version is used instead
    /// of the default AOS (4.0.0.0) fallback.
    pub fn with_expected_version(mut self, version: Option<ProtocolVersion>) -> Self {
        self.expected_version = version;
        self
    }
}

const LOGIN_SIZE: usize = AccountLogin::SIZE.fixed_len().unwrap();


impl DetectionStrategy for LoginDetectionStrategy {
    fn name(&self) -> &str {
        "login_phase"
    }

    fn min_bytes(&self) -> usize {
        LEGACY_SEED_SIZE + LOGIN_SIZE
    }

    fn detect(&self, buf: &[u8]) -> Result<Option<Protocol>, DetectionError> {
        let is_extended_seed = buf.len() > 0 && buf[0] == ExtendedSeed::ID;

        let expected_len = if is_extended_seed {
            EXT_SEED_SIZE + LOGIN_SIZE
        } else {
            self.min_bytes()
        };
        if buf.len() < expected_len {
            return Err(DetectionError::InsufficientData {
                received: buf.len(),
                minimum: expected_len,
            });
        }

        let (
            seed,
            seed_size,
            known_version,
            payload
        ) = if is_extended_seed {
            let mut reader = u_io::BinaryReader::<BE>::new(buf);
            let ext_seed = <ExtendedSeed as Decode<BE>>::decode(&mut reader)
                .map_err(|_| DetectionError::InsufficientData {
                    received: buf.len(),
                    minimum: EXT_SEED_SIZE,
                })?;
            (
                ext_seed.seed,
                EXT_SEED_SIZE,
                Some(ext_seed.version()),
                &buf[EXT_SEED_SIZE..],
            )
        } else {
            let leg_seed = Seed::decode_raw(buf)
                .map_err(|_| DetectionError::InsufficientData {
                    received: buf.len(),
                    minimum: LEGACY_SEED_SIZE,
                })?;
            (
                leg_seed.value,
                LEGACY_SEED_SIZE,
                None,
                &buf[LEGACY_SEED_SIZE..]
            )
        };

        if seed == 0 {
            return Ok(None);
        }

        let found = |version: ProtocolVersion, encrypted| {
            Ok(Some(Protocol::Login(LoginProtocolInfo {
                seed,
                seed_size,
                client_version: version,
                encrypted,
            })))
        };

        if is_valid_login_request(payload) {
            let version = if let Some(version) = known_version {
                version
            } else if let Some(expected) = self.expected_version {
                debug!(target: logs::DETECTION, "login phase unknown version of unencrypted client, using expected version {expected}");
                expected
            } else {
                debug!(target: logs::DETECTION, "login phase unknown version of unencrypted client, fallback on AOS version");
                ProtocolVersion::AOS_CLIENT
            };
            trace!(target: logs::DETECTION, "login phase detected (unencrypted): seed=0x{seed:08X}, version={version}");
            return found(version, false);
        }

        if let Some(v) = known_version {
            let decrypted = login_decrypt(seed, v, payload);
            if is_valid_login_request(&decrypted) {
                trace!(target: logs::DETECTION, "login phase detected (encrypted, extended seed): seed=0x{seed:08X}, version={v}");
                return found(v, true);
            }
        } else {
            for version in self.version_db.iter() {
                let decrypted = login_decrypt(seed, *version, payload);
                if is_valid_login_request(&decrypted) {
                    trace!(
                        target: logs::DETECTION,
                        "login phase detected (encrypted): seed=0x{seed:08X}, version={version}"
                    );
                    return found(*version, true);
                }
            }
        }

        Ok(None)
    }
}

fn login_decrypt(seed: u32, version: ProtocolVersion, encrypted_payload: &[u8]) -> BytesMut {
    let mut crypt = XorLoginCrypt::new(seed, version);
    let mut dst = BytesMut::with_capacity(encrypted_payload.len());
    crypt.encode(encrypted_payload, &mut dst);
    dst
}

fn is_valid_login_request(payload: &[u8]) -> bool {
    payload.len() >= LOGIN_SIZE
        && payload[0] == AccountLogin::ID
        && payload[1..=60]
            .iter()
            .all(|&b| b == 0 || (32..128).contains(&b))
}

