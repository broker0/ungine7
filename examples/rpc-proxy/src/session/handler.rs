use std::net::SocketAddrV4;
use std::time::Instant;

use async_trait::async_trait;
use log::{error, info, warn};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use common::handlers::SubcommandFilter;
use network::handler::redirect::RedirectHandler;
use network::handler::HandlerChain;
use network::listener::{
    ConnectionContext, ListenerConfig, ListenerHandler, SessionPhase, Listener,
};
use network::relay;
use network::session::{Session, SessionEvent};
use network::error as fw_error;
use protocol::binder::PendingConnection;
use protocol::RawPacket;
use protocol::packets::login::{AccountLogin, GameServerEntry, GameServerList, SelectServer};
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{BasicPacket, encode_packet};
use protocol::Protocol;
use protocol::transport::builder::TransportBuilder;

use crate::config::Config;
use crate::registry::SharedSessionRegistry;
use crate::session::headless::{LoginRelayC2S, LoginRelayS2C, run_headless_session};
use crate::session::virtual_client::{LoginMode, run_virtual_client};
use crate::types::{ClientRole, SessionId};

// ── ManagedSessionHandler ─────────────────────────────────────────────────

/// Unified session handler used by both listener ports.
///
/// - Port 2593 (`is_mirror = false`) -> **Source** client (full two-phase
///   UO login relay).
/// - Port 2594 (`is_mirror = true`)  -> **Mirror** client.
pub struct ManagedSessionHandler {
    registry: SharedSessionRegistry,
    role: ClientRole,
    /// Address written into the rewritten `0x8C` packet — must be the
    /// externally reachable IP:port of *this* proxy.
    proxy_addr: SocketAddrV4,
    /// Upstream UO login-server address ("ip:port").
    server_addr: String,
    /// Initial Lua script to run when a Source session is created.
    #[cfg(feature = "lua")]
    lua_script: Option<std::path::PathBuf>,
}

impl ManagedSessionHandler {
    pub fn new(
        registry: SharedSessionRegistry,
        is_mirror: bool,
        proxy_addr: SocketAddrV4,
        server_addr: String,
        #[cfg(feature = "lua")] lua_script: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            registry,
            role: if is_mirror { ClientRole::Mirror } else { ClientRole::Source },
            proxy_addr,
            server_addr,
            #[cfg(feature = "lua")]
            lua_script,
        }
    }
}

// ── ListenerHandler impl ──────────────────────────────────────────────────

