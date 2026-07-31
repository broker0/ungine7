//! UO proxy using MemoryTransport for downstream communication.
//!
//! Architecture:
//!
//! ```text
//! [UO Client]
//!      │ TCP
//!      ▼
//! client_transport (TcpTransport, acts as server)
//!      │
//!  relay()  ← bidirectional relay
//!      │
//! memory_transport (MemoryTransport)
//!      ║ mpsc channels
//! MemoryHandle
//!      │
//! bridge task ← logs all events, intercepts 0x8C redirect
//!      │
//! downstream_transport (TcpTransport, acts as client)
//!      │ TCP
//!      ▼
//! [Real UO Server]
//! ```
//!
//! Usage: cargo run -p memory-proxy

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use log::{debug, error, info};
use tokio::net::{TcpListener, TcpStream};
use u_core::{PacketDirection, ProtocolVersion, RawPacket};
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{encode_packet, BasicPacket};
use protocol::{Protocol, GameProtocolInfo};
use protocol::detection::tcp_detector::TcpConnectionDetector;
use protocol::detection::version_db::VersionDatabase;
use protocol::binder::{BinderConfig, ConnectionBinder, PendingConnection};
use protocol::transport::builder::TransportBuilder;
use protocol::transport::{PacketTransport, TransportError, TransportEvent};
use protocol::transport::memory::{MemoryHandle, MemoryTransport};

use common::logging::init_logger;

const PROXY_LISTEN: &str = "0.0.0.0:2593";
const REAL_SERVER: &str = "127.0.0.1:5003";


struct ProxyState {
    detector: TcpConnectionDetector,
    binder: ConnectionBinder,
    proxy_addr: SocketAddrV4,
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logger().filter("memory_proxy").build()?;

    let state = Arc::new(ProxyState {
        detector: TcpConnectionDetector::standard(VersionDatabase::common()),
        binder: ConnectionBinder::new(BinderConfig::default()),
        proxy_addr: SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 2593),
    });

    let listener = TcpListener::bind(PROXY_LISTEN).await?;
    info!("UO Memory Proxy listening on {PROXY_LISTEN}");

    loop {
        let (client_stream, addr) = listener.accept().await?;
        let state = state.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(client_stream, addr, &state).await {
                error!("[{addr}] error: {e}");
            }
        });
    }
}


async fn handle_connection(
    client_stream: TcpStream,
    addr: std::net::SocketAddr,
    state: &ProxyState,
) -> anyhow::Result<()> {
    let protocol = state.detector.detect(&client_stream).await?;
    debug!("[{addr}] detected: {protocol:?}");

    match &protocol {
        Protocol::Login(_) => handle_login(client_stream, addr, state, &protocol).await,
        Protocol::Game(info) => handle_game(client_stream, addr, state, info, &protocol).await,
    }
}


// ── Login Phase ─────────────────────────────────────────────────────────────

async fn handle_login(
    client_stream: TcpStream,
    addr: std::net::SocketAddr,
    state: &ProxyState,
    protocol: &Protocol,
) -> anyhow::Result<()> {
    let tag = format!("[{addr} login]");
    info!("{tag} phase start, version={}, encrypted={}",
        protocol.client_version(), protocol.is_encrypted());

    // Client transport — TCP from UO client
    let (mut client, _) = TransportBuilder::server(client_stream, &protocol).build()?;

    // Memory channel — connects relay to the bridge task
    let (mem_transport, handle) = MemoryTransport::channel(32);
    let mut memory_transport: Box<dyn PacketTransport> = Box::new(mem_transport);

    // Downstream TCP — the bridge task owns this
    let server_stream = TcpStream::connect(REAL_SERVER).await?;
    let (mut downstream, downstream_dir) = TransportBuilder::client(server_stream, protocol).build()?;

    // Bridge task: MemoryHandle ↔ downstream TCP, with redirect interception
    let bridge_tag = tag.clone();
    let redirect_cfg = RedirectConfig {
        proxy_addr: state.proxy_addr,
        binder: state.binder.clone(),
        client_version: protocol.client_version(),
        encrypted: protocol.is_encrypted(),
        seed_size: protocol.seed_size(),
    };

    let bridge = tokio::spawn(async move {
        bridge(&bridge_tag, handle, &mut downstream, downstream_dir, Some(redirect_cfg)).await;
        downstream.close().await;
    });

    // Relay: client TCP ↔ memory transport
    let result = transport_relay(&tag, &mut client, &mut memory_transport).await;

    bridge.abort();
    let _ = bridge.await;

    debug!("{tag} login phase complete");
    result
}


// ── Bridge ──────────────────────────────────────────────────────────────────

/// Configuration for redirect interception in the bridge.
struct RedirectConfig {
    proxy_addr: SocketAddrV4,
    binder: ConnectionBinder,
    client_version: ProtocolVersion,
    encrypted: bool,
    seed_size: usize,
}

