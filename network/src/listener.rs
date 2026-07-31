//! Unified TCP listener for UO connections.
//!
//! `Listener` accepts incoming connections, detects the protocol,
//! builds a `Session`, and hands it off to a [`ListenerHandler`].
//! The handler decides what to do — run a server packet loop,
//! proxy to a downstream server, or anything else.

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use async_trait::async_trait;
use log::{debug, error, info, warn};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use u_core::{PacketDirection, ProtocolVersion};
use protocol::detection::tcp_detector::TcpConnectionDetector;
use protocol::detection::version_db::VersionDatabase;
use crate::logs;
use protocol::binder::{BinderConfig, BoundConnection, ConnectionBinder};
use protocol::Protocol;
use protocol::transport::builder::{TransportBuildError, TransportBuilder};
use protocol::transport::PacketTransport;

use crate::error::{self, NetworkError};
use crate::handler::HandlerChain;
use crate::session::Session;

/// Commands that can be sent to a [`Listener`] via a control channel.
#[derive(Debug)]
pub enum ListenerControl {
    /// Stop accepting new connections and wait for active ones to finish.
    Shutdown,
    /// Stop accepting new connections and abort all active connections immediately.
    ForceShutdown,
    /// Stop accepting new connections but let active relay tasks finish on their own.
    ///
    /// Unlike [`Shutdown`](Self::Shutdown) this does **not** await the active tasks —
    /// they keep running in the background.  The listener task returns immediately
    /// after closing the TCP accept loop, so the caller can consider the instance
    /// "stopped" (no new connections) while existing sessions remain alive.
    StopListening,
}

/// Listener configuration.
#[derive(Debug)]
pub struct ListenerConfig {
    pub listen_addr: String,
    pub detector: Option<TcpConnectionDetector>,
    pub binder_config: BinderConfig,
    pub allowed: Vec<(Option<ProtocolVersion>, Option<bool>)>,
}

impl ListenerConfig {
    pub fn new(listen_addr: impl Into<String>) -> Self {
        Self {
            listen_addr: listen_addr.into(),
            detector: None,
            binder_config: BinderConfig::default(),
            allowed: vec![(None, None)],
        }
    }

    pub fn with_allowed(mut self, allowed: Vec<(Option<ProtocolVersion>, Option<bool>)>) -> Self {
        self.allowed = allowed;
        self
    }

    pub fn with_required_version(mut self, version: ProtocolVersion) -> Self {
        self.allowed = vec![(Some(version), None)];
        self
    }

    pub fn with_required_encryption(mut self, encrypted: bool) -> Self {
        self.allowed = vec![(None, Some(encrypted))];
        self
    }

    pub fn with_detector(mut self, detector: TcpConnectionDetector) -> Self {
        self.detector = Some(detector);
        self
    }

    pub fn with_binder_config(mut self, config: BinderConfig) -> Self {
        self.binder_config = config;
        self
    }
}

/// Context passed to handler methods.
pub struct ConnectionContext {
    pub addr: SocketAddr,
    pub protocol: Protocol,
    pub binder: ConnectionBinder,
    /// For game-phase connections: the real game server address extracted from
    /// the `ConnectionBinder` (originally stored there when the login-phase
    /// `0x8C` redirect was intercepted).
    ///
    /// `None` for login-phase connections and for game connections where no
    /// matching pending entry was found in the binder (e.g. direct game logins
    /// without a preceding proxy login).
    pub game_server_address: Option<SocketAddrV4>,
    /// Full bound connection data for game-phase connections. Contains the
    /// client version and encryption flag resolved during login, which may
    /// differ from what the detector inferred. `None` for login-phase.
    pub bound_connection: Option<BoundConnection>,
}

impl ConnectionContext {
    /// Returns the upstream server address for proxy use.
    ///
    /// Prefers `game_server_address` (resolved via the binder during game phase),
    /// falls back to the provided default for login-phase connections.
    pub fn upstream_addr(&self, default: &str) -> String {
        self.game_server_address
            .map(|a| format!("{}:{}", a.ip(), a.port()))
            .unwrap_or_else(|| default.to_string())
    }
}