#[async_trait]
impl ListenerHandler for ManagedSessionHandler {
    /// Attach `RedirectHandler` to the **server-side** (`LoginServer`) chain.
    fn configure_handlers(
        &self,
        phase: SessionPhase,
        ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        let mut inbound = HandlerChain::new();
        let outbound = HandlerChain::new();

        if phase == SessionPhase::LoginServer {
            let (client_version, encrypted, seed_size) = match &ctx.protocol {
                Protocol::Login(info) => (info.client_version, info.encrypted, info.seed_size),
                Protocol::Game(info)  => (info.client_version, info.encrypted, info.seed_size),
            };

            inbound.add(Box::new(RedirectHandler::new(
                self.proxy_addr,
                ctx.binder.clone(),
                client_version,
                encrypted,
                seed_size,
            )));
        }

        (inbound, outbound)
    }

    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut client_session: Session,
    ) -> fw_error::Result<()> {
        let addr = ctx.addr;

        match (&ctx.protocol, self.role) {
            // ── Source: Login phase ───────────────────────────────────────
            (Protocol::Login(_), ClientRole::Source) => {
                info!("[{}] login phase -> relaying to {}", addr, self.server_addr);

                let upstream = TcpStream::connect(&self.server_addr).await.map_err(|e| {
                    error!("[{}] cannot connect to upstream login server {}: {}", addr, self.server_addr, e);
                    fw_error::NetworkError::Io(e)
                })?;

                let (transport, direction) =
                    TransportBuilder::client(upstream, &ctx.protocol).build()?;

                let (inbound, outbound) =
                    self.configure_handlers(SessionPhase::LoginServer, ctx);

                let mut server_session =
                    Session::with_handlers(transport, direction, inbound, outbound);

                relay::relay(
                    &format!("[login {}]", addr),
                    &mut client_session,
                    &mut server_session,
                    None,
                )
                .await
            }

            // ── Mirror: Login phase ──────────────────────────────────────
            (Protocol::Login(_), ClientRole::Mirror) => {
                self.handle_mirror_login(ctx, &mut client_session).await
            }

            // ── Source: Game phase ────────────────────────────────────────
            // Creates a HeadlessSession (owns the upstream server connection)
            // and runs a VirtualClient with InitiateSession login mode.
            (Protocol::Game(_), ClientRole::Source) => {
                let game_addr = match ctx.game_server_address {
                    Some(a) => a,
                    None => {
                        warn!(
                            "[{}] game phase but no upstream address in binder — dropping",
                            addr
                        );
                        return Ok(());
                    }
                };

                let game_addr_str = format!("{}:{}", game_addr.ip(), game_addr.port());
                info!("[{}] game phase -> connecting to {}", addr, game_addr_str);

                let upstream = TcpStream::connect(&game_addr_str).await.map_err(|e| {
                    error!("[{}] cannot connect to upstream game server {}: {}", addr, game_addr_str, e);
                    fw_error::NetworkError::Io(e)
                })?;

                let (transport, direction) =
                    TransportBuilder::client(upstream, &ctx.protocol).build()?;

                let mut server_inbound = HandlerChain::new();
                server_inbound.add(Box::new(SubcommandFilter));
                let server_session = Session::with_handlers(
                    transport,
                    direction,
                    server_inbound,
                    HandlerChain::new(),
                );

                // Register a new session.
                let (session_id, entry, command_rx) = {
                    let mut reg = self.registry.write().await;
                    reg.register_or_join(addr, ClientRole::Source)
                };

                // Store the upstream game-server address.
                *entry.game_server_address.write().await = Some(game_addr);
                info!("[{}] session_id={} role=Source", addr, session_id.0);

                // Create login relay channels.
                let (login_c2s_tx, login_c2s_rx) = mpsc::channel::<LoginRelayC2S>(16);
                let (login_s2c_tx, login_s2c_rx) = mpsc::channel::<LoginRelayS2C>(16);

                // Spawn HeadlessSession as a background task.
                let headless_entry = entry.clone();
                let headless_session_id = session_id;
                tokio::spawn(async move {
                    run_headless_session(
                        server_session,
                        command_rx.expect("Source always has command_rx"),
                        headless_entry,
                        headless_session_id,
                        login_c2s_rx,
                        login_s2c_tx,
                    )
                    .await;
                });

                // Spawn Lua script manager (if feature enabled).
                #[cfg(feature = "lua")]
                {
                    let lua_entry = entry.clone();
                    let lua_initial = self.lua_script.clone();
                    tokio::spawn(async move {
                        let lua_cmd_rx = lua_entry
                            .lua_cmd_rx
                            .lock()
                            .await
                            .take()
                            .expect("lua_cmd_rx taken only once");
                        crate::lua_script::run_lua_manager(
                            lua_entry.command_tx.clone(),
                            lua_entry.event_tx.clone(),
                            lua_cmd_rx,
                            lua_initial,
                        )
                        .await;
                    });
                }

                // Run VirtualClient with InitiateSession mode.
                let result = run_virtual_client(
                    client_session,
                    entry.clone(),
                    session_id,
                    LoginMode::InitiateSession {
                        login_relay_tx: login_c2s_tx,
                        login_relay_rx: login_s2c_rx,
                    },
                )
                .await;

                // Cleanup.
                self.registry.write().await.deactivate(session_id);
                info!(
                    "[session {}] Source disconnected — session deactivated",
                    session_id.0
                );

                result
            }

            // ── Mirror: Game phase ───────────────────────────────────────
            (Protocol::Game(_), ClientRole::Mirror) => {
                // Retrieve the target SessionId from the binder context.
                let target_session_id = ctx
                    .bound_connection
                    .as_ref()
                    .and_then(|b| b.context.downcast_ref::<SessionId>().copied());

                let target_session_id = match target_session_id {
                    Some(id) => id,
                    None => {
                        warn!(
                            "[{}] mirror game phase but no session id in binder context — dropping",
                            addr
                        );
                        return Ok(());
                    }
                };

                // Look up the session entry.
                let entry = {
                    let reg = self.registry.read().await;
                    reg.get(target_session_id)
                };

                let entry = match entry {
                    Some(e) if e.is_active => e,
                    _ => {
                        warn!(
                            "[{}] mirror game phase: session {} not found or inactive — dropping",
                            addr, target_session_id.0
                        );
                        return Ok(());
                    }
                };

                info!(
                    "[{}] mirror game phase -> session {} (JoinExisting)",
                    addr, target_session_id.0
                );

                // Run VirtualClient with JoinExisting mode.
                let result = run_virtual_client(
                    client_session,
                    entry.clone(),
                    target_session_id,
                    LoginMode::JoinExisting,
                )
                .await;

                result
            }
        }
    }
}

// ── Mirror login helpers ──────────────────────────────────────────────────

impl ManagedSessionHandler {
    /// Handle login phase for a Mirror client without connecting to the real
    /// login server.
    async fn handle_mirror_login(
        &self,
        ctx: &ConnectionContext,
        client_session: &mut Session,
    ) -> fw_error::Result<()> {
        let addr = ctx.addr;
        let seed_size = ctx.protocol.seed_size();
        let (client_version, encrypted) = match &ctx.protocol {
            Protocol::Login(info) => (info.client_version, info.encrypted),
            Protocol::Game(info) => (info.client_version, info.encrypted),
        };

        info!("[{}] mirror login phase — serving session list", addr);

        // Collect active sessions for the server list.
        let sessions: Vec<(SessionId, String)> = {
            let reg = self.registry.read().await;
            reg.active_sessions()
                .map(|e| {
                    let label = format!("Session {}", e.id.0);
                    (e.id, label)
                })
                .collect()
        };

        if sessions.is_empty() {
            warn!("[{}] mirror login: no active sessions — dropping", addr);
            client_session.close().await;
            return Ok(());
        }

        let result: fw_error::Result<()> = async {
            loop {
                match client_session.recv().await.event {
                    SessionEvent::Seed(_) => {
                        // Consume the seed — nothing to forward (no upstream).
                    }
                    SessionEvent::Packet(p)
                        if p.id() == AccountLogin::ID =>
                    {
                        if let Ok(login) = AccountLogin::from_bytes(&p.data) {
                            info!(
                                "[{}] mirror login: account '{}'",
                                addr, &*login.account
                            );
                        }

                        // Build synthetic server list from active sessions.
                        let entries: Vec<GameServerEntry> = sessions
                            .iter()
                            .enumerate()
                            .map(|(i, (_sid, label))| GameServerEntry {
                                index: i as u16,
                                name: make_name(label),
                                full_percent: 0,
                                timezone: 0,
                                ip: *self.proxy_addr.ip(),
                            })
                            .collect();

                        let server_list = GameServerList::new(0x5D, entries);
                        client_session
                            .send(RawPacket::s2c(encode_packet(&server_list)))
                            .await?;
                    }
                    SessionEvent::Packet(p)
                        if p.id() == SelectServer::ID =>
                    {
                        let sel = match SelectServer::from_bytes(&p.data) {
                            Ok(s) => s,
                            Err(e) => {
                                error!(
                                    "[{}] mirror login: failed to parse 0xA0: {}",
                                    addr, e
                                );
                                continue;
                            }
                        };

                        let idx = sel.index as usize;
                        let (target_session_id, label) = match sessions.get(idx) {
                            Some(s) => s,
                            None => {
                                warn!(
                                    "[{}] mirror login: invalid server index {}",
                                    addr, idx
                                );
                                continue;
                            }
                        };

                        info!(
                            "[{}] mirror login: selected '{}' (session {})",
                            addr, label, target_session_id.0
                        );

                        // Look up the session to verify it's still active.
                        let entry = {
                            let reg = self.registry.read().await;
                            reg.get(*target_session_id)
                        };
                        let _entry = match entry {
                            Some(e) if e.is_active => e,
                            _ => {
                                warn!(
                                    "[{}] mirror login: session {} no longer active",
                                    addr, target_session_id.0
                                );
                                continue;
                            }
                        };

                        // Generate a unique auth_key for this mirror connection.
                        let fake_auth_key: u32 =
                            0xFACE_0000 | (target_session_id.0 as u32 & 0xFFFF);

                        // Register with the binder.
                        if let Err(e) = ctx.binder.register(PendingConnection {
                            auth_key: fake_auth_key,
                            client_version,
                            game_server_address: None,
                            encrypted,
                            seed_size,
                            created_at: Instant::now(),
                            context: Box::new(*target_session_id),
                        }) {
                            error!(
                                "[{}] mirror login: binder register failed: {}",
                                addr, e
                            );
                            return Ok(());
                        }

                        // Send 0x8C redirect pointing at the mirror port.
                        let redirect =
                            ServerRedirect::new(self.proxy_addr, fake_auth_key);
                        client_session
                            .send(RawPacket::s2c(encode_packet(&redirect)))
                            .await?;

                        break;
                    }
                    SessionEvent::Packet(p) => {
                        log::debug!(
                            "[{}] mirror login: ignoring packet 0x{:02X}",
                            addr,
                            p.id()
                        );
                    }
                    SessionEvent::Stopped | SessionEvent::Disconnected => break,
                    SessionEvent::Error(e) => return Err(e.into()),
                }
            }
            Ok(())
        }
        .await;

        client_session.close().await;
        result
    }
}

/// Convert a display name into a 32-byte null-padded ASCII field.
fn make_name(name: &str) -> packets::u_io::RawBytes<32> {
    let mut buf = [0u8; 32];
    for (dst, src) in buf.iter_mut().zip(name.bytes().take(31)) {
        *dst = src;
    }
    packets::u_io::RawBytes(buf)
}

// ── start_listeners ───────────────────────────────────────────────────────

/// Bind both listeners concurrently:
/// - Source on `config.proxy.proxy_port`
/// - Mirror on `config.mirror_port`
pub async fn start_listeners(
    config: &Config,
    registry: SharedSessionRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = config.proxy.server.clone();
    let proxy_addr  = config.proxy.proxy_addr();

    let source_config = ListenerConfig::new(format!("0.0.0.0:{}", config.proxy.proxy_port))
        .with_allowed(config.allowed());
    let source_handler = ManagedSessionHandler::new(
        registry.clone(),
        false,
        proxy_addr,
        server_addr.clone(),
        #[cfg(feature = "lua")]
        config.lua_script.clone(),
    );
    let source_listener = Listener::new(source_config, source_handler);

    let mirror_config = ListenerConfig::new(format!("0.0.0.0:{}", config.mirror_port))
        .with_allowed(config.allowed());
    let mirror_addr = SocketAddrV4::new(*proxy_addr.ip(), config.mirror_port);
    let mirror_handler = ManagedSessionHandler::new(
        registry.clone(),
        true,
        mirror_addr,
        server_addr,
        #[cfg(feature = "lua")]
        None,
    );
    let mirror_listener = Listener::new(mirror_config, mirror_handler);

    info!("Source listener on :{}", config.proxy.proxy_port);
    info!("Mirror listener on :{}", config.mirror_port);

    tokio::try_join!(source_listener.run(), mirror_listener.run())?;

    Ok(())
}
