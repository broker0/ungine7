//! Server redirect packet (0x8C).

use std::net::{Ipv4Addr, SocketAddrV4};

use macros::Packet;

use crate::traits::BasicPacket;

// ── 0x8C ServerRedirect (11 bytes, S→C) ────────────────────────────────────

/// Packet 0x8C — ConnectToGameServer (11 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x8C, size = fixed(11), endian = "be")]
pub struct ServerRedirect {
    pub id: u8,
    pub ip: Ipv4Addr,
    pub port: u16,
    pub auth_key: u32,
}

impl ServerRedirect {
    pub fn new(address: SocketAddrV4, auth_key: u32) -> Self {
        Self {
            id: Self::ID,
            ip: *address.ip(),
            port: address.port(),
            auth_key,
        }
    }

    pub fn address(&self) -> SocketAddrV4 {
        SocketAddrV4::new(self.ip, self.port)
    }

    /// Create a modified copy with a different address, preserving the auth key.
    pub fn with_address(&self, new_address: SocketAddrV4) -> Self {
        Self {
            id: self.id,
            ip: *new_address.ip(),
            port: new_address.port(),
            auth_key: self.auth_key,
        }
    }
}
