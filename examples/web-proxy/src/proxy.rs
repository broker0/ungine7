//! Core proxy logic extracted from the original `main.rs`.
//!
//! Contains:
//! - [`ProxyConfig`] — settings for a single proxy instance (no web server).
//! - [`run_proxy`]   — starts the UO proxy listener only.
//! - [`WebProxy`]    — the [`ListenerHandler`] implementation.
//! - [`LinkingRedirectHandler`] — redirects 0x8C with session linking.

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::Duration;

use async_trait::async_trait;
use log::{error, info};

use u_core::ProtocolVersion;
use protocol::transport::builder::{TransportBuilder, TransportBuildError};
use protocol::transport::tcp::TcpByteStream;
use protocol::transport::PacketTransport;
use protocol::Protocol;
use protocol::binder::{ConnectionBinder, PendingConnection};
use protocol::RawPacket;
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{encode_packet, BasicPacket};

use network::error;
use network::handler::HandlerChain;
use network::listener::{
    ConnectionContext, ListenerConfig, ListenerControl, ListenerHandler, SessionPhase, Listener,
};
use network::session::{Session, SessionEvent, RecvResult};
use network::handler::packet_handler::{HandlerAction, PacketHandler};

use u_core::PacketDirection;

use common::handlers::SubcommandFilter;
use protocol::codec::encryption;
use protocol::connector::{connect, ConnectorConfig};
use protocol::packets::seed::ExtendedSeed;
use protocol::transport::crypto_stream::CryptoStream;

use crate::logging_stream::LoggingStream;
use crate::packet_observer::PacketObserver;
use crate::session_registry::{
    now_ms, ConnectionEntry, ConnectionEvent, PacketEntry, RawStage,
    SessionId, SessionRegistry, SharedRegistry, SocketRole,
};

// ── ProxyConfig ──────────────────────────────────────────────────────────

/// Settings for a single UO proxy instance.
///
/// Does **not** include web server or tick parameters — those are managed
/// at the application level since a single web server serves all instances.
pub struct ProxyConfig {
    pub proxy_addr: SocketAddrV4,
    pub server: String,
    pub listen_addr: String,
    pub raw_log: bool,
    pub connector: ConnectorConfig,
    pub client_version: ProtocolVersion,
    pub encrypted: bool,
}

// ── run_proxy ────────────────────────────────────────────────────────────

/// Start a UO proxy listener.
///
/// The caller is responsible for starting the web UI server and the
/// periodic sessions-broadcast tick — they are shared across all proxy
/// instances running in the same process.
///
/// Blocks until `control_rx` receives [`ListenerControl::Shutdown`]
/// (or the sender is dropped).
pub async fn run_proxy(
    config: ProxyConfig,
    registry: SharedRegistry,
    control_rx: mpsc::Receiver<ListenerControl>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("UO proxy listening on {}", config.listen_addr);
    info!(
        "Instance config: server={} proxy_addr={} client_version={} encrypted={} raw_log={} connector={:?}",
        config.server,
        config.proxy_addr,
        config.client_version,
        config.encrypted,
        config.raw_log,
        config.connector,
    );
    if config.raw_log {
        info!("Raw byte-level logging: enabled");
    }

    let listener_config = ListenerConfig::new(&config.listen_addr)
        .with_allowed(vec![
            (Some(config.client_version), Some(config.encrypted)),
        ]);

    let handler = WebProxy::new(
        config.proxy_addr,
        &config.server,
        registry,
        config.connector,
        config.raw_log,
    );

    Listener::new(listener_config, handler)
        .run_with_control(control_rx)
        .await?;

    Ok(())
}

// ── LinkingRedirectHandler ────────────────────────────────────────────────

/// Like `RedirectHandler` from the framework, but stores the current `SessionId`
/// in `PendingConnection::context` so the subsequent Game TCP connection can
/// retrieve it and reuse the same `SessionRegistry` entry.
#[derive(Debug)]
struct LinkingRedirectHandler {
    proxy_address: SocketAddrV4,
    binder: ConnectionBinder,
    client_version: ProtocolVersion,
    encrypted: bool,
    seed_size: usize,
    session_id: SessionId,
    addr: SocketAddr,
    pending: PendingMap,
}

