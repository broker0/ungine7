//! UO TCP server for path-server.
//!
//! Listens on port 2593 (configurable), handles the UO login handshake
//! (AccountLogin 0x80 → GameServerList, SelectServer 0xA0 → ServerRedirect),
//! and routes game sessions to [`session::run_game_session`].

pub mod session;
pub mod world_events;

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use log::info;

use protocol::Protocol;
use u_core::ProtocolVersion;

use network::error;
use network::handler::HandlerChain;
use network::listener::{
    ConnectionContext, ListenerConfig, ListenerHandler, SessionPhase, Listener,
};
use network::session::Session;

use crate::state::AppState;

// ── PathServer ───────────────────────────────────────────────────────────

pub struct PathServer {
    pub state: Arc<AppState>,
    /// Shared login handler (moira login phase).
    pub login_handler: common::login_handler::LoginHandler,
}

#[async_trait::async_trait]
impl ListenerHandler for PathServer {
    fn configure_handlers(
        &self,
        _phase: SessionPhase,
        _ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        (HandlerChain::new(), HandlerChain::new())
    }

    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut session: Session,
    ) -> error::Result<()> {
        let addr = ctx.addr;
        let is_game = matches!(&ctx.protocol, Protocol::Game(_));

        if is_game {
            // Prefer the version recorded by the login-phase binder (authoritative),
            // fall back to the version inferred by the game-phase detector.
            let client_version = ctx.bound_connection
                .as_ref()
                .map(|b| b.client_version)
                .unwrap_or_else(|| ctx.protocol.client_version());

            let (observer_tx, observer_rx) = tokio::sync::mpsc::channel(4096);
            session::run_game_session(
                &mut session,
                &self.state,
                addr,
                client_version,
                observer_rx,
                observer_tx,
            )
            .await?;
        } else {
            // Login session — delegated to the shared login handler.
            self.login_handler.run_login_session(&mut session, ctx).await?;
        }

        session.close().await;
        Ok(())
    }
}

/// Start the UO listener and block until it stops.
pub async fn run_uo_listener(
    state: Arc<AppState>,
    listen_addr: &str,
    server_ip: Ipv4Addr,
    server_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let account_store = Arc::new(common::uo_engine::auth::MemoryAccountStore::new());
    let authenticator = Arc::new(common::uo_engine::auth::PlainAuthenticator {
        store: account_store,
        admin_usernames: vec!["admin".to_string()],
    });
    let session_manager = Arc::new(common::uo_engine::auth::SimpleSessionManager::new());

    info!("UO listener: {}", listen_addr);

    let server_addr = SocketAddrV4::new(server_ip, server_port);
    let config = ListenerConfig::new(listen_addr);
    let handler = PathServer {
        state,
        login_handler: common::login_handler::LoginHandler {
            server_name: "Path Server".to_string(),
            server_addr,
            version: ProtocolVersion::new(3, 0, 8, 0),
            encrypted: true,
            authenticator,
            session_manager,
        },
    };

    Listener::new(config, handler).run().await?;
    Ok(())
}
