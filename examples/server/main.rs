//! Minimal UO server using protocol directly.
//!
//! Handles the full login flow and spawns a fake game world:
//! - Login phase: 0x80 → 0xA8, 0xA0 → 0x8C
//! - Game phase:  0x91 → 0xA9, 0x5D → world spawn packets
//! - In-world:    0x73 (ping/pong), 0x02 (movement)
//!
//! Usage: cargo run -p server

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

use log::{debug, error, info};
use tokio::net::TcpListener;
use u_core::{PacketDirection, ProtocolVersion, RawPacket};
use protocol::packets::login::*;
use protocol::packets::system::{Ping, EnableFeatures};
use protocol::packets::redirect::ServerRedirect;
use packets::traits::encode_packet;
use packets::traits::{ManualPacket, BasicPacket};
use protocol::detection::tcp_detector::TcpConnectionDetector;
use protocol::detection::version_db::VersionDatabase;
use protocol::binder::{BinderConfig, ConnectionBinder};
use protocol::transport::builder::TransportBuilder;
use protocol::transport::{PacketTransport, TransportError, TransportEvent};
use protocol::{Protocol, GameProtocolInfo};

use packets::movement::{MoveRequest, MoveAck, Notoriety};

use common::logging::init_logger;
use common::spawn::build_world_spawn;


const LISTEN_ADDR: &str = "0.0.0.0:2593";
const SERVER_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const SERVER_PORT: u16 = 2593;

struct ServerState {
    detector: TcpConnectionDetector,
    binder: ConnectionBinder,
    server_addr: SocketAddrV4,
    version: ProtocolVersion,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logger().build()?;

    let state = Arc::new(ServerState {
        detector: TcpConnectionDetector::standard(VersionDatabase::common()),
        binder: ConnectionBinder::new(BinderConfig::default()),
        server_addr: SocketAddrV4::new(SERVER_IP, SERVER_PORT),
        version: ProtocolVersion::new(7, 0, 95, 0),
    });

    let listener = TcpListener::bind(LISTEN_ADDR).await?;
    info!("UO Server listening on {LISTEN_ADDR}");

    loop {
        let (stream, addr) = listener.accept().await?;
        let state = state.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, addr, &state).await {
                error!("[{addr}] error: {e}");
            }
        });
    }
}


async fn handle_connection(
    stream: tokio::net::TcpStream,
    addr: SocketAddr,
    state: &ServerState,
) -> anyhow::Result<()> {
    let protocol = state.detector.detect(&stream).await?;
    debug!("[{addr}] detected: {protocol:?}");

    match &protocol {
        Protocol::Login(_) => handle_login(stream, addr, state, &protocol).await,
        Protocol::Game(info) => handle_game(stream, addr, state, info, &protocol).await,
    }
}


// ── Login Phase ─────────────────────────────────────────────────────────────

async fn handle_login(
    stream: tokio::net::TcpStream,
    addr: SocketAddr,
    state: &ServerState,
    protocol: &Protocol,
) -> anyhow::Result<()> {
    let tag = format!("[{addr} login]");
    info!("{tag} login phase, version={}", protocol.client_version());

    let (mut transport, direction) = TransportBuilder::server(stream, &protocol).build()?;

    login_loop(&tag, &mut transport, direction, state).await?;

    transport.close().await;
    debug!("{tag} login phase complete");
    Ok(())
}

async fn login_loop(
    tag: &str,
    transport: &mut Box<dyn PacketTransport>,
    direction: PacketDirection,
    state: &ServerState,
) -> anyhow::Result<()> {
    let mut auth_counter: u32 = 0x1000_0000;

    loop {
        match transport.recv().await {
            Ok(TransportEvent::Seed(s)) => {
                debug!("{tag} received seed ({} bytes)", s.len());
            }

            Ok(TransportEvent::Packet(data)) => {
                let p = RawPacket::new(data, direction);
                debug!("{tag} C→S 0x{:02X} ({} bytes)", p.id(), p.len());
                let responses = handle_login_packet(tag, &p, state, &mut auth_counter);
                for resp in responses {
                    transport.send(TransportEvent::Packet(resp.data)).await?;
                }
            }

            Err(TransportError::Closed) => {
                debug!("{tag} client disconnected");
                break;
            }
            Err(e) => {
                error!("{tag} error: {e}");
                break;
            }
        }
    }
    Ok(())
}