impl LinkingRedirectHandler {
    fn new(
        proxy_address: SocketAddrV4,
        binder: ConnectionBinder,
        client_version: ProtocolVersion,
        encrypted: bool,
        seed_size: usize,
        session_id: SessionId,
        addr: SocketAddr,
        pending: PendingMap,
    ) -> Self {
        Self { proxy_address, binder, client_version, encrypted, seed_size, session_id, addr, pending }
    }
}

impl PacketHandler for LinkingRedirectHandler {
    fn name(&self) -> &str { "linking-redirect" }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if packet.id() != <ServerRedirect as BasicPacket>::ID {
            return HandlerAction::Forward(packet);
        }

        let redirect = match ServerRedirect::from_bytes(&packet.data) {
            Ok(r) => r,
            Err(e) => {
                error!("[linking-redirect] failed to parse 0x8C: {e}");
                return HandlerAction::Forward(packet);
            }
        };

        let pending = PendingConnection {
            auth_key: redirect.auth_key,
            client_version: self.client_version,
            game_server_address: Some(redirect.address()),
            encrypted: self.encrypted,
            seed_size: self.seed_size,
            created_at: Instant::now(),
            context: Box::new(self.session_id),
        };

        if let Err(e) = self.binder.register(pending) {
            error!("[linking-redirect] binder register failed: {e}");
            return HandlerAction::Forward(packet);
        }

        // Mark the login session as linked so on_disconnect won't unregister it.
        if let Some(entry) = self.pending.lock().unwrap().get_mut(&self.addr) {
            entry.linked = true;
        }

        let rewritten = redirect.with_address(self.proxy_address);
        HandlerAction::Stop(RawPacket::new(encode_packet(&rewritten), packet.direction))
    }
}

// ── Session tracking ──────────────────────────────────────────────────────

#[derive(Debug)]
struct PendingEntry {
    session_id: SessionId,
    /// Whether raw byte-level logging is enabled for this session.
    raw_log: bool,
    /// `true` for Game-phase connections that reused a Login session.
    is_continuation: bool,
    /// `true` once 0x8C was successfully processed — Login TCP may close safely.
    linked: bool,
}

type PendingMap = Arc<Mutex<HashMap<SocketAddr, PendingEntry>>>;

// ── WebProxy ──────────────────────────────────────────────────────────────

struct WebProxy {
    proxy_addr: SocketAddrV4,
    server_addr: String,
    registry: SharedRegistry,
    pending: PendingMap,
    connector: ConnectorConfig,
    raw_log: bool,
}

impl WebProxy {
    fn new(
        proxy_addr: SocketAddrV4,
        server_addr: impl Into<String>,
        registry: SharedRegistry,
        connector: ConnectorConfig,
        raw_log: bool,
    ) -> Self {
        Self {
            proxy_addr,
            server_addr: server_addr.into(),
            registry,
            pending: Arc::new(Mutex::new(HashMap::new())),
            connector,
            raw_log,
        }
    }
}