/// Phase of the connection (used in `configure_session`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    LoginClient,
    LoginServer,
    GameClient,
    GameServer,
}

impl SessionPhase {
    /// Returns the **client-facing** phase for a detected protocol.
    ///
    /// - `Protocol::Login` → `LoginClient`
    /// - `Protocol::Game`  → `GameClient`
    pub fn client_for(protocol: &Protocol) -> Self {
        match protocol {
            Protocol::Login(_) => Self::LoginClient,
            Protocol::Game(_) => Self::GameClient,
        }
    }

    /// Returns the **server-facing** phase for a detected protocol.
    ///
    /// - `Protocol::Login` → `LoginServer`
    /// - `Protocol::Game`  → `GameServer`
    pub fn server_for(protocol: &Protocol) -> Self {
        match protocol {
            Protocol::Login(_) => Self::LoginServer,
            Protocol::Game(_) => Self::GameServer,
        }
    }
}

impl std::fmt::Display for SessionPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoginClient => write!(f, "LoginClient"),
            Self::LoginServer => write!(f, "LoginServer"),
            Self::GameClient => write!(f, "GameClient"),
            Self::GameServer => write!(f, "GameServer"),
        }
    }
}

/// Handler trait for `Listener`.
///
/// The listener handles accept, detection, and session building.
/// Everything else — packet processing, proxying, etc. — lives in
/// [`handle_session`](ListenerHandler::handle_session).
#[async_trait]
pub trait ListenerHandler: Send + Sync + 'static {
    /// Configure handler chains for the session.
    /// Returns `(inbound_handlers, outbound_handlers)`.
    /// By default, no handlers are attached.
    fn configure_handlers(
        &self,
        _phase: SessionPhase,
        _ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        (HandlerChain::new(), HandlerChain::new())
    }

    /// Build the server-side transport for an accepted connection.
    ///
    /// The default implementation creates a standard
    /// [`TransportBuilder::server`] transport. Override this to build a
    /// custom [`ProtocolStream`](protocol::transport::protocol_stream::ProtocolStream)
    /// stack (e.g. with logging middleware between encryption and framing):
    ///
    /// ```rust,ignore
    /// fn build_transport(
    ///     &self,
    ///     stream: TcpStream,
    ///     ctx: &ConnectionContext,
    /// ) -> Result<(Box<dyn PacketTransport>, PacketDirection), TransportBuildError> {
    ///     let tcp = TcpByteStream::new(stream);
    ///     let raw_logged = LoggingStream::new(tcp, ...);  // sees ciphertext
    ///     let (enc, dec) = encryption::cipher_pair(&ctx.protocol, Role::Server);
    ///     let crypto = CryptoStream::new(raw_logged, enc, dec);
    ///     let plain_logged = LoggingStream::new(crypto, ...);  // sees plaintext
    ///     TransportBuilder::server_with_stream(plain_logged, &ctx.protocol).build()
    /// }
    /// ```
    fn build_transport(
        &self,
        stream: TcpStream,
        _ctx: &ConnectionContext,
    ) -> Result<(Box<dyn PacketTransport>, PacketDirection), TransportBuildError> {
        TransportBuilder::server(stream, &_ctx.protocol).build()
    }

    /// Called when a new TCP connection is accepted, before session creation.
    /// Return `false` to reject the connection.
    async fn on_connect(&self, _ctx: &ConnectionContext) -> bool {
        true
    }

    /// Called after a connection ends (successfully or with an error).
    async fn on_disconnect(&self, _addr: SocketAddr, _result: &error::Result<()>) {}

    /// Main entry point — called with a ready-to-use session.
    ///
    /// Use [`relay::relay`](crate::relay::relay) for proxying,
    /// or run your own recv/send loop for a server.
    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        session: Session,
    ) -> error::Result<()>;
}