fn handle_login_packet(
    tag: &str,
    packet: &RawPacket,
    state: &ServerState,
    auth_counter: &mut u32,
) -> Vec<RawPacket> {
    let mut responses = Vec::new();

    match packet.id() {
        // 0x80 AccountLogin → respond with GameServerList (0xA8)
        0x80 => {
            if let Ok(login) = AccountLogin::from_bytes(&packet.data) {
                info!("{tag} account login: '{}' / '*****'", &*login.account/*, &*login.password*/);
            }

            let server_list = GameServerList::new(0xCC, vec![GameServerEntry {
                index: 0,
                name: "Test Server".into(),
                full_percent: 0,
                timezone: 0,
                ip: *state.server_addr.ip(),
            }]);
            responses.push(RawPacket::s2c(encode_packet(&server_list)));
        }

        // 0xA0 SelectServer → respond with ServerRedirect (0x8C)
        0xA0 => {
            if let Ok(sel) = SelectServer::from_bytes(&packet.data) {
                debug!("{tag} server selected: index={}", sel.index);
            }

            *auth_counter = auth_counter.wrapping_add(1);
            let auth_key = *auth_counter;

            let redirect = ServerRedirect::new(state.server_addr, auth_key);

            // Register pending connection for game phase
            let pending = protocol::binder::PendingConnection {
                auth_key,
                client_version: ProtocolVersion::new(7, 0, 95, 0),
                game_server_address: Some(state.server_addr),
                encrypted: true,
                seed_size: 4,
                created_at: std::time::Instant::now(),
                context: Box::new(()),
            };
            if let Err(e) = state.binder.register(pending) {
                error!("{tag} binder register failed: {e}");
            }

            responses.push(RawPacket::s2c(encode_packet(&redirect)));
        }

        id => {
            debug!("{tag} unhandled packet 0x{id:02X} ({} bytes)", packet.len());
        }
    }

    responses
}


// ── Game Phase ──────────────────────────────────────────────────────────────

async fn handle_game(
    stream: tokio::net::TcpStream,
    addr: SocketAddr,
    state: &ServerState,
    game_info: &GameProtocolInfo,
    protocol: &Protocol,
) -> anyhow::Result<()> {
    let tag = format!("[{addr} game]");

    let _bound = state.binder.bind(game_info.auth_key);
    info!("{tag} game phase, version={}", protocol.client_version());

    let (mut transport, direction) = TransportBuilder::server(stream, &protocol).build()?;

    game_loop(&tag, &mut transport, direction, state).await?;

    transport.close().await;
    debug!("{tag} game phase complete");
    Ok(())
}

async fn game_loop(tag: &str, transport: &mut Box<dyn PacketTransport>, direction: PacketDirection, state: &ServerState) -> anyhow::Result<()> {
    loop {
        match transport.recv().await {
            Ok(TransportEvent::Seed(s)) => {
                debug!("{tag} received seed ({} bytes)", s.len());
            }

            Ok(TransportEvent::Packet(data)) => {
                let p = RawPacket::new(data, direction);
                debug!("{tag} C→S 0x{:02X} ({} bytes)", p.id(), p.len());
                let responses = handle_game_packet(tag, &p, state);
                for resp in responses {
                    transport.send(TransportEvent::Packet(resp.data)).await?;
                }
            }

            Err(TransportError::Closed) => {
                info!("{tag} client disconnected");
                break;
            }
            Err(e) => {
                error!("{tag} error: {e}");
                break;
            }
        }
    }
    Ok(())
}

fn handle_game_packet(tag: &str, packet: &RawPacket, state: &ServerState) -> Vec<RawPacket> {
    let mut responses = Vec::new();

    match packet.id() {
        // 0x91 GameLogin → respond with CharacterList (0xA9)
        0x91 => {
            if let Ok(login) = GameLogin::from_bytes(&packet.data) {
                info!("{tag} game login: '{}' (auth_key=0x{:08X})", &*login.account, login.auth_key);
            }

            // EnableLockedClientFeatures (0xB9)
            let features = EnableFeatures::new(0x0002, state.version);
            responses.push(RawPacket::s2c(features.to_bytes()));

            let char_list = CharacterList::new(
                vec![
                    CharacterSlot::new("Player"),
                    CharacterSlot::new(""),
                    CharacterSlot::new(""),
                    CharacterSlot::new(""),
                    CharacterSlot::new(""),
                ],
                vec![StartingLocation {
                    index: 0,
                    city_name: "Yew".into(),
                    area_name: "Town Center".into(),
                }],
                1024,
            );
            responses.push(RawPacket::s2c(encode_packet(&char_list)));
        }

        // 0x5D LoginCharacter → spawn world
        0x5D => {
            if let Ok(lc) = LoginCharacter::from_bytes(&packet.data) {
                info!("{tag} character selected: '{}' (slot {})", &*lc.name, lc.slot);
            }
            responses.extend(build_world_spawn());
        }

        // 0x73 Ping → Pong
        0x73 => {
            if let Ok(ping) = Ping::from_bytes(&packet.data) {
                debug!("{tag} ping seq={}, sending pong", ping.sequence);
                responses.push(RawPacket::s2c(encode_packet(&ping)));
            }
        }

        // 0x02 MoveRequest → MoveAck (0x22)
        0x02 => {
            if let Ok(req) = MoveRequest::from_bytes(&packet.data) {
                let ack = MoveAck {
                    id: MoveAck::ID,
                    sequence: req.sequence,
                    notoriety: Notoriety::Innocent,
                };
                responses.push(RawPacket::s2c(encode_packet(&ack)));
            }
        }

        // 0xBF General Information
        0xBF => {
            if packet.len() >= 5 {
                let sub_cmd = u16::from_be_bytes([packet.data[3], packet.data[4]]);
                debug!("{tag} general info subcmd=0x{sub_cmd:04X}");
            }
        }

        id => {
            debug!("{tag} unhandled packet 0x{id:02X} ({} bytes)", packet.len());
        }
    }

    responses
}



