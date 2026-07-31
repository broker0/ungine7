use std::any::Any;
use std::net::SocketAddrV4;
use std::time::Instant;
use log::{debug, error};
use u_core::{PacketDirection, ProtocolVersion};
use protocol::RawPacket;
use protocol::packets::redirect::ServerRedirect;
use protocol::prelude::{encode_packet, BasicPacket};
use protocol::binder::{ConnectionBinder, PendingConnection};
use crate::logs;

use crate::handler::packet_handler::{HandlerAction, PacketHandler};

/// Factory function for creating context data to attach to
/// [`PendingConnection::context`] when a redirect is intercepted.
///
/// Receives the parsed [`ServerRedirect`] packet so the factory
/// can inspect the auth key or server address if needed.
pub type ContextFactory = Box<dyn Fn(&ServerRedirect) -> Box<dyn Any + Send + Sync> + Send + Sync>;

/// Handler that intercepts packet 0x8C (ServerRedirect),
/// registers a pending connection in the `ConnectionBinder`, rewrites
/// the address to point at the proxy, and stops the session.
///
/// Optionally attaches application-specific context data to the
/// `PendingConnection` via a [`ContextFactory`]. This is useful for
/// linking login-phase sessions to their subsequent game-phase sessions
/// (e.g. storing a session ID in the binder entry).
///
/// # Example
///
/// ```rust,ignore
/// // Basic usage (no context):
/// let handler = RedirectHandler::new(proxy_addr, binder, version, true, 4);
///
/// // With context factory (e.g. linking session IDs):
/// let session_id = 42u64;
/// let handler = RedirectHandler::new(proxy_addr, binder, version, true, 4)
///     .with_context(move |_redirect| Box::new(session_id));
/// ```
pub struct RedirectHandler {
    proxy_address: SocketAddrV4,
    binder: ConnectionBinder,
    client_version: ProtocolVersion,
    encrypted: bool,
    seed_size: usize,
    context_factory: Option<ContextFactory>,
}

impl std::fmt::Debug for RedirectHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedirectHandler")
            .field("proxy_address", &self.proxy_address)
            .field("client_version", &self.client_version)
            .field("encrypted", &self.encrypted)
            .field("seed_size", &self.seed_size)
            .field("context_factory", &self.context_factory.as_ref().map(|_| ".."))
            .finish()
    }
}

impl RedirectHandler {
    pub fn new(
        proxy_address: SocketAddrV4,
        binder: ConnectionBinder,
        client_version: ProtocolVersion,
        encrypted: bool,
        seed_size: usize,
    ) -> Self {
        Self {
            proxy_address,
            binder,
            client_version,
            encrypted,
            seed_size,
            context_factory: None,
        }
    }

    /// Attach a factory that produces context data for each intercepted
    /// redirect.  The context is stored in
    /// [`PendingConnection::context`] and can be retrieved from
    /// [`BoundConnection::context`](protocol::binder::BoundConnection::context) when the game-phase connection
    /// arrives.
    pub fn with_context<F>(mut self, factory: F) -> Self
    where
        F: Fn(&ServerRedirect) -> Box<dyn Any + Send + Sync> + Send + Sync + 'static,
    {
        self.context_factory = Some(Box::new(factory));
        self
    }
}

impl PacketHandler for RedirectHandler {
    fn name(&self) -> &str {
        "redirect"
    }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if packet.id() != <ServerRedirect as BasicPacket>::ID {
            return HandlerAction::Forward(packet);
        }

        let redirect = match ServerRedirect::from_bytes(&packet.data) {
            Ok(r) => r,
            Err(e) => {
                error!(target: logs::REDIRECT, "failed to parse 0x8C: {e}");
                return HandlerAction::Forward(packet);  // maybe stop connection?
            }
        };

        debug!(target: logs::REDIRECT,
            "intercepted 0x8C: server={} auth_key=0x{:08X}",
            redirect.address(), redirect.auth_key);

        let context: Box<dyn Any + Send + Sync> = match &self.context_factory {
            Some(factory) => factory(&redirect),
            None => Box::new(()),
        };

        let pending = PendingConnection {
            auth_key: redirect.auth_key,
            client_version: self.client_version,
            game_server_address: Some(redirect.address()),
            encrypted: self.encrypted,
            seed_size: self.seed_size,
            created_at: Instant::now(),
            context,
        };

        if let Err(e) = self.binder.register(pending) {
            error!(target: logs::REDIRECT, "binder register failed: {e}");
            return HandlerAction::Forward(packet);
        }

        // Rewrite the redirect to point at the proxy
        let rewritten = redirect.with_address(self.proxy_address);
        debug!(target: logs::REDIRECT, "rewritten 0x8C: {} → {}", redirect.address(), rewritten.address());

        HandlerAction::Stop(RawPacket::new(encode_packet(&rewritten), packet.direction))
    }
}
