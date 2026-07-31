pub mod error;
pub mod logs;
pub mod handler;
pub mod session;
pub mod relay;
pub mod listener;
pub mod client;
pub mod proxy;

// ── Prelude ────────────────────────────────────────────────────────────────

/// Prelude — the most commonly needed network types.
///
/// ```rust,ignore
/// use network::prelude::*;
/// ```
pub mod prelude {
    pub use crate::error::{NetworkError, Result};
    pub use crate::handler::{HandlerChain, HandlerResult};
    pub use crate::handler::packet_handler::{HandlerAction, PacketHandler};
    pub use crate::listener::{
        ConnectionContext, ListenerConfig, ListenerControl, ListenerHandler,
        SessionPhase, Listener,
    };
    pub use crate::session::{PacketSink, RecvResult, SendResult, Session, SessionBuilder, SessionEvent};
    pub use crate::client::{ClientConfig, PacketClient};
    pub use crate::proxy::connect_upstream;
    pub use crate::relay::relay;
}
