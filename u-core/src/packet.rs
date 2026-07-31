use bytes::Bytes;

/// Packet direction — from the handler's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketDirection {
    ClientToServer,
    ServerToClient,
}

impl std::fmt::Display for PacketDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketDirection::ClientToServer => write!(f, "ClientToServer"),
            PacketDirection::ServerToClient => write!(f, "ServerToClient"),
        }
    }
}

// ── RawPacket ──────────────────────────────────────────────────────────────

/// A raw UO packet with directional metadata.
///
/// This is the fundamental unit of data exchange in the UO protocol stack.
/// It pairs a byte buffer ([`Bytes`]) with a [`PacketDirection`] tag that
/// indicates whether the packet travels from client to server or vice versa.
#[derive(Debug, Clone)]
pub struct RawPacket {
    pub data: Bytes,
    pub direction: PacketDirection,
}

impl RawPacket {
    pub fn new(data: Bytes, direction: PacketDirection) -> Self {
        Self { data, direction }
    }

    /// Create a ServerToClient packet from `Bytes`.
    pub fn s2c(data: Bytes) -> Self {
        Self::new(data, PacketDirection::ServerToClient)
    }

    /// Create a ServerToClient packet from a byte slice.
    pub fn s2c_raw(data: &[u8]) -> Self {
        Self::new(Bytes::copy_from_slice(data), PacketDirection::ServerToClient)
    }

    /// Create a ClientToServer packet from `Bytes`.
    pub fn c2s(data: Bytes) -> Self {
        Self::new(data, PacketDirection::ClientToServer)
    }

    /// Create a ClientToServer packet from a byte slice.
    pub fn c2s_raw(data: &[u8]) -> Self {
        Self::new(Bytes::copy_from_slice(data), PacketDirection::ClientToServer)
    }

    /// Packet id — first byte.
    pub fn id(&self) -> u8 {
        self.data[0]
    }

    /// Full packet length.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}