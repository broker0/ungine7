//! Minimal UO server using network.
//!
//! The server handles login flow, spawns a fake character with a horse mount,
//! responds to pings, and acknowledges movement requests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU32, Ordering};

use log::{debug, error, info};

use protocol::RawPacket;
use protocol::packets::login::*;
use protocol::packets::system::{EnableFeatures, Ping};
use protocol::packets::redirect::ServerRedirect;
use protocol::packets::traits::{encode_packet, ManualPacket};
use protocol::Protocol;
use protocol::binder::PendingConnection;

use network::error::{self, NetworkError};
use network::handler::filters::LogHandler as LogFilter;
use network::handler::HandlerChain;
use network::session::{Session, SessionEvent};
use network::listener::{
    ConnectionContext, ListenerConfig, ListenerHandler, SessionPhase, Listener,
};

use common::logging::init_logger;
use common::spawn::build_world_spawn;
use packets::traits::BasicPacket;
use u_core::ProtocolVersion;

use packets::movement::{MoveRequest, MoveAck, Notoriety};

const LISTEN_ADDR: &str = "0.0.0.0:2593";
const SERVER_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const SERVER_PORT: u16 = 2593;


struct TestServer {
    server_addr: SocketAddrV4,
    auth_counter: AtomicU32,
    version: ProtocolVersion,
}

impl TestServer {
    fn new(server_addr: SocketAddrV4) -> Self {
        Self {
            server_addr,
            auth_counter: AtomicU32::new(0x1000_0000),
            version: ProtocolVersion::new(3, 0, 8, 0)
        }
    }

    fn next_auth_key(&self) -> u32 {
        self.auth_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Handle a login-phase packet. Returns packets to send back,
    /// or None to silently consume.
    fn handle_login(&self, addr: SocketAddr, ctx: &ConnectionContext, packet: &RawPacket) -> Option<Vec<RawPacket>> {
        match packet.id() {
            0x80 => {
                if let Ok(login) = AccountLogin::from_bytes(&packet.data) {
                    info!("[{addr}] login: '{}' / '*****'", &*login.account/*, &*login.password*/);
                }
                let list = GameServerList::new(0xCC, vec![GameServerEntry {
                    index: 0,
                    name: "Test Server".into(),
                    full_percent: 0,
                    timezone: 0,
                    ip: *self.server_addr.ip(),
                }]);
                Some(vec![RawPacket::s2c(encode_packet(&list))])
            }

            0xA0 => {
                if let Ok(sel) = SelectServer::from_bytes(&packet.data) {
                    debug!("[{addr}] selected server index={}", sel.index);
                }
                let auth_key = self.next_auth_key();
                let pending = PendingConnection {
                    auth_key,
                    client_version: self.version,
                    game_server_address: Some(self.server_addr),
                    encrypted: true,
                    seed_size: 4,
                    created_at: std::time::Instant::now(),
                    context: Box::new(()),
                };
                let _ = ctx.binder.register(pending);
                let redirect = ServerRedirect::new(self.server_addr, auth_key);
                Some(vec![RawPacket::s2c(encode_packet(&redirect))])
            }

            id => {
                debug!("[{addr}] login: unhandled 0x{id:02X}");
                None
            }
        }
    }

    /// Handle a game-phase packet. Returns packets to send back,
    /// or None to silently consume.
    fn handle_game(&self, addr: SocketAddr, packet: &RawPacket) -> Option<Vec<RawPacket>> {
        match packet.id() {
            0x91 => {
                if let Ok(login) = GameLogin::from_bytes(&packet.data) {
                    info!("[{addr}] game login: '{}' (auth=0x{:08X})",
                        &*login.account, login.auth_key);
                }
                // let mut packets = vec![RawPacket::s2c(&[0xB9, 0x00, 0x02])];
                let features = EnableFeatures::new(0x0002, self.version);
                let mut packets = vec![RawPacket::s2c(features.to_bytes())];
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
                packets.push(RawPacket::s2c(encode_packet(&char_list)));
                Some(packets)
            }

            0x5D => {
                if let Ok(lc) = LoginCharacter::from_bytes(&packet.data) {
                    info!("[{addr}] character: '{}' (slot {})", &*lc.name, lc.slot);
                }
                Some(build_world_spawn())
            }

            0x73 => {
                Ping::from_bytes(&packet.data).ok().map(|ping| {
                    debug!("[{addr}] ping seq={}", ping.sequence);
                    vec![RawPacket::s2c(encode_packet(&ping))]
                })
            }

            0x02 => {
                MoveRequest::from_bytes(&packet.data).ok().map(|req| {
                    let ack = MoveAck {
                        id: MoveAck::ID,
                        sequence: req.sequence,
                        notoriety: Notoriety::Innocent,
                    };
                    vec![RawPacket::s2c(encode_packet(&ack))]
                })
            }

            0xBF => {
                if packet.len() >= 5 {
                    let sub = u16::from_be_bytes([packet.data[3], packet.data[4]]);
                    debug!("[{addr}] general info 0x{sub:04X}");
                }
                None
            }

            id => {
                debug!("[{addr}] game: unhandled 0x{id:02X}");
                None
            }
        }
    }
}


#[async_trait::async_trait]
impl ListenerHandler for TestServer {
    fn configure_handlers(
        &self,
        phase: SessionPhase,
        _ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        let mut inbound = HandlerChain::new();
        let outbound = HandlerChain::new();

        let log_name = match phase {
            SessionPhase::LoginClient => "C->S login",
            SessionPhase::GameClient => "C->S game",
            _ => "unknown",
        };
        inbound.add(Box::new(LogFilter::new(log_name)));

        (inbound, outbound)
    }

    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut session: Session,
    ) -> error::Result<()> {
        let addr = ctx.addr;
        let is_game = matches!(&ctx.protocol, Protocol::Game(_));

        loop {
            match session.recv().await.event {
                SessionEvent::Seed(_) => {}
                SessionEvent::Packet(p) => {
                    let response = if is_game {
                        self.handle_game(addr, &p)
                    } else {
                        self.handle_login(addr, ctx, &p)
                    };
                    if let Some(packets) = response {
                        for resp in packets {
                            session.send(resp).await?;
                        }
                    }
                }
                SessionEvent::Stopped => { debug!("[{addr}] stopped"); break; }
                SessionEvent::Disconnected => { debug!("[{addr}] disconnected"); break; }
                SessionEvent::Error(e) => {
                    error!("[{addr}] error: {e}");
                    return Err(NetworkError::Transport(e));
                }
            }
        }

        session.close().await;
        Ok(())
    }
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logger().build()?;

    let server_addr = SocketAddrV4::new(SERVER_IP, SERVER_PORT);
    let config = ListenerConfig::new(LISTEN_ADDR);
    let handler = TestServer::new(server_addr);

    Listener::new(config, handler).run().await?;

    Ok(())
}
