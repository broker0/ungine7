//! Replay server with web control — the UO client connects and receives
//! the replay stream, but playback is controlled entirely from a web UI.
//!
//! # Architecture
//!
//! ```text
//!   ┌──────────┐         ┌──────────────────────┐         ┌──────────┐
//!   │ Browser  │──WS────▶│    replay-server     │◀──UO───│ UO Client│
//!   │ (web UI) │         │                      │         │          │
//!   └──────────┘         │  Axum Web Server     │         └──────────┘
//!                        │       ↕ channels     │
//!                        │  Login Handler       │
//!                        │  Playback Engine     │
//!                        │  (headless)          │
//!                        └──────────────────────┘
//! ```
//!
//! The client is a passive viewer — it receives packets and renders the world,
//! but all playback commands (pause, seek, step, fast-forward) come from the
//! web UI via WebSocket.
//!
//! # Usage
//!
//! ```text
//! replay-server --logs-dir logs/ --proxy-port 2593 --web-port 8080
//! ```

use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use clap::Parser;
use log::{error, info, warn};

use protocol::binder::PendingConnection;
use protocol::RawPacket;
use protocol::packets::login::{AccountLogin, SelectServer};
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{BasicPacket, encode_packet};
use protocol::Protocol;
use u_core::ProtocolVersion;

use network::error as fw_error;
use network::listener::{ConnectionContext, ListenerConfig, ListenerHandler, Listener};
use network::session::{Session, SessionEvent};

use common::args::{DataDirArgs, ProxyArgs, VerbosityArgs};
use framework::ecumene::StaticWorldData;

use replay_proxy::replay_session;
use replay_proxy::replay_session::playback_headless::{PlaybackCommand, HeadlessChannels};
use replay_proxy::server_list::build_replay_server_list;
use replay_proxy::web::{ReplayAppState, SessionEvent as WebSessionEvent, SharedAppState};

// ── CLI ───────────────────────────────────────────────────────────────────

/// UO replay server with web-based playback control.
///
/// Runs in offline mode only — no real server connection.  The UO client
/// connects, selects a replay from the server list, and playback is
/// controlled entirely from the web UI.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(flatten)]
    proxy: ProxyArgs,

    #[command(flatten)]
    verbosity: VerbosityArgs,

    /// Directory for `.uolog` files.
    #[arg(long, default_value = "logs")]
    logs_dir: PathBuf,

    #[command(flatten)]
    data: DataDirArgs,

    /// Port to serve the web control UI on.
    #[arg(long, default_value_t = 8080)]
    web_port: u16,

    /// Directory containing `replay.html` to serve from disk instead of the
    /// built-in copy (dev mode — re-read on every request).
    #[arg(long)]
    dev_html: Option<PathBuf>,
}

// ── ReplayServerHandler ───────────────────────────────────────────────────

struct ReplayServerHandler {
    proxy_addr: SocketAddrV4,
    version: ProtocolVersion,
    encrypted: bool,
    logs_dir: PathBuf,
    static_data: Option<Arc<StaticWorldData>>,
    app_state: SharedAppState,
}

#[async_trait]
impl ListenerHandler for ReplayServerHandler {
    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut client_session: Session,
    ) -> fw_error::Result<()> {
        match &ctx.protocol {
            Protocol::Login(_) => {
                self.handle_login_offline(ctx, &mut client_session).await
            }
            Protocol::Game(_) => {
                self.handle_game(ctx, client_session).await
            }
        }
    }
}