/// Bidirectional bridge between a [`MemoryHandle`] and a downstream transport.
async fn bridge(
    tag: &str,
    mut handle: MemoryHandle,
    downstream: &mut Box<dyn PacketTransport>,
    downstream_dir: PacketDirection,
    redirect: Option<RedirectConfig>,
) {
    loop {
        tokio::select! {
            // Events from relay (client → memory → handle) → downstream
            event = handle.recv() => {
                match event {
                    Ok(TransportEvent::Seed(s)) => {
                        debug!("{tag} C→S seed ({} bytes)", s.len());
                        if downstream.send(TransportEvent::Seed(s)).await.is_err() { break; }
                    }
                    Ok(TransportEvent::Packet(data)) => {
                        debug!("{tag} C→S 0x{:02X} ({} bytes)", data[0], data.len());
                        if downstream.send(TransportEvent::Packet(data)).await.is_err() { break; }
                    }
                    Err(_) => {
                        debug!("{tag} bridge: memory handle closed");
                        break;
                    }
                }
            }

            // Events from downstream server → handle → memory → relay → client
            event = downstream.recv() => {
                match event {
                    Ok(TransportEvent::Seed(s)) => {
                        debug!("{tag} S→C seed ({} bytes)", s.len());
                        if handle.send(TransportEvent::Seed(s)).await.is_err() { break; }
                    }

                    Ok(TransportEvent::Packet(data)) => {
                        // Intercept redirect packet 0x8C if configured
                        if let Some(ref cfg) = redirect {
                            if data[0] == <ServerRedirect as BasicPacket>::ID {
                                let packet = RawPacket::new(data.clone(), downstream_dir);
                                if let Ok(redir) = ServerRedirect::from_bytes(&packet.data) {
                                    info!("{tag} S→C 0x8C redirect to {} (auth=0x{:08X}) — intercepted",
                                        redir.address(), redir.auth_key);

                                    let pending = PendingConnection {
                                        auth_key: redir.auth_key,
                                        client_version: cfg.client_version,
                                        game_server_address: Some(redir.address()),
                                        encrypted: cfg.encrypted,
                                        seed_size: cfg.seed_size,
                                        created_at: std::time::Instant::now(),
                                        context: Box::new(()),
                                    };
                                    if let Err(e) = cfg.binder.register(pending) {
                                        error!("{tag} binder register failed: {e}");
                                    }

                                    let rewritten = redir.with_address(cfg.proxy_addr);
                                    info!("{tag} S→C 0x8C rewritten to {}", rewritten.address());

                                    let event = TransportEvent::Packet(encode_packet(&rewritten));
                                    if handle.send(event).await.is_err() { break; }
                                    continue;
                                }
                            }
                        }

                        debug!("{tag} S→C 0x{:02X} ({} bytes)", data[0], data.len());
                        let event = TransportEvent::Packet(data);
                        if handle.send(event).await.is_err() { break; }
                    }

                    Err(TransportError::Closed) => {
                        info!("{tag} downstream disconnected");
                        break;
                    }
                    Err(e) => {
                        error!("{tag} downstream error: {e}");
                        break;
                    }
                }
            }
        }
    }
}


// ── Game Phase ──────────────────────────────────────────────────────────────

async fn handle_game(
    client_stream: TcpStream,
    addr: std::net::SocketAddr,
    state: &ProxyState,
    game_info: &GameProtocolInfo,
    protocol: &Protocol,
) -> anyhow::Result<()> {
    let bound = state.binder.bind(game_info.auth_key)
        .ok_or_else(|| anyhow::anyhow!(
            "no pending connection for key=0x{:08X}", game_info.auth_key
        ))?;

    let tag = format!("[{addr} game]");

    let real_addr = bound.game_server_address
        .ok_or_else(|| anyhow::anyhow!("{tag} no game server address"))?;

    info!("{tag} routing to {real_addr} (version {}, encrypted={})",
        bound.client_version, bound.encrypted);

    // Client transport — TCP from UO client
    let (mut client, _) = TransportBuilder::server(client_stream, &protocol).build()?;

    // Memory channel
    let (mem_transport, handle) = MemoryTransport::channel(32);
    let mut memory_transport: Box<dyn PacketTransport> = Box::new(mem_transport);

    // Downstream TCP to real game server
    let downstream_protocol = Protocol::Game(GameProtocolInfo {
        seed: game_info.seed,
        seed_size: game_info.seed_size,
        auth_key: game_info.auth_key,
        client_version: bound.client_version,
        encrypted: bound.encrypted,
    });

    let server_stream = TcpStream::connect(
        format!("{}:{}", real_addr.ip(), real_addr.port())
    ).await?;

    let (mut downstream, downstream_dir) = TransportBuilder::client(server_stream, &downstream_protocol).build()?;

    // Bridge task: MemoryHandle ↔ downstream TCP, passthrough
    let bridge_tag = tag.clone();
    let bridge = tokio::spawn(async move {
        bridge(&bridge_tag, handle, &mut downstream, downstream_dir, None).await;
        downstream.close().await;
    });

    // Relay: client TCP ↔ memory transport
    let result = transport_relay(&tag, &mut client, &mut memory_transport).await;

    bridge.abort();
    let _ = bridge.await;

    debug!("{tag} game phase complete");
    result
}


// ── Transport Relay ────────────────────────────────────────────────────────

/// Simple bidirectional relay between two transports.
async fn transport_relay(
    tag: &str,
    client: &mut Box<dyn PacketTransport>,
    server: &mut Box<dyn PacketTransport>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            event = client.recv() => {
                match event {
                    Ok(ev) => { server.send(ev).await?; }
                    Err(TransportError::Closed) => {
                        debug!("{tag} client disconnected");
                        break;
                    }
                    Err(e) => {
                        error!("{tag} client error: {e}");
                        break;
                    }
                }
            }
            event = server.recv() => {
                match event {
                    Ok(ev) => { client.send(ev).await?; }
                    Err(TransportError::Closed) => {
                        debug!("{tag} server disconnected");
                        break;
                    }
                    Err(e) => {
                        error!("{tag} server error: {e}");
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}