/// Main listener.
pub struct Listener<H: ListenerHandler> {
    config: Arc<ListenerConfig>,
    handler: Arc<H>,
    binder: ConnectionBinder,
}

impl<H: ListenerHandler> Listener<H> {
    pub fn new(mut config: ListenerConfig, handler: H) -> Self {
        let binder = ConnectionBinder::new(config.binder_config.clone());

        // If no custom detector was provided, create a binder-aware one
        // so game-phase connections can resolve the correct client version
        // from the login-phase registration stored in the binder.
        //
        // When a specific version is declared in the `allowed` list, pass it
        // as `expected_version` so that unencrypted clients older than 6.0.5.0
        // (which don't send an extended seed) are detected with the correct
        // version instead of the default AOS (4.0.0.0) fallback.
        if config.detector.is_none() {
            let expected_version = config.allowed.iter()
                .find_map(|(v, _)| *v);

            config.detector = Some(TcpConnectionDetector::standard_with_version(
                VersionDatabase::common(),
                binder.clone(),
                expected_version,
            ));
        }

        Self {
            config: Arc::new(config),
            handler: Arc::new(handler),
            binder,
        }
    }

    pub fn binder(&self) -> &ConnectionBinder { &self.binder }

    pub async fn run(&self) -> error::Result<()> {
        let listener = TcpListener::bind(&self.config.listen_addr).await?;
        self.log_startup();

        loop {
            let (stream, addr) = listener.accept().await?;
            let config = self.config.clone();
            let handler = self.handler.clone();
            let binder = self.binder.clone();

            tokio::spawn(async move {
                let result = handle_connection(stream, addr, &config, &*handler, &binder).await;
                if let Err(ref e) = result {
                    error!(target: logs::LISTENER, "[{addr}] error: {e}");
                }
                handler.on_disconnect(addr, &result).await;
            });
        }
    }

    /// Run the listener with a control channel for graceful shutdown.
    ///
    /// Behaves like [`run`](Self::run) but also listens on `control_rx`.
    /// When [`ListenerControl::Shutdown`] is received the listener stops
    /// accepting new connections and waits for all active connection tasks
    /// to finish before returning.
    pub async fn run_with_control(
        &self,
        mut control_rx: mpsc::Receiver<ListenerControl>,
    ) -> error::Result<()> {
        let listener = TcpListener::bind(&self.config.listen_addr).await?;
        self.log_startup();

        let mut tasks = JoinSet::new();

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, addr) = result?;
                    let config = self.config.clone();
                    let handler = self.handler.clone();
                    let binder = self.binder.clone();

