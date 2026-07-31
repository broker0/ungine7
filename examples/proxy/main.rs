use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use log::{debug, error, info, warn};
use tokio::net::{TcpListener, TcpStream};

use protocol::logs;
use protocol::Protocol;
use u_core::ProtocolVersion;
use protocol::RawPacket;
use protocol::detection::tcp_detector::TcpConnectionDetector;
use protocol::detection::version_db::VersionDatabase;
use protocol::binder::{BinderConfig, ConnectionBinder, PendingConnection};
use protocol::packets::redirect::ServerRedirect;
use packets::traits::{encode_packet, BasicPacket};
use protocol::transport::builder::TransportBuilder;
use protocol::transport::{PacketTransport, TransportError, TransportEvent};

use common::logging::init_logger;

const PROXY_LISTEN: &str = "0.0.0.0:2593";
const REAL_SERVER: &str = "127.0.0.1:5003";


struct ProxyState {
    detector: TcpConnectionDetector,
    binder: ConnectionBinder,
    proxy_addr: SocketAddrV4,
}

/// Redirect interception config, passed into the relay loop.
struct RedirectConfig {
    proxy_addr: SocketAddrV4,
    binder: ConnectionBinder,
    client_version: ProtocolVersion,
    encrypted: bool,
    seed_size: usize,
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logger().debug().filters(["proxy", logs::DETECTION]) .build()?;

    debug!("UO Proxy started");

    let state = Arc::new(ProxyState {
        detector: TcpConnectionDetector::standard(VersionDatabase::common()),
        binder: ConnectionBinder::new(BinderConfig::default()),
        proxy_addr: SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 2593),
    });

    let listener = TcpListener::bind(PROXY_LISTEN).await?;
    debug!("UO Proxy listening on {PROXY_LISTEN}");

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

    // Resolve target address and redirect config based on phase
    let (server_addr, redirect_cfg) = match &protocol {
        Protocol::Login(_) => {
            let cfg = RedirectConfig {
                proxy_addr: state.proxy_addr,
                binder: state.binder.clone(),
                client_version: protocol.client_version(),
                encrypted: protocol.is_encrypted(),
                seed_size: protocol.seed_size(),
            };
            (REAL_SERVER.to_string(), Some(cfg))
        }

        Protocol::Game(info) => {
            let bound = state.binder.bind(info.auth_key)
                .ok_or_else(|| anyhow::anyhow!("no pending connection for key=0x{:08X}", info.auth_key))?;
            let bound_addr = bound.game_server_address
                .ok_or_else(|| anyhow::anyhow!("[{addr}] no game server address"))?;

            debug!("[{addr}] game routing to {bound_addr} (version {}, encrypted={})",
                bound.client_version, bound.encrypted);

            if (bound.encrypted, bound.client_version) != (info.encrypted, info.client_version) {
                warn!(
                    "[{addr}] Mismatch detected: bound(encrypted={}, client_version={}) != info(encrypted={}, client_version={})",
                    bound.encrypted, bound.client_version,
                    info.encrypted, info.client_version
                );
            }
            (format!("{}:{}", bound_addr.ip(), bound_addr.port()), None)
        }
    };

    let phase = if matches!(protocol, Protocol::Login(_)) { "login" } else { "game" };
    let enc = if protocol.is_encrypted() { "enc" } else { " noenc" };
    let tag = format!("[{addr} {phase}-{enc}]");
    debug!("{tag} phase start, version={}", protocol.client_version());

    // Build client-side transport (proxy ← UO client)
    let (mut client, _) = TransportBuilder::server(client_stream, &protocol).build()?;

    // Build server-side transport (proxy → real server)
    let server_stream = TcpStream::connect(&server_addr).await?;
    let (mut server, server_dir) = TransportBuilder::client(server_stream, &protocol).build()?;
    debug!("{tag} connected to {}, starting relaying...", server_addr);

    // Relay
    relay(&tag, &mut *client, &mut *server, server_dir, &redirect_cfg).await?;

    client.close().await;
    server.close().await;
    debug!("{tag} phase complete");
    Ok(())
}


// ── Unified Relay ───────────────────────────────────────────────────────────

/// Bidirectional packet relay between client and server transports.
///
/// If `redirect` is Some, intercepts 0x8C (ServerRedirect) from the server,
/// registers the real address in the binder, rewrites to point at proxy,
/// and breaks out of the relay (login phase ends after redirect).
async fn relay(
    tag: &str,
    client: &mut dyn PacketTransport,
    server: &mut dyn PacketTransport,
    server_dir: u_core::PacketDirection,
    redirect: &Option<RedirectConfig>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            event = client.recv() => {
                match event {
                    Ok(TransportEvent::Seed(s)) => {
                        debug!("{tag} C->S seed ({} bytes)", s.len());
                        server.send(TransportEvent::Seed(s)).await?;
                    }
                    Ok(TransportEvent::Packet(data)) => {
                        debug!("{tag} C->S 0x{:02X} ({} bytes)", data[0], data.len());
                        server.send(TransportEvent::Packet(data)).await?;
                    }
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
                    Ok(TransportEvent::Seed(s)) => {
                        debug!("{tag} S->C seed ({} bytes)", s.len());
                        client.send(TransportEvent::Seed(s)).await?;
                    }
                    Ok(TransportEvent::Packet(data)) => {
                        if let Some(cfg) = redirect {   // Intercept redirect 0x8C if configured
                            if data[0] == ServerRedirect::ID {  // Safe: Transport guarantees non-empty packets
                                let packet = RawPacket::new(data.clone(), server_dir);
                                if let Ok(redir) = ServerRedirect::from_bytes(&packet.data) {
                                    info!("{tag} S->C 0x8C redirect to {} (auth=0x{:08X}) — intercepted",
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
                                        break;
                                    }

                                    let rewritten = redir.with_address(cfg.proxy_addr);
                                    info!("{tag} S->C 0x8C rewritten to {}", rewritten.address());

                                    let new_data = encode_packet(&rewritten);
                                    client.send(TransportEvent::Packet(new_data)).await?;
                                    debug!("{tag} redirect sent, ending login relay");
                                    break;
                                }
                            }
                        }

                        debug!("{tag} S->C 0x{:02X} ({} bytes)", data[0], data.len());
                        client.send(TransportEvent::Packet(data)).await?;
                    }
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
