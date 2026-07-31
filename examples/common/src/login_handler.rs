//! Reusable login-phase handler for UO server examples.
//!
//! Handles the two login packets (0x80 AccountLogin → GameServerList,
//! 0xA0 SelectServer → ServerRedirect) that are identical across
//! `demo-server` and `path-server`.
//!
//! The game-phase login (0x91 GameLogin → CharacterList) is **not**
//! included — it differs significantly between servers.

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Instant;

use log::{error, info};

use protocol::binder::PendingConnection;
use protocol::{Protocol, RawPacket};
use protocol::packets::login::*;
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{encode_packet, BasicPacket};
use u_core::ProtocolVersion;

use network::error::{self, NetworkError};
use network::listener::ConnectionContext;
use network::session::{Session, SessionEvent};

use framework::moira::{AccountStatus, Authenticator, SessionManager};

use crate::uo_engine::auth::{
    Credential, DemoAccount, AccessLevel, PlainAuthenticator, SimpleSessionManager,
};

// ── LoginHandler ──────────────────────────────────────────────────────────

/// Configuration for the shared login-phase handler.
pub struct LoginHandler {
    /// Display name shown in the GameServerList (e.g. "Demo Server").
    pub server_name: String,
    /// Address the client will reconnect to for the game phase.
    pub server_addr: SocketAddrV4,
    /// Fallback client version used in the `PendingConnection` when the
    /// real client version cannot be determined from the login-phase context.
    ///
    /// In practice this is never used: the version is always taken from
    /// `ctx.protocol.client_version()` (i.e. from the extended seed or
    /// brute-force detection done during the login handshake).  The field
    /// is kept for callers that still initialise it explicitly.
    pub version: ProtocolVersion,
    /// Fallback encryption flag for the game-phase `PendingConnection`.
    ///
    /// Like `version`, this is superseded by the value detected in the
    /// login-phase context (`ctx.protocol.is_encrypted()`).
    pub encrypted: bool,
    /// Authenticator (moira).
    pub authenticator: Arc<PlainAuthenticator>,
    /// Session manager (moira).
    pub session_manager: Arc<SimpleSessionManager>,
}

impl LoginHandler {
    /// Handle a single login-phase packet (0x80 or 0xA0).
    ///
    /// Returns `Some(packets)` if a response should be sent,
    /// `None` if the packet is not a login-phase packet.
    ///
    /// `authenticated_account` is used to carry the account from the 0x80
    /// authentication step to the 0xA0 session-creation step, so the real
    /// [`AccessLevel`] is stored in the session manager.
    pub fn handle_login_packet(
        &self,
        addr: SocketAddr,
        ctx: &ConnectionContext,
        packet: &RawPacket,
        authenticated_account: &mut Option<DemoAccount>,
    ) -> Option<Vec<RawPacket>> {
        match packet.id() {
            0x80 => {
                if let Ok(login) = AccountLogin::from_bytes(&packet.data) {
                    let credential = Credential {
                        password: login.password.to_string(),
                    };

                    match self.authenticator.authenticate(&login.account, &credential) {
                        Ok(account) => {
                            info!(
                                "[{addr}] login: '{}' (id={}, level={})",
                                account.username, account.id, account.access_level,
                            );
                            *authenticated_account = Some(account);
                        }
                        Err(e) => {
                            // For demo servers we log but still let them in.
                            // A real server would send AccountLoginReject here.
                            info!("[{addr}] auth note for '{}': {e}", &*login.account);
                        }
                    }
                }
                let list = GameServerList::new(
                    0xCC,
                    vec![GameServerEntry {
                        index: 0,
                        name: self.server_name.clone().into(),
                        full_percent: 0,
                        timezone: 0,
                        ip: *self.server_addr.ip(),
                    }],
                );
                Some(vec![RawPacket::s2c(encode_packet(&list))])
            }

            0xA0 => {
                // Use the authenticated account if available, otherwise
                // fall back to a default Player-level account.
                let session_account = authenticated_account.take().unwrap_or(DemoAccount {
                    id: 0,
                    username: String::new(),
                    password: String::new(),
                    status: AccountStatus::Active,
                    access_level: AccessLevel::Player,
                });

                // Create a session token via the session manager.
                let token = self
                    .session_manager
                    .create_session(&session_account)
                    .unwrap();

                // Use the version and encryption state that were detected
                // during the login phase (from the extended seed or by
                // brute-force).  This ensures the game-phase detector and
                // codec use the real client's version instead of a
                // hard-coded fallback.
                let (client_version, encrypted) = match &ctx.protocol {
                    Protocol::Login(info) => (info.client_version, info.encrypted),
                    _ => (self.version, self.encrypted),
                };

                let auth_key = token.0;
                let pending = PendingConnection {
                    auth_key,
                    client_version,
                    game_server_address: Some(self.server_addr),
                    encrypted,
                    seed_size: 4,
                    created_at: Instant::now(),
                    context: Box::new(()),
                };
                let _ = ctx.binder.register(pending);
                let redirect = ServerRedirect::new(self.server_addr, auth_key);
                Some(vec![RawPacket::s2c(encode_packet(&redirect))])
            }

            _ => None,
        }
    }

    /// Run a login-phase session: recv loop dispatching 0x80/0xA0 packets.
    ///
    /// Returns when the client disconnects, encounters an error, or sends
    /// any non-login packet (which the caller should handle separately).
    pub async fn run_login_session(
        &self,
        session: &mut Session,
        ctx: &ConnectionContext,
    ) -> error::Result<()> {
        let addr = ctx.addr;
        let mut authenticated_account: Option<DemoAccount> = None;
        loop {
            match session.recv().await.event {
                SessionEvent::Seed(_) => {}
                SessionEvent::Packet(p) => {
                    if let Some(packets) = self.handle_login_packet(
                        addr, ctx, &p, &mut authenticated_account,
                    ) {
                        for resp in packets {
                            session.send(resp).await?;
                        }
                    }
                }
                SessionEvent::Stopped | SessionEvent::Disconnected => {
                    info!("[{addr}] disconnected");
                    break;
                }
                SessionEvent::Error(e) => {
                    error!("[{addr}] error: {e}");
                    return Err(NetworkError::Transport(e));
                }
            }
        }
        Ok(())
    }
}
