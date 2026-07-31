//! Proxy utilities for building UO proxies on top of [`Listener`](crate::listener::Listener).
//!
//! Provides helper functions that encapsulate the boilerplate steps every
//! UO proxy needs when implementing
//! [`ListenerHandler::handle_session`]:
//!
//! 1. Determine the server-side [`SessionPhase`]
//! 2. Resolve the upstream target address
//! 3. Connect to the upstream server
//! 4. Build a client-role transport
//! 5. Attach handler chains
//! 6. Create a server-side [`Session`]
//! 7. Relay packets between the two sessions
//!
//! # Example
//!
//! ```rust,ignore
//! async fn handle_session(&self, ctx: &ConnectionContext, mut client: Session) -> error::Result<()> {
//!     let target = ctx.upstream_addr(&self.server_addr);
//!     let mut server = proxy::connect_upstream(ctx, &target, self).await?;
//!     relay::relay("[proxy]", &mut client, &mut server, None).await
//! }
//! ```

use tokio::net::TcpStream;
use protocol::transport::builder::TransportBuilder;

use crate::error;
use crate::listener::{ConnectionContext, ListenerHandler, SessionPhase};
use crate::session::Session;

/// Connect to the upstream server and build a [`Session`] with handler chains.
///
/// This performs the standard proxy setup:
/// 1. Opens a TCP connection to `target_addr`
/// 2. Builds a client-role transport using the detected protocol from `ctx`
/// 3. Calls `handler.configure_handlers()` with the server-side phase
/// 4. Returns a ready-to-use [`Session`]
///
/// The returned session is typically passed to [`relay::relay`](crate::relay::relay)
/// alongside the client session.
pub async fn connect_upstream(
    ctx: &ConnectionContext,
    target_addr: &str,
    handler: &dyn ListenerHandler,
) -> error::Result<Session> {
    let server_phase = SessionPhase::server_for(&ctx.protocol);
    let stream = TcpStream::connect(target_addr).await?;
    let (transport, direction) = TransportBuilder::client(stream, &ctx.protocol).build()?;
    let (inbound, outbound) = handler.configure_handlers(server_phase, ctx);
    Ok(Session::with_handlers(transport, direction, inbound, outbound))
}
