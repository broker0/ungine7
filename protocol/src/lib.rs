pub mod codec;
pub mod transport;
pub mod detection;
pub mod encoding;
pub mod logs;
pub mod protocol;
pub mod packets;
pub mod binder;
pub mod connector;

// ── Convenience re-exports ─────────────────────────────────────────────────

pub use u_core::{PacketDirection, ProtocolVersion, RawPacket, Role};
pub use protocol::{Protocol, LoginProtocolInfo, GameProtocolInfo};
pub use codec::CodecError;

// ── Prelude ────────────────────────────────────────────────────────────────

/// Prelude — the most commonly needed protocol types.
///
/// ```rust,ignore
/// use protocol::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{CodecError, PacketDirection, Protocol, ProtocolVersion, RawPacket, Role};
    pub use crate::binder::{BinderConfig, BoundConnection, ConnectionBinder, PendingConnection};
    pub use crate::connector::ConnectorConfig;
    pub use crate::transport::{PacketTransport, TransportError, TransportEvent};
    pub use crate::transport::builder::{TransportBuildError, TransportBuilder};
    // Packet traits.
    pub use packets::traits::{encode_packet, ManualPacket, PacketError, BasicPacket};
}