#[async_trait]
impl ListenerHandler for WebProxy {
    async fn on_connect(&self, ctx: &ConnectionContext) -> bool {
        match &ctx.protocol {
            Protocol::Login(_) => {
                let id = self.registry.register(ctx.addr, "Login", self.raw_log);
                self.pending.lock().unwrap().insert(ctx.addr, PendingEntry {
                    session_id: id,
                    raw_log: self.raw_log,
                    is_continuation: false,
                    linked: false,
                });
                self.registry.push_connection(id, ConnectionEntry::new(
                    SocketRole::Client, ctx.addr.to_string(), ConnectionEvent::Connected,
                ));
                info!("[web-proxy] login session #{id} from {}", ctx.addr);
            }

            Protocol::Game(_) => {
                // The framework already called binder.bind() and placed the result
                // in ctx.bound_connection — we just downcast the context to get SessionId.
                let (session_id, is_continuation) = match &ctx.bound_connection {
                    Some(b) => match b.context.downcast_ref::<SessionId>() {
                        Some(id) => {
                            let sid = *id;
                            self.registry.set_phase(sid, "Game");
                            info!("[web-proxy] game session #{sid} (continuation) from {}, target: {}",
                                ctx.addr,
                                ctx.game_server_address
                                    .map(|a| format!("{}:{}", a.ip(), a.port()))
                                    .as_deref()
                                    .unwrap_or("unknown"));
                            (sid, true)
                        }
                        None => {
                            let id = self.registry.register(ctx.addr, "Game", self.raw_log);
                            info!("[web-proxy] game session #{id} (no linked login) from {}", ctx.addr);
                            (id, false)
                        }
                    },
                    None => {
                        let id = self.registry.register(ctx.addr, "Game", self.raw_log);
                        info!("[web-proxy] game session #{id} (standalone) from {}", ctx.addr);
                        (id, false)
                    }
                };

                self.pending.lock().unwrap().insert(ctx.addr, PendingEntry {
                    session_id,
                    raw_log: self.raw_log,
                    is_continuation,
                    linked: false,
                });
                self.registry.push_connection(session_id, ConnectionEntry::new(
                    SocketRole::Client, ctx.addr.to_string(), ConnectionEvent::Connected,
                ));
            }
        }
        true
    }

    fn build_transport(
        &self,
        stream: tokio::net::TcpStream,
        ctx: &ConnectionContext,
    ) -> Result<(Box<dyn PacketTransport>, PacketDirection), TransportBuildError> {
        let entry = self.pending.lock().unwrap()
            .get(&ctx.addr)
            .map(|e| (e.session_id, e.raw_log));

        info!("[web-proxy] build_transport for {} → entry={:?}", ctx.addr, entry);

        if let Some((session_id, true)) = entry {
            // Client-facing transport (proxy <-> UO client):
            //   read  = bytes from client  -> "C->S"
            //   write = bytes to client    -> "S->C"
            let tcp = TcpByteStream::new(stream);
            let raw_logged = LoggingStream::new(
                tcp, session_id, self.registry.clone(),
                "C→S", "S→C", RawStage::RawRead, RawStage::RawWrite,
            );

            let role = protocol::protocol::Role::Server;
            let (enc, dec) = encryption::cipher_pair(&ctx.protocol, role);
            let crypto = CryptoStream::new(raw_logged, enc, dec);

            let plain_logged = LoggingStream::new(
                crypto, session_id, self.registry.clone(),
                "C→S", "S→C", RawStage::Decrypted, RawStage::PreEncrypt,
            );

            TransportBuilder::server_with_stream(plain_logged, &ctx.protocol).build()
        } else {
            TransportBuilder::server(stream, &ctx.protocol).build()
        }
    }

