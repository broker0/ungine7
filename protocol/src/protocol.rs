pub use u_core::{ProtocolVersion, Role};

use crate::packets::seed::{ExtendedSeed, Seed};
use crate::packets::traits::BasicPacket;

/// Maximum seed size in bytes (extended seed 0xEF = 21 bytes).
///
/// Used as the upper bound for stack-allocated seed buffers.
/// Legacy seed is 4 bytes, extended seed (client >= 6.0.5.0) is 21 bytes.
pub const LEGACY_SEED_SIZE: usize = Seed::SIZE.fixed_len().unwrap();
pub const EXT_SEED_SIZE: usize = ExtendedSeed::SIZE.fixed_len().unwrap();
pub const MAX_SEED_SIZE: usize = EXT_SEED_SIZE;


#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Protocol {
    Login(LoginProtocolInfo),
    Game(GameProtocolInfo),
}

impl Protocol {
    /// Get the client version regardless of protocol type
    pub fn client_version(&self) -> ProtocolVersion {
        match self {
            Self::Login(info) => info.client_version,
            Self::Game(info) => info.client_version,
        }
    }

    pub fn is_encrypted(&self) -> bool {
        match self {
            Self::Login(info) => info.encrypted,
            Self::Game(info) => info.encrypted,
        }
    }

    pub fn seed_size(&self) -> usize {
        match self {
            Self::Login(info) => info.seed_size,
            Self::Game(info) => info.seed_size,
        }
    }

    pub fn login(seed: u32, version: ProtocolVersion, encrypted: bool) -> Self {
        let seed_size = if version >= ProtocolVersion::EXT_SEED_CLIENT {
            EXT_SEED_SIZE
        } else {
            LEGACY_SEED_SIZE
        };

        Self::Login(LoginProtocolInfo {
            seed,
            seed_size,
            client_version: version,
            encrypted,
        })
    }

    pub fn game(seed: u32, auth_key: u32, version: ProtocolVersion, encrypted: bool) -> Self {
        Self::Game(GameProtocolInfo {
            seed,
            seed_size: LEGACY_SEED_SIZE,
            auth_key,
            client_version: version,
            encrypted,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LoginProtocolInfo {
    pub seed: u32,
    pub seed_size: usize, // 4 for normal seed, MAX_SEED_SIZE for 0xEF extended seed
    pub client_version: ProtocolVersion,
    pub encrypted: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GameProtocolInfo {
    pub seed: u32,
    pub seed_size: usize, // usually 4, kept for consistency with login
    pub client_version: ProtocolVersion,
    pub encrypted: bool,
    pub auth_key: u32,
}
