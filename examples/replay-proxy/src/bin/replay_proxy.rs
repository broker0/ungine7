//! Replay proxy — a UO proxy that records sessions to `.uolog` files and
//! replays them on demand.
//!
//! ## Server list
//!
//! When a client connects to the login port, the proxy intercepts `0xA8
//! GameServerList` from the real server and appends virtual replay entries —
//! one per `.uolog` file found in `--logs-dir`.  The real server entries are
//! kept intact (original indices, original names).
//!
//! ```text
//! [real indices]  Live Server, ...    ← from the real login server
//! [N]   [Replay] 2026-05-02_18-30-00 ← logs/2026-05-02_18-30-00.uolog
//! [N+1] [Replay] 2026-05-02_19-15-42 ← logs/2026-05-02_19-15-42.uolog
//! ...
//! ```
//!
//! ## Offline mode (`--offline`)
//!
//! In offline mode the proxy does **not** connect to a real login server.
//! The server list contains only replay entries.  Record mode is unavailable.
//!
//! ## Record mode
//!
//! Selecting a real server forwards the connection to that server and
//! logs all S→C packets to `<logs-dir>/<timestamp>.uolog`.
//!
//! ## Replay mode
//!
//! Selecting a virtual server plays back the corresponding log file without
//! connecting to any real host.

use replay_proxy::packet_log;
use replay_proxy::record_session;
use replay_proxy::replay_session;
use replay_proxy::server_list::{build_replay_server_list, patch_game_server_list};

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use clap::Parser;
use log::{error, info, warn};

use u_core::PacketDirection;
use protocol::binder::PendingConnection;
use protocol::RawPacket;
use protocol::connector::{ConnectorConfig, connect};
use protocol::packets::login::{AccountLogin, GameServerList, SelectServer};
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{BasicPacket, encode_packet};
use protocol::Protocol;
use protocol::transport::builder::TransportBuilder;
use u_core::ProtocolVersion;

use network::error as fw_error;
use network::handler::HandlerChain;
use network::handler::packet_handler::{HandlerAction, PacketHandler};
use network::listener::{ConnectionContext, ListenerConfig, ListenerHandler, Listener};
use network::session::{Session, SessionEvent, RecvResult};

use common::args::{DataDirArgs, ProxyArgs, Socks5Args, VerbosityArgs};
use common::handlers::SubcommandFilter;
use framework::ecumene::StaticWorldData;
// ── CLI ───────────────────────────────────────────────────────────────────

/// UO replay proxy: record and replay game sessions.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(flatten)]
    proxy: ProxyArgs,

    #[command(flatten)]
    socks5: Socks5Args,

    #[command(flatten)]
    verbosity: VerbosityArgs,

    /// Directory for `.uolog` files (created if it does not exist).
    #[arg(long, default_value = "logs")]
    logs_dir: PathBuf,

    #[command(flatten)]
    data: DataDirArgs,

    /// Offline mode: do not connect to a real login server.
    ///
    /// Only replay entries are shown in the server list.  The `--server`
    /// argument is ignored.
    #[arg(long)]
    offline: bool,
}

// ── Session mode ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum SessionMode {
    Record { real_game_addr: SocketAddrV4 },
    Replay { log_path: PathBuf },
}

// ── ServerListPatcher ─────────────────────────────────────────────────────

/// Intercepts `0xA8 GameServerList` on the S→C path, patches it by appending
/// replay entries, and stores metadata needed by the relay loop:
///
/// - `patched`: shared cell written once when `0xA8` is intercepted; contains
///   the patched bytes, log file list, and first replay index.
#[derive(Debug)]
struct ServerListPatcher {
    proxy_addr: SocketAddrV4,
    logs_dir: PathBuf,
    /// Written once when 0xA8 is intercepted.
    patched: Arc<Mutex<Option<PatchResult>>>,
}

#[derive(Debug, Clone)]
struct PatchResult {
    log_files: Vec<PathBuf>,
    first_replay_index: u16,
}

impl PacketHandler for ServerListPatcher {
    fn name(&self) -> &str {
        "server-list-patcher"
    }

    fn handle(&mut self, _dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if packet.id() != <GameServerList as BasicPacket>::ID {
            return HandlerAction::Forward(packet);
        }

        match patch_game_server_list(&packet.data, self.proxy_addr, &self.logs_dir) {
            Some(pl) => {
                let patched_bytes = pl.bytes.clone();
                *self.patched.lock().unwrap() = Some(PatchResult {
                    log_files: pl.log_files,
                    first_replay_index: pl.first_replay_index,
                });
                HandlerAction::Replace(vec![RawPacket::s2c(patched_bytes)])
            }
            None => {
                error!("[patcher] failed to parse 0xA8 — forwarding original");
                HandlerAction::Forward(packet)
            }
        }
    }
}

// ── ReplayProxy ───────────────────────────────────────────────────────────

struct ReplayProxy {
    proxy_addr: SocketAddrV4,
    server_addr: String,
    version: ProtocolVersion,
    encrypted: bool,
    logs_dir: PathBuf,
    connector: ConnectorConfig,
    offline: bool,
    chosen: Arc<Mutex<HashMap<SocketAddr, SessionMode>>>,
    /// Global read-only world data (loaded once from `--data-dir`).
    static_data: Option<Arc<StaticWorldData>>,
}

impl ReplayProxy {
    fn new(
        proxy_addr: SocketAddrV4,
        server_addr: impl Into<String>,
        version: ProtocolVersion,
        encrypted: bool,
        logs_dir: PathBuf,
        connector: ConnectorConfig,
        offline: bool,
        static_data: Option<Arc<StaticWorldData>>,
    ) -> Self {
        Self {
            proxy_addr,
            server_addr: server_addr.into(),
            version,
            encrypted,
            logs_dir,
            connector,
            offline,
            chosen: Arc::new(Mutex::new(HashMap::new())),
            static_data,
        }
    }
}

#[async_trait]
impl ListenerHandler for ReplayProxy {
    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut client_session: Session,
    ) -> fw_error::Result<()> {
        match &ctx.protocol {
            Protocol::Login(_) if self.offline => {
                self.handle_login_offline(ctx, &mut client_session).await
            }
            Protocol::Login(_) => self.handle_login(ctx, &mut client_session).await,
            Protocol::Game(_) => self.handle_game(ctx, client_session).await,
        }
    }

    async fn on_disconnect(&self, addr: SocketAddr, _result: &fw_error::Result<()>) {
        self.chosen.lock().unwrap().remove(&addr);
    }
}

impl ReplayProxy {
    // ── Login phase ───────────────────────────────────────────────────────

    async fn handle_login(
        &self,
        ctx: &ConnectionContext,
        client_session: &mut Session,
    ) -> fw_error::Result<()> {
        // Connect to the real login server.
        let server_stream = connect(&self.connector, &self.server_addr).await?;
        let (transport, direction) =
            TransportBuilder::client(server_stream, &ctx.protocol).build()?;

        // ServerListPatcher intercepts 0xA8 and writes patch metadata here.
        let patched_cell: Arc<Mutex<Option<PatchResult>>> = Arc::new(Mutex::new(None));
        let mut server_inbound = HandlerChain::new();
        server_inbound.add(Box::new(SubcommandFilter));
        server_inbound.add(Box::new(ServerListPatcher {
            proxy_addr: self.proxy_addr,
            logs_dir: self.logs_dir.clone(),
            patched: patched_cell.clone(),
        }));
        let mut server_session =
            Session::with_handlers(transport, direction, server_inbound, HandlerChain::new());

        let seed_size = ctx.protocol.seed_size();

        let result: fw_error::Result<()> = async {
            loop {
                tokio::select! {
                    // ── Client → Server ───────────────────────────────────
                    recv = client_session.recv() => {
                        let RecvResult { event, replies } = recv;
                        for reply in replies {
                            client_session.send(reply).await?;
                        }
                        match event {
                        SessionEvent::Seed(data) => {
                            server_session.send_seed(data).await?;
                        }
                        SessionEvent::Packet(p) if p.id() == <SelectServer as BasicPacket>::ID => {
                            let patch = patched_cell.lock().unwrap().clone();
                            match self.handle_select_server(
                                p,
                                patch.as_ref(),
                                ctx,
                                seed_size,
                                client_session,
                            ).await? {
                                SelectResult::ForwardToServer(raw) => {
                                    server_session.send(raw).await?;
                                    info!("[login] 0xA0 forwarded to real server");
                                }
                                SelectResult::ReplayChosen => break,
                            }
                        }
                        SessionEvent::Packet(p) => {
                            server_session.send(p).await?;
                        }
                        SessionEvent::Stopped | SessionEvent::Disconnected => break,
                        SessionEvent::Error(e) => return Err(e.into()),
                    }},

                    // ── Server → Client ───────────────────────────────────
                    recv = server_session.recv() => {
                        let RecvResult { event, replies } = recv;
                        for reply in replies {
                            server_session.send(reply).await?;
                        }
                        match event {
                        SessionEvent::Seed(data) => {
                            client_session.send_seed(data).await?;
                        }
                        SessionEvent::Packet(p) if p.id() == <ServerRedirect as BasicPacket>::ID => {
                            match ServerRedirect::from_bytes(&p.data) {
                                Ok(redirect) => {
                                    let real_game_addr = redirect.address();
                                    info!("[login] real game server: {real_game_addr}");

                                    self.chosen.lock().unwrap().insert(
                                        ctx.addr,
                                        SessionMode::Record { real_game_addr },
                                    );

                                    if let Err(e) = ctx.binder.register(PendingConnection {
                                        auth_key: redirect.auth_key,
                                        client_version: self.version,
                                        game_server_address: Some(real_game_addr),
                                        encrypted: self.encrypted,
                                        seed_size,
                                        created_at: Instant::now(),
                                        context: Box::new(()),
                                    }) {
                                        error!("[login] binder register failed: {e}");
                                        return Ok(());
                                    }

                                    let rewritten = redirect.with_address(self.proxy_addr);
                                    client_session
                                        .send(RawPacket::s2c(encode_packet(&rewritten)))
                                        .await?;
                                    break;
                                }
                                Err(e) => {
                                    error!("[login] failed to parse 0x8C: {e}");
                                    client_session.send(p).await?;
                                }
                            }
                        }
                        SessionEvent::Packet(p) => {
                            client_session.send(p).await?;
                        }
                        SessionEvent::Stopped | SessionEvent::Disconnected => break,
                        SessionEvent::Error(e) => return Err(e.into()),
                    }},
                }
            }
            Ok(())
        }
        .await;

        client_session.close().await;
        server_session.close().await;
        result
    }

    // ── Offline login phase ───────────────────────────────────────────────

