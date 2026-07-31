//! Example UO client using network.
//!
//! Connects to a login server, completes the full login sequence
//! (account login → server select → game login → character select),
//! then stays in the game world responding to ping packets (0x73).
//!
//! Usage: set SERVER_ADDR, ACCOUNT, and PASSWORD below.

use log::{debug, error, info};

use protocol::RawPacket;
use protocol::packets::system::Ping;
use packets::traits::encode_packet;
use u_core::ProtocolVersion;
use network::session::SessionEvent;

use network::client::{ClientConfig, PacketClient};

use common::logging::init_logger;
use packets::traits::BasicPacket;
use protocol::connector::ConnectorConfig;
// ── Configuration ──────────────────────────────────────────────────────────

const SERVER_ADDR: &str = "127.0.0.1:2593";
const ACCOUNT: &str = "admin";
const PASSWORD: &str = "admin";
const CLIENT_VERSION: ProtocolVersion = ProtocolVersion::new(3, 0, 8, 0);
const LOGIN_SEED: u32 = 0xDEADBEEF;
const SERVER_INDEX: u16 = 1;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logger().build()?;

    let client = PacketClient::new(ClientConfig {
        version: CLIENT_VERSION,
        encrypted: true,
        connector: ConnectorConfig::Direct,
    });

    // ── Login phase ────────────────────────────────────────────────────

    info!("connecting to login server {SERVER_ADDR}...");
    let mut login = client.connect_login(SERVER_ADDR, LOGIN_SEED).await?;

    login.authenticate(ACCOUNT, PASSWORD).await?;
    let redirect = login.select_server(SERVER_INDEX).await?;
    info!("got redirect to {} (auth_key=0x{:08X})", redirect.address(), redirect.auth_key);

    // ── Game phase ─────────────────────────────────────────────────────

    info!("transitioning to game server...");
    let mut game = login.into_game(&redirect).await?;

    let char_info = game.enter_world(ACCOUNT, PASSWORD).await?;
    info!("entered world as '{}' at ({},{},{})", char_info.name, char_info.x, char_info.y, char_info.z);

    // ── Main loop: respond to pings ────────────────────────────────────

    loop {
        match game.recv().await.event {
            SessionEvent::Packet(p) => {
                if let Ok(ping) = Ping::from_bytes(&p.data) {
                    debug!("ping (seq={}), sending pong", ping.sequence);
                    game.send(RawPacket::c2s(encode_packet(&ping))).await?;
                } else {
                    debug!("recv packet 0x{:02X} ({} bytes)", p.id(), p.data.len());
                }
            }
            SessionEvent::Disconnected => {
                info!("server disconnected");
                break;
            }
            SessionEvent::Error(e) => {
                error!("session error: {e}");
                break;
            }
            _ => {}
        }
    }

    game.close().await;
    Ok(())
}
