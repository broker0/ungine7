//! Minimal UO proxy using network.
//!
//! This example demonstrates how to set up a fully functional UO proxy
//! with packet logging in just a few lines of code.

mod packet_logger;

use std::net::{Ipv4Addr, SocketAddrV4};

use u_core::ProtocolVersion;
use protocol::transport::builder::TransportBuilder;

use tokio::net::TcpStream;

use network::error;
use network::handler::redirect::RedirectHandler;
use network::handler::HandlerChain;
use network::session::Session;
use network::listener::{
    ConnectionContext, ListenerConfig, ListenerHandler, SessionPhase, Listener,
};
use network::relay;

use common::logging::init_logger;
use common::handlers::SubcommandFilter;
use protocol::logs;
use protocol::Protocol;

use crate::packet_logger::PacketLogger;

const REAL_SERVER: &str = "127.0.0.1:2593";

struct SimpleProxy {
    proxy_addr: SocketAddrV4,
    version: ProtocolVersion,
    server_addr: String,
}

impl SimpleProxy {
    fn new(proxy_addr: SocketAddrV4, version: ProtocolVersion, server_addr: impl Into<String>) -> Self {
        Self { proxy_addr, version, server_addr: server_addr.into() }
    }
}

#[async_trait::async_trait]
impl ListenerHandler for SimpleProxy {
    fn configure_handlers(
        &self,
        phase: SessionPhase,
        ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        let mut inbound = HandlerChain::new();
        let outbound = HandlerChain::new();

        match phase {
            SessionPhase::LoginClient => {
                inbound.add(Box::new(PacketLogger::new("C->P login")));
            }
            SessionPhase::LoginServer => {
                inbound.add(Box::new(PacketLogger::new("S->P login")));
                inbound.add(Box::new(RedirectHandler::new(
                    self.proxy_addr,
                    ctx.binder.clone(),
                    self.version,
                    true,
                    4,
                )));
            }
            SessionPhase::GameClient => {
                inbound.add(Box::new(PacketLogger::new("C->P game")));
            }
            SessionPhase::GameServer => {
                inbound.add(Box::new(PacketLogger::new("S->P game")));
                inbound.add(Box::new(SubcommandFilter));
            }
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
            Protocol::Game(_) => SessionPhase::GameServer,
        };

        // Use the real game server address from the binder when available
        // (game-phase connections on OSI route to a different host than login).
        let target = ctx.game_server_address
            .map(|a| format!("{}:{}", a.ip(), a.port()))
            .unwrap_or_else(|| self.server_addr.clone());

        let server_stream = TcpStream::connect(&target).await?;
        let (transport, direction) = TransportBuilder::client(server_stream, &ctx.protocol).build()?;

        let (inbound, outbound) = self.configure_handlers(server_phase, ctx);
        let mut server_session = Session::with_handlers(transport, direction, inbound, outbound);

        relay::relay("[proxy]", &mut client_session, &mut server_session, None).await?;
        Ok(())
    }
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logger().debug().filters(["packet_logger", logs::DETECTION]) .build()?;
    // init_logger().build()?;

    let proxy_addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 2593);
    let config = ListenerConfig::new("0.0.0.0:2593")
        .with_allowed(vec![
            (Some(ProtocolVersion::BLOWFISH_TWOFISH_CLIENT), Some(true)),
            (Some(ProtocolVersion::new(3, 0, 8, 0)), Some(true))]);
    let handler = SimpleProxy::new(proxy_addr, ProtocolVersion::new(3, 0, 8, 0), REAL_SERVER);

    Listener::new(config, handler).run().await?;
    Ok(())
}