    /// Handle login without connecting to a real server.
    ///
    /// Waits for `0x80 AccountLogin`, responds with a replay-only server
    /// list, then handles `0xA0 SelectServer` locally.
    async fn handle_login_offline(
        &self,
        ctx: &ConnectionContext,
        client_session: &mut Session,
    ) -> fw_error::Result<()> {
        let seed_size = ctx.protocol.seed_size();

        // Build the replay-only server list up front.
        let patch = match build_replay_server_list(self.proxy_addr, &self.logs_dir) {
            Some(pl) => {
                info!("[offline] {} replay entries available", pl.log_files.len());
                pl
            }
            None => {
                warn!(
                    "[offline] no .uolog files found in {}",
                    self.logs_dir.display()
                );
                client_session.close().await;
                return Ok(());
            }
        };

        let result: fw_error::Result<()> = async {
            loop {
                let RecvResult { event, replies } = client_session.recv().await;
                for reply in replies {
                    client_session.send(reply).await?;
                }
                match event {
                    SessionEvent::Seed(_) => {
                        // Consume the seed — nothing to forward in offline mode.
                    }
                    SessionEvent::Packet(p) if p.id() == <AccountLogin as BasicPacket>::ID => {
                        if let Ok(login) = AccountLogin::from_bytes(&p.data) {
                            info!("[offline] account login: '{}'", &*login.account);
                        }
                        // Send the replay-only server list.
                        client_session
                            .send(RawPacket::s2c(patch.bytes.clone()))
                            .await?;
                    }
                    SessionEvent::Packet(p) if p.id() == <SelectServer as BasicPacket>::ID => {
                        match self
                            .handle_select_server(
                                p,
                                Some(&PatchResult {
                                    log_files: patch.log_files.clone(),
                                    first_replay_index: patch.first_replay_index,
                                }),
                                ctx,
                                seed_size,
                                client_session,
                            )
                            .await?
                        {
                            SelectResult::ReplayChosen => break,
                            SelectResult::ForwardToServer(_) => {
                                // Should never happen — all entries are replays.
                                warn!("[offline] client selected a non-replay entry, ignoring");
                            }
                        }
                    }
                    SessionEvent::Packet(p) => {
                        log::debug!("[offline] ignoring packet 0x{:02X}", p.id());
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

    /// Handle `0xA0 SelectServer`.
    ///
    /// `patch` is `None` if `0xA8` hasn't been intercepted yet (shouldn't
    /// happen in practice).
    async fn handle_select_server(
        &self,
        packet: RawPacket,
        patch: Option<&PatchResult>,
        ctx: &ConnectionContext,
        seed_size: usize,
        client_session: &mut Session,
    ) -> fw_error::Result<SelectResult> {
        let sel = match SelectServer::from_bytes(&packet.data) {
            Ok(s) => s,
            Err(e) => {
                error!("[login] failed to parse 0xA0: {e}");
                return Ok(SelectResult::ForwardToServer(packet));
            }
        };

        let idx = sel.index as u16;

        // If we have patch metadata and the index is a replay slot — handle locally.
        if let Some(p) = patch {
            if idx >= p.first_replay_index {
                let log_idx = (idx - p.first_replay_index) as usize;
                if let Some(log_path) = p.log_files.get(log_idx) {
                    let log_path = log_path.clone();
                    info!("[login] client selected replay: {}", log_path.display());

                    let fake_auth_key: u32 = 0xFA4E_0000 | (log_idx as u32 & 0xFF);
                    let redirect = ServerRedirect::new(self.proxy_addr, fake_auth_key);

                    if let Err(e) = ctx.binder.register(PendingConnection {
                        auth_key: fake_auth_key,
                        client_version: self.version,
                        game_server_address: None,
                        encrypted: self.encrypted,
                        seed_size,
                        created_at: Instant::now(),
                        context: Box::new(log_path.clone()),
                    }) {
                        error!("[login] binder register (replay) failed: {e}");
                    }

                    self.chosen
                        .lock()
                        .unwrap()
                        .insert(ctx.addr, SessionMode::Replay { log_path });

                    client_session
                        .send(RawPacket::s2c(encode_packet(&redirect)))
                        .await?;

                    return Ok(SelectResult::ReplayChosen);
                }
            }
        }

        // Real server — forward the original packet unchanged (real index preserved).
        info!("[login] client selected real server (index {idx})");
        Ok(SelectResult::ForwardToServer(packet))
    }

    // ── Game phase ────────────────────────────────────────────────────────

    async fn handle_game(
        &self,
        ctx: &ConnectionContext,
        client_session: Session,
    ) -> fw_error::Result<()> {
        let mode = self.determine_mode(ctx);

        match mode {
            SessionMode::Record { real_game_addr } => {
                info!("[game] record → {real_game_addr}");
                let target = format!("{}:{}", real_game_addr.ip(), real_game_addr.port());
                let server_stream = connect(&self.connector, &target).await?;
                let (transport, direction) =
                    TransportBuilder::client(server_stream, &ctx.protocol).build()?;
                let mut server_inbound = HandlerChain::new();
                server_inbound.add(Box::new(SubcommandFilter));
                let server_session = Session::with_handlers(
                    transport,
                    direction,
                    server_inbound,
                    HandlerChain::new(),
                );
                record_session::run(client_session, server_session, self.logs_dir.clone()).await
            }
            SessionMode::Replay { log_path } => {
                info!("[game] replay ← {}", log_path.display());
                replay_session::run(client_session, &log_path, self.static_data.clone()).await
            }
        }
    }

    fn determine_mode(&self, ctx: &ConnectionContext) -> SessionMode {
        // Record mode: framework resolved the real game server address from the binder.
        if let Some(real_addr) = ctx.game_server_address {
            return SessionMode::Record {
                real_game_addr: real_addr,
            };
        }

        // Replay mode: log_path was stored in PendingConnection::context during
        // handle_select_server and carried over into BoundConnection by the framework.
        if let Some(bound) = &ctx.bound_connection {
            if let Some(log_path) = bound.context.downcast_ref::<PathBuf>() {
                return SessionMode::Replay {
                    log_path: log_path.clone(),
                };
            }
        }

        // Fallback: try the chosen map (may already be cleaned up by on_disconnect).
        {
            let guard = self.chosen.lock().unwrap();
            if let Some(SessionMode::Replay { log_path }) = guard.get(&ctx.addr) {
                return SessionMode::Replay {
                    log_path: log_path.clone(),
                };
            }
        }

        warn!("[game] could not determine session mode, falling back to newest log");
        let log_path = packet_log::scan_log_files(&self.logs_dir)
            .ok()
            .and_then(|mut v| {
                v.sort();
                v.into_iter().last()
            })
            .unwrap_or_else(|| self.logs_dir.join("unknown.uolog"));

        SessionMode::Replay { log_path }
    }
}

// ── SelectResult ──────────────────────────────────────────────────────────

enum SelectResult {
    ForwardToServer(RawPacket),
    ReplayChosen,
}

// ── main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    args.verbosity.logger(["replay_proxy"]).build()?;

    std::fs::create_dir_all(&args.logs_dir)?;

    // ── Load static world data (optional) ─────────────────────────────────
    let static_data: Option<Arc<StaticWorldData>> = match args.data.path() {
        Some(dir) => match StaticWorldData::load(dir) {
            Ok(sd) => {
                info!("World data: loaded from {}", dir.display());
                Some(Arc::new(sd))
            }
            Err(e) => {
                warn!("World data: failed to load from {}: {e}", dir.display());
                None
            }
        },
        None => {
            info!("World data: disabled (--no-data)");
            None
        }
    };

    args.proxy.log_info();
    info!("Logs dir:   {}", args.logs_dir.display());

    let connector = args.socks5.into_connector();
    let proxy_addr = args.proxy.proxy_addr();

    let config = ListenerConfig::new(format!("0.0.0.0:{}", args.proxy.proxy_port)).with_allowed(
        vec![(Some(args.proxy.client_version), Some(args.proxy.encrypted))],
    );

    let handler = ReplayProxy::new(
        proxy_addr,
        &args.proxy.server,
        args.proxy.client_version,
        args.proxy.encrypted,
        args.logs_dir,
        connector,
        args.offline,
        static_data,
    );

    if args.offline {
        info!("Mode:       offline (replay only)");
    }
    info!(
        "UO replay proxy listening on 0.0.0.0:{}",
        args.proxy.proxy_port
    );
    Listener::new(config, handler).run().await?;
    Ok(())
}