    fn configure_handlers(
        &self,
        phase: SessionPhase,
        ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        let mut inbound  = HandlerChain::new();
        let     outbound = HandlerChain::new();

        let session_id = self.pending.lock().unwrap()
            .get(&ctx.addr)
            .map(|e| e.session_id);

        let Some(sid) = session_id else { return (inbound, outbound); };

        inbound.add(Box::new(PacketObserver::new(sid, self.registry.clone())));

        match phase {
            SessionPhase::LoginServer => {
                inbound.add(Box::new(LinkingRedirectHandler::new(
                    self.proxy_addr,
                    ctx.binder.clone(),
                    ctx.protocol.client_version(),
                    ctx.protocol.is_encrypted(),
                    ctx.protocol.seed_size(),
                    sid,
                    ctx.addr,
                    self.pending.clone(),
                )));
            }
            SessionPhase::GameServer => {
                inbound.add(Box::new(SubcommandFilter));
            }
            _ => {}
        }

        (inbound, outbound)
    }

    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut client_session: Session,
    ) -> error::Result<()> {
        let server_phase = match &ctx.protocol {
            Protocol::Login(_) => SessionPhase::LoginServer,
            Protocol::Game(_)  => SessionPhase::GameServer,
        };

        // Use the real game server address from the framework context when
        // available — on OSI login and game servers are on different hosts.
        let target = ctx.game_server_address
            .map(|a| format!("{}:{}", a.ip(), a.port()))
            .unwrap_or_else(|| self.server_addr.clone());

        let server_stream = connect(&self.connector, &target).await?;
        // Log successful connection to the upstream server.
        if let Some(sid) = self.pending.lock().unwrap().get(&ctx.addr).map(|e| e.session_id) {
            self.registry.push_connection(sid, ConnectionEntry::new(
                SocketRole::Server, &target, ConnectionEvent::Connected,
            ));
        }
        let (transport, direction) = {
            let entry = self.pending.lock().unwrap()
                .get(&ctx.addr)
                .map(|e| (e.session_id, e.raw_log));

            info!("[web-proxy] handle_session server transport for {} → entry={:?}", ctx.addr, entry);

            if let Some((session_id, true)) = entry {
                // Server-facing transport (proxy <-> real UO server):
                //   read  = bytes from server  -> "S->C"
                //   write = bytes to server    -> "C->S"
                let tcp = TcpByteStream::new(server_stream);
                let raw_logged = LoggingStream::new(
                    tcp, session_id, self.registry.clone(),
                    "S→C", "C→S", RawStage::RawRead, RawStage::RawWrite,
                );

                let role = protocol::protocol::Role::Client;
                let (enc, dec) = encryption::cipher_pair(&ctx.protocol, role);
                let crypto = CryptoStream::new(raw_logged, enc, dec);

                let plain_logged = LoggingStream::new(
                    crypto, session_id, self.registry.clone(),
                    "S→C", "C→S", RawStage::Decrypted, RawStage::PreEncrypt,
                );

                TransportBuilder::client_with_stream(plain_logged, &ctx.protocol).build()?
            } else {
                TransportBuilder::client(server_stream, &ctx.protocol).build()?
            }
        };

        let (inbound, outbound) = self.configure_handlers(server_phase, ctx);
        let mut server_session = Session::with_handlers(transport, direction, inbound, outbound);

        // Custom relay loop — identical to relay::relay but also logs seed
        // events to the session registry so they appear in the web UI.
        let session_id = self.pending.lock().unwrap()
            .get(&ctx.addr)
            .map(|e| e.session_id);
        let registry = self.registry.clone();

        // Track which side caused the relay loop to exit so we can log
        // disconnect events in the correct causal order (initiator first).
        enum ExitSide { Client, Server }
        let mut exit_side = ExitSide::Client; // default; overwritten below

        let result: error::Result<()> = async {
            loop {
                tokio::select! {
                    recv = client_session.recv() => {
                        let RecvResult { event, replies } = recv;
                        for reply in replies {
                            client_session.send(reply).await?;
                        }
                        match event {
                            SessionEvent::Seed(ref data) => {
                                if let Some(sid) = session_id {
                                    log_seed(&registry, sid, data, PacketDirection::ClientToServer);
                                }
                                server_session.send_seed(data.clone()).await?;
                            }
                            SessionEvent::Packet(p) => { server_session.send(p).await?; }
                            SessionEvent::Stopped | SessionEvent::Disconnected => {
                                exit_side = ExitSide::Client;
                                break;
                            }
                            SessionEvent::Error(e) => {
                                exit_side = ExitSide::Client;
                                return Err(e.into());
                            }
                        }
                    }
                    recv = server_session.recv() => {
                        let RecvResult { event, replies } = recv;
                        for reply in replies {
                            server_session.send(reply).await?;
                        }
                        match event {
                            SessionEvent::Seed(ref data) => {
                                if let Some(sid) = session_id {
                                    log_seed(&registry, sid, data, PacketDirection::ServerToClient);
                                }
                                client_session.send_seed(data.clone()).await?;
                            }
                            SessionEvent::Packet(p) => { client_session.send(p).await?; }
                            SessionEvent::Stopped | SessionEvent::Disconnected => {
                                exit_side = ExitSide::Server;
                                break;
                            }
                            SessionEvent::Error(e) => {
                                exit_side = ExitSide::Server;
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
            Ok(())
        }.await;

        // Log disconnect events: initiating side first, then the other.
        if let Some(sid) = session_id {
            let ok_event  = || ConnectionEvent::Disconnected;
            let err_event = |e: &error::NetworkError| ConnectionEvent::Error { message: e.to_string() };

            let (first_role, first_addr, second_role, second_addr) = match exit_side {
                ExitSide::Client => (SocketRole::Client, ctx.addr.to_string(), SocketRole::Server, target.clone()),
                ExitSide::Server => (SocketRole::Server, target.clone(),       SocketRole::Client, ctx.addr.to_string()),
            };

            registry.push_connection(sid, ConnectionEntry::new(
                first_role, &first_addr,
                match &result { Ok(_) => ok_event(), Err(e) => err_event(e) },
            ));
            registry.push_connection(sid, ConnectionEntry::new(
                second_role, &second_addr,
                match &result { Ok(_) => ok_event(), Err(e) => err_event(e) },
            ));
        }

        client_session.close().await;
        server_session.close().await;
        result
    }

    async fn on_disconnect(&self, addr: SocketAddr, _result: &error::Result<()>) {
        let entry = self.pending.lock().unwrap().remove(&addr);
        let Some(entry) = entry else { return; };

        if entry.is_continuation {
            // Game-phase disconnect — always unregister.
            info!("[web-proxy] session #{} ended (game disconnect from {addr})", entry.session_id);
        } else if !entry.linked {
            // Login-phase disconnect without a 0x8C redirect — unregister too.
            info!("[web-proxy] session #{} ended (login without redirect from {addr})", entry.session_id);
        } else {
            // Login TCP closed after 0x8C — game phase will follow, keep the session.
            info!("[web-proxy] login TCP ended for session #{} from {addr}", entry.session_id);
            return;
        }

        let registry = self.registry.clone();
        let session_id = entry.session_id;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            registry.unregister(session_id);
        });
    }
}

// ── Seed logging ──────────────────────────────────────────────────────────

/// Build a [`PacketEntry`] for a raw seed buffer and push it to the registry.
///
/// - 4 bytes  -> legacy `Seed` (value displayed as `0x????????`)
/// - 21 bytes -> `ExtendedSeed` (0xEF; version fields decoded)
/// - anything else -> shown as raw hex
fn log_seed(
    registry: &SessionRegistry,
    session_id: SessionId,
    data: &[u8],
    dir: PacketDirection,
) {
    let direction_str = match dir {
        PacketDirection::ClientToServer => "C\u{2192}S",
        PacketDirection::ServerToClient => "S\u{2192}C",
    };

    let hex: String = data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");

    let (id_str, desc) = match data.len() {
        4 => {
            let val = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            ("seed".to_string(), format!("Seed value=0x{val:08X}"))
        }
        21 => {
            match ExtendedSeed::from_bytes(data) {
                Ok(s) => {
                    let desc = format!(
                        "ExtendedSeed seed=0x{:08X} version={}.{}.{}.{}",
                        s.seed, s.major, s.minor, s.patch, s.build
                    );
                    ("0xEF".to_string(), desc)
                }
                Err(_) => ("0xEF".to_string(), format!("ExtendedSeed (parse error) {hex}")),
            }
        }
        _ => ("seed".to_string(), format!("Seed (unknown format) {hex}")),
    };

    let entry = PacketEntry {
        timestamp: now_ms(),
        direction: direction_str.to_string(),
        id: id_str,
        len: data.len(),
        desc,
        hex,
    };

    registry.push_packet(session_id, entry);
}