                    tasks.spawn(async move {
                        let result = handle_connection(stream, addr, &config, &*handler, &binder).await;
                        if let Err(ref e) = result {
                            error!(target: logs::LISTENER, "[{addr}] error: {e}");
                        }
                        handler.on_disconnect(addr, &result).await;
                    });
                }
                cmd = control_rx.recv() => {
                    match cmd {
                        Some(ListenerControl::Shutdown) | None => {
                            info!(target: logs::LISTENER, "shutting down, waiting for {} active connection(s)…", tasks.len());
                            // Wait for all active connections to finish.
                            while tasks.join_next().await.is_some() {}
                        }
                        Some(ListenerControl::ForceShutdown) => {
                            info!(target: logs::LISTENER, "force-shutting down, aborting {} active connection(s)…", tasks.len());
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                        }
                        Some(ListenerControl::StopListening) => {
                            info!(target: logs::LISTENER, "stopped accepting connections, {} active relay(s) continue independently", tasks.len());
                            // Detach all active tasks — they keep running on the tokio runtime
                            // and will clean up via on_disconnect when they finish naturally.
                            tasks.detach_all();
                        }
                    }
                    break;
                }
            }
        }

        info!(target: logs::LISTENER, "shutdown complete");
        Ok(())
    }

    fn log_startup(&self) {
        info!(target: logs::LISTENER, "listening on {}", self.config.listen_addr);

        if !self.config.allowed.is_empty() {
            let allowed = self.config.allowed.iter()
                .map(|(v, e)| format!(
                    "{}/{}",
                    v.as_ref().map_or("any".to_string(), ToString::to_string),
                    match e { Some(true) => "enc", Some(false) => "plain", None => "any" }
                ))
                .collect::<Vec<_>>()
                .join(", ");

            debug!(target: logs::LISTENER, "allowed versions: [{allowed}]");
        } else {
            warn!(target: logs::LISTENER, "no compatible versions configured");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal
// ─────────────────────────────────────────────────────────────────────────────

async fn handle_connection<H: ListenerHandler>(
    stream: TcpStream,
    addr: SocketAddr,
    config: &ListenerConfig,
    handler: &H,
    binder: &ConnectionBinder,
) -> error::Result<()> {
    let detector = config.detector.as_ref()
        .expect("ListenerConfig must have a detector");
    let protocol = detector.detect(&stream).await?;
    info!(target: logs::LISTENER, "[{addr}] detected: version={} encrypted={} phase={:?}",
        protocol.client_version(), protocol.is_encrypted(),
        match &protocol { Protocol::Login(_) => "Login", Protocol::Game(_) => "Game" });

    let matches_allowed = config.allowed.iter().any(|(v, e)| {
        v.as_ref().map_or(true, |rv| *rv == protocol.client_version()) &&
            e.map_or(true, |re| re == protocol.is_encrypted())
    });

    if config.allowed.is_empty() || !matches_allowed {
        let allowed_str: Vec<String> = config.allowed.iter().map(|(v, e)| {
            let vs = v.as_ref().map(|x| x.to_string()).unwrap_or_else(|| "*".into());
            let es = e.map(|x| x.to_string()).unwrap_or_else(|| "*".into());
            format!("(version={vs} encrypted={es})")
        }).collect();
        warn!(target: logs::LISTENER, "[{addr}] connection rejected: detected version={} encrypted={}, \
               allowed: [{}]",
            protocol.client_version(), protocol.is_encrypted(),
            allowed_str.join(", "));
        return Err(NetworkError::Rejected(format!(
            "no allowed version/encryption match for {} (detected: {:?})",
            addr, protocol
        )));
    }
    debug!(target: logs::LISTENER, "[{addr}] version/enc ok: {protocol:?}");

    // For game-phase connections, consume the binder entry now so that
    // the real game server address is available in ConnectionContext.
    // This is the single authoritative bind() call — handlers must NOT
    // call binder.bind() themselves for the game phase.
    let (game_server_address, bound_connection) = match &protocol {
        Protocol::Game(info) => {
            match binder.bind(info.auth_key) {
                Some(bound) => {
                    let addr_v4 = bound.game_server_address;
                    debug!(target: logs::LISTENER, "[{addr}] game bound: server={:?} version={} encrypted={}",
                        addr_v4, bound.client_version, bound.encrypted);
                    (addr_v4, Some(bound))
                }
                None => {
                    debug!(target: logs::LISTENER, "[{addr}] game: no pending connection for key=0x{:08X}", info.auth_key);
                    (None, None)
                }
            }
        }
        Protocol::Login(_) => (None, None),
    };

    let ctx = ConnectionContext {
        addr,
        protocol,
        binder: binder.clone(),
        game_server_address,
        bound_connection,
    };

    if !handler.on_connect(&ctx).await {
        return Err(NetworkError::Rejected("connection rejected by handler".into()));
    }

    let phase = SessionPhase::client_for(&ctx.protocol);

    let (transport, direction) = handler.build_transport(stream, &ctx)?;

    let (inbound, outbound) = handler.configure_handlers(phase, &ctx);
    let session = Session::with_handlers(transport, direction, inbound, outbound);

    handler.handle_session(&ctx, session).await
}