impl ReplayServerHandler {
    /// Handle login in offline mode — build a replay-only server list.
    async fn handle_login_offline(
        &self,
        ctx: &ConnectionContext,
        client_session: &mut Session,
    ) -> fw_error::Result<()> {
        let seed_size = ctx.protocol.seed_size();

        let patch = match build_replay_server_list(self.proxy_addr, &self.logs_dir) {
            Some(pl) => {
                info!("[login] {} replay entries available", pl.log_files.len());
                pl
            }
            None => {
                warn!(
                    "[login] no .uolog files found in {}",
                    self.logs_dir.display()
                );
                client_session.close().await;
                return Ok(());
            }
        };

        let result: fw_error::Result<()> = async {
            loop {
                match client_session.recv().await.event {
                    SessionEvent::Seed(_) => {}
                    SessionEvent::Packet(p) if p.id() == <AccountLogin as BasicPacket>::ID => {
                        if let Ok(login) = AccountLogin::from_bytes(&p.data) {
                            info!("[login] account login: '{}'", &*login.account);
                        }
                        client_session
                            .send(RawPacket::s2c(patch.bytes.clone()))
                            .await?;
                    }
                    SessionEvent::Packet(p) if p.id() == <SelectServer as BasicPacket>::ID => {
                        if let Ok(sel) = SelectServer::from_bytes(&p.data) {
                            let idx = sel.index as u16;
                            if idx >= patch.first_replay_index {
                                let log_idx = (idx - patch.first_replay_index) as usize;
                                if let Some(log_path) = patch.log_files.get(log_idx) {
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
                                        error!("[login] binder register failed: {e}");
                                    }

                                    client_session
                                        .send(RawPacket::s2c(encode_packet(&redirect)))
                                        .await?;
                                    break;
                                }
                            }
                            warn!("[login] client selected invalid index {idx}");
                        }
                    }
                    SessionEvent::Packet(p) => {
                        log::debug!("[login] ignoring packet 0x{:02X}", p.id());
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

    /// Handle game phase — start headless replay with web control.
    async fn handle_game(
        &self,
        ctx: &ConnectionContext,
        client_session: Session,
    ) -> fw_error::Result<()> {
        // Determine which log file was selected.
        let log_path = if let Some(bound) = &ctx.bound_connection {
            if let Some(path) = bound.context.downcast_ref::<PathBuf>() {
                path.clone()
            } else {
                warn!("[game] no log path in bound connection context");
                return Ok(());
            }
        } else {
            warn!("[game] no bound connection — cannot determine log file");
            return Ok(());
        };

        info!("[game] starting headless replay: {}", log_path.display());

        // Notify web clients that a UO client connected.
        let _ = self.app_state.session_tx.send(WebSessionEvent::ClientConnected);

        // Create command channel for this session.
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<PlaybackCommand>(256);

        // Register the command sender in shared state so web clients can send commands.
        {
            let mut tx_guard = self.app_state.command_tx.lock().await;
            *tx_guard = Some(cmd_tx);
        }

        // Clear packet history from any previous session.
        self.app_state.clear_packet_history().await;

        // Use the shared broadcast channels from app state.
        let status_tx = self.app_state.status_tx.clone();
        let packet_log_tx = self.app_state.packet_log_tx.clone();

        // Spawn a task to listen for status updates and cache the latest.
        let state_for_cache = self.app_state.clone();
        let mut cache_status_rx = status_tx.subscribe();
        let cache_status_handle = tokio::spawn(async move {
            while let Ok(status) = cache_status_rx.recv().await {
                state_for_cache.update_status(status).await;
            }
        });

        // Spawn a task to cache packet log entries in the ring buffer.
        let state_for_pkt_cache = self.app_state.clone();
        let mut cache_pkt_rx = packet_log_tx.subscribe();
        let cache_pkt_handle = tokio::spawn(async move {
            while let Ok(entry) = cache_pkt_rx.recv().await {
                state_for_pkt_cache.push_packet_log(entry).await;
            }
        });

        // Build headless channels.
        let mut channels = HeadlessChannels {
            command_rx: cmd_rx,
            status_tx,
            packet_log_tx,
        };

        // Run headless replay.
        let result = replay_session::run_headless(
            client_session,
            &log_path,
            self.static_data.clone(),
            &mut channels,
        )
        .await;

        // Cleanup: remove the command sender.
        {
            let mut tx_guard = self.app_state.command_tx.lock().await;
            *tx_guard = None;
        }
        *self.app_state.last_status.lock().await = None;
        cache_status_handle.abort();
        cache_pkt_handle.abort();

        // Notify web clients that the UO client disconnected.
        let _ = self.app_state.session_tx.send(WebSessionEvent::ClientDisconnected);

        result
    }
}

// ── main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    args.verbosity.logger(["replay_proxy", "replay_server"]).build()?;

    std::fs::create_dir_all(&args.logs_dir)?;

    // Load static world data (optional).
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
    info!("Web port:   {}", args.web_port);
    info!("Mode:       offline (replay only, web-controlled)");

    // Create shared state.
    let app_state: SharedAppState = Arc::new(ReplayAppState::new(
        args.logs_dir.clone(),
        args.dev_html.clone(),
    ));

    // Start the web server.
    let web_addr = format!("0.0.0.0:{}", args.web_port);
    let web_state = app_state.clone();
    let web_handle = tokio::spawn(async move {
        replay_proxy::web::run_server(
            web_state,
            &web_addr,
            // Shutdown when the main task exits (ctrl-c handling below).
            async { tokio::signal::ctrl_c().await.ok(); },
        ).await;
    });

    // Start the UO listener.
    let proxy_addr = args.proxy.proxy_addr();
    let config = ListenerConfig::new(format!("0.0.0.0:{}", args.proxy.proxy_port)).with_allowed(
        vec![(Some(args.proxy.client_version), Some(args.proxy.encrypted))],
    );

    let handler = ReplayServerHandler {
        proxy_addr,
        version: args.proxy.client_version,
        encrypted: args.proxy.encrypted,
        logs_dir: args.logs_dir,
        static_data,
        app_state,
    };

    info!(
        "UO replay server listening on 0.0.0.0:{}",
        args.proxy.proxy_port
    );
    info!(
        "Web UI at http://127.0.0.1:{}",
        args.web_port,
    );

    // Run the UO listener — blocks until Ctrl-C.
    Listener::new(config, handler).run().await?;

    // Wait for web server to stop.
    let _ = web_handle.await;

    info!("Shutdown complete");
    Ok(())
}
