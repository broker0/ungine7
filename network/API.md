# network

Application-level framework over `protocol`: sessions with handler pipelines, TCP listener, bidirectional relay, high-level client, and proxy utilities — everything needed to build UO servers, clients, and proxies without touching raw transport.

## Key Types

- **`Listener<H>`** (`listener::Listener`) — TCP listener that accepts connections, auto-detects protocol via `TcpConnectionDetector`, builds sessions, and dispatches them to a `ListenerHandler`. Supports graceful shutdown via `ListenerControl`.

- **`ListenerHandler`** (trait, `listener::ListenerHandler`) — Async trait for handling accepted connections. Provides hooks: `on_connect`, `build_transport`, `configure_handlers`, `handle_session`, `on_disconnect`. Only `handle_session` is required.

- **`ListenerConfig`** (`listener::ListenerConfig`) — Listener configuration: bind address, allowed version/encryption combos, custom detector, binder config.

- **`ConnectionContext`** (`listener::ConnectionContext`) — Per-connection context passed to handler methods: client address, detected `Protocol`, `ConnectionBinder`, and game-phase binding data.

- **`Session`** (`session::Session`) — Wraps a `PacketTransport` and applies inbound/outbound `HandlerChain` pipelines. `recv()` returns `RecvResult` (event + reply packets), `send()` writes a packet through outbound handlers.

- **`SessionBuilder`** (`session::SessionBuilder`) — Builder for `Session` with fluent `handler_inbound()` / `handler_outbound()` / `handler_both()`.

- **`SessionEvent`** (`session::SessionEvent`) — Events from `Session::recv()`: `Seed(Bytes)`, `Packet(RawPacket)`, `Disconnected`, `Stopped`, `Error(TransportError)`.

- **`PacketHandler`** (trait, `handler::packet_handler::PacketHandler`) — Trait for packet processing in the handler chain. Returns `HandlerAction` (Forward, Drop, Replace, Stop, Reply, etc.).

- **`HandlerChain`** (`handler::HandlerChain`) — Ordered pipeline of `PacketHandler` instances. Feeds each handler's output into the next.

- **`RedirectHandler`** (`handler::redirect::RedirectHandler`) — Intercepts packet 0x8C (ServerRedirect), registers a `PendingConnection` in the binder, rewrites the address to point at the proxy, and signals session stop.

- **`PacketClient`** (`client::PacketClient`) — High-level UO client. `connect_login()` and `connect_game()` create `LoginConnection` / `GameConnection` with full login flow methods.

- **`LoginConnection`** / **`GameConnection`** (`client`) — Active connections with high-level methods: `authenticate()`, `select_server()`, `into_game()`, `enter_world()`, plus low-level `recv()` / `send()`.

- **`relay()`** (`relay::relay`) — Bidirectional packet relay between two sessions with optional external `ConnectionControl::Disconnect`.

- **`NetworkError`** (`error::NetworkError`) — Unified error enum: `Detection`, `TransportBuild`, `Transport`, `Rejected`, `Io`, `LoginDenied`, `Disconnected`, etc.

## Usage

### Server — accept, detect, session loop

```rust
use network::listener::{ListenerConfig, ListenerHandler, ConnectionContext, SessionPhase, Listener};
use network::handler::HandlerChain;
use network::session::{RecvResult, Session, SessionEvent};
use network::error::{self, NetworkError};

struct MyServer;

#[async_trait::async_trait]
impl ListenerHandler for MyServer {
    fn configure_handlers(&self, _phase: SessionPhase, _ctx: &ConnectionContext)
        -> (HandlerChain, HandlerChain)
    {
        (HandlerChain::new(), HandlerChain::new())
    }

    async fn handle_session(&self, ctx: &ConnectionContext, mut session: Session)
        -> error::Result<()>
    {
        // Packet loop — detection and transport are handled by the listener
        loop {
            let RecvResult { event, replies } = session.recv().await;
            for reply in replies {
                session.send(reply).await?;
            }
            match event {
                SessionEvent::Seed(_) => {}
                SessionEvent::Packet(packet) => {
                    // parse packet.id(), build response, send back
                    session.send(response_packet).await?;
                }
                SessionEvent::Disconnected | SessionEvent::Stopped => break,
                SessionEvent::Error(e) => return Err(NetworkError::Transport(e)),
            }
        }
        session.close().await;
        Ok(())
    }
}

// Start listening — detection, transport build, handler wiring are automatic
let config = ListenerConfig::new("0.0.0.0:2593");
Listener::new(config, MyServer).run().await?;
```

### Client — login, redirect, enter world

```rust
use network::client::{ClientConfig, PacketClient};
use network::session::SessionEvent;
use protocol::connector::ConnectorConfig;
use protocol::ProtocolVersion;

// High-level client handles seed, transport, encryption automatically
let client = PacketClient::new(ClientConfig {
    version: ProtocolVersion::new(7, 0, 95, 0),
    encrypted: true,
    connector: ConnectorConfig::Direct,
});

// Login phase: connect -> authenticate -> select server -> get redirect
let mut login = client.connect_login("127.0.0.1:2593", 0xDEADBEEF).await?;
login.authenticate("account", "password").await?;
let redirect = login.select_server(0).await?;

// Game phase: seamless transition via redirect
let mut game = login.into_game(&redirect).await?;
let char_info = game.enter_world("account", "password").await?;

// Main game loop
loop {
    match game.recv().await.event {
        SessionEvent::Packet(p) => { /* handle game packets */ }
        SessionEvent::Disconnected => break,
        SessionEvent::Error(e) => break,
        _ => {}
    }
}
game.close().await;
```

### Proxy — RedirectHandler + relay

```rust
use network::listener::{ListenerConfig, ListenerHandler, ConnectionContext, SessionPhase, Listener};
use network::handler::redirect::RedirectHandler;
use network::handler::HandlerChain;
use network::session::Session;
use network::relay;
use network::error;
use protocol::transport::builder::TransportBuilder;
use std::net::SocketAddrV4;
use tokio::net::TcpStream;

struct MyProxy {
    proxy_addr: SocketAddrV4,
    server_addr: String,
}

#[async_trait::async_trait]
impl ListenerHandler for MyProxy {
    fn configure_handlers(&self, phase: SessionPhase, ctx: &ConnectionContext)
        -> (HandlerChain, HandlerChain)
    {
        let mut inbound = HandlerChain::new();
        if phase == SessionPhase::LoginServer {
            // Intercept 0x8C redirect — rewrites address, registers binder entry
            inbound.add(Box::new(RedirectHandler::new(
                self.proxy_addr, ctx.binder.clone(),
                ctx.protocol.client_version(), ctx.protocol.is_encrypted(), 4,
            )));
        }
        (inbound, HandlerChain::new())
    }

    async fn handle_session(&self, ctx: &ConnectionContext, mut client: Session)
        -> error::Result<()>
    {
        // Connect upstream and build server-side session
        let target = ctx.upstream_addr(&self.server_addr);
        let stream = TcpStream::connect(&target).await?;
        let (transport, dir) = TransportBuilder::client(stream, &ctx.protocol).build()?;
        let (in_h, out_h) = self.configure_handlers(
            SessionPhase::server_for(&ctx.protocol), ctx);
        let mut server = Session::with_handlers(transport, dir, in_h, out_h);

        // Bidirectional relay — forwards packets both ways, handles replies
        relay::relay("[proxy]", &mut client, &mut server, None).await
    }
}

let config = ListenerConfig::new("0.0.0.0:2593");
Listener::new(config, MyProxy { proxy_addr, server_addr }).run().await?;
```

### Custom PacketHandler — inspect / filter / modify packets

```rust
use network::handler::packet_handler::{HandlerAction, PacketHandler};
use network::session::SessionBuilder;
use protocol::RawPacket;
use u_core::PacketDirection;

#[derive(Debug)]
struct PacketLogger { label: String }

impl PacketHandler for PacketLogger {
    fn name(&self) -> &str { &self.label }

    fn handle(&mut self, dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        log::debug!("[{}] 0x{:02X} ({} bytes)", self.label, packet.id(), packet.len());
        HandlerAction::Forward(packet) // pass through unchanged
    }
}

// Attach to session via builder or configure_handlers
let session = SessionBuilder::new(transport, direction)
    .handler_inbound(Box::new(PacketLogger { label: "C->S".into() }))
    .handler_outbound(Box::new(PacketLogger { label: "S->C".into() }))
    .build();
```

### Shutdown control

```rust
use network::listener::ListenerControl;

let (control_tx, control_rx) = tokio::sync::mpsc::channel(1);

// Run listener with external control channel
tokio::spawn(async move {
    listener.run_with_control(control_rx).await.unwrap();
});

// Graceful shutdown from another task
control_tx.send(ListenerControl::Shutdown).await.unwrap();
```

## Secondary API

### HandlerAction — all variants

```rust
enum HandlerAction {
    Forward(RawPacket),                                      // pass through
    Drop,                                                     // silently consume
    Replace(Vec<RawPacket>),                                  // replace with 0..N packets
    Stop(RawPacket),                                          // forward now; next recv returns Stopped
    StopDrop,                                                 // drop + signal session stop
    ForwardAndReply { forward: RawPacket, reply: Vec<RawPacket> },  // forward + send replies back
    Reply(Vec<RawPacket>),                                    // drop + send replies back
}
```

### HandlerChain

```rust
impl HandlerChain {
    fn new() -> Self
    fn add(&mut self, handler: Box<dyn PacketHandler>)
    fn is_empty(&self) -> bool
    fn process(&mut self, packet: RawPacket, dir: PacketDirection) -> HandlerResult
    fn notify_start(&mut self)               // calls on_start() on all handlers
    fn notify_close(&mut self)               // calls on_close() on all handlers
}
```

### Session — additional methods

```rust
impl Session {
    fn direction(&self) -> PacketDirection
    async fn send_packet<T: BasicPacket>(&mut self, packet: &T) -> error::Result<SendResult>  // typed packet
    async fn send_buffered(&mut self, packet: RawPacket) -> error::Result<SendResult>       // buffer without flush
    async fn flush(&mut self) -> error::Result<()>
    async fn send_all(&mut self, packets: Vec<RawPacket>) -> error::Result<SendResult>      // batch send
    async fn send_seed(&mut self, data: Bytes) -> error::Result<()>
    async fn close(&mut self)
}
```

### PacketSink — trait for abstracting packet output

```rust
#[async_trait::async_trait]
trait PacketSink: Send {
    async fn send_packet(&mut self, packet: RawPacket) -> error::Result<()>;
}
// Implemented for Session and mpsc::Sender<RawPacket>
```

### ListenerHandler — default-provided methods

```rust
#[async_trait::async_trait]
trait ListenerHandler: Send + Sync + 'static {
    // Required:
    async fn handle_session(&self, ctx: &ConnectionContext, session: Session) -> error::Result<()>;

    // Optional (have defaults):
    fn configure_handlers(&self, phase: SessionPhase, ctx: &ConnectionContext)
        -> (HandlerChain, HandlerChain);                                    // default: empty chains
    fn build_transport(&self, stream: TcpStream, ctx: &ConnectionContext)
        -> std::result::Result<(Box<dyn PacketTransport>, PacketDirection), TransportBuildError>;  // default: standard server
    async fn on_connect(&self, ctx: &ConnectionContext) -> bool;           // default: true (accept)
    async fn on_disconnect(&self, addr: SocketAddr, result: &error::Result<()>);  // default: no-op
}
```

### ListenerConfig — builder methods

```rust
impl ListenerConfig {
    fn new(listen_addr: impl Into<String>) -> Self
    fn with_allowed(self, allowed: Vec<(Option<ProtocolVersion>, Option<bool>)>) -> Self
    fn with_required_version(self, version: ProtocolVersion) -> Self
    fn with_required_encryption(self, encrypted: bool) -> Self
    fn with_detector(self, detector: TcpConnectionDetector) -> Self
    fn with_binder_config(self, config: BinderConfig) -> Self
}
```

### ListenerControl — shutdown commands

```rust
enum ListenerControl {
    Shutdown,       // stop accepting, wait for active connections
    ForceShutdown,  // stop accepting, abort active connections
    StopListening,  // stop accepting, let active connections finish independently
}
```

### SessionPhase

```rust
enum SessionPhase { LoginClient, LoginServer, GameClient, GameServer }

impl SessionPhase {
    fn client_for(protocol: &Protocol) -> Self
    fn server_for(protocol: &Protocol) -> Self
}
```

### Built-in Handlers

```rust
// Logs every packet at debug/trace level, forwards unchanged
LogHandler::new(label: impl Into<String>) -> Self

// Drops packets matching packet_id + subcommand. Default: drops 0xBF/0xFACE
SubcommandFilter::new(packet_id: u8, blocked: Vec<u16>) -> Self

// Intercepts 0x8C, registers binder entry, rewrites address, signals stop
RedirectHandler::new(proxy_address, binder, client_version, encrypted, seed_size) -> Self
RedirectHandler::with_context<F>(self, factory: F) -> Self   // attach custom data to PendingConnection
```

### Proxy Utility

```rust
// Connect upstream, build server-side Session with handler chains
async fn connect_upstream(
    ctx: &ConnectionContext,
    target_addr: &str,
    handler: &dyn ListenerHandler,
) -> error::Result<Session>
```

### Relay control

```rust
enum ConnectionControl {
    Disconnect,
}

// The receiver is owned by relay; closing it also ends the relay.
async fn relay(
    label: &str,
    client: &mut Session,
    server: &mut Session,
    control: Option<tokio::sync::mpsc::Receiver<ConnectionControl>>,
) -> error::Result<()>;
```

### ClientConfig

```rust
struct ClientConfig {
    version: ProtocolVersion,       // default: ProtocolVersion::SA_CLIENT
    encrypted: bool,                // default: false
    connector: ConnectorConfig,     // default: ConnectorConfig::Direct
}
```

### PacketClient

```rust
impl PacketClient {
    async fn connect_login(&self, addr: &str, seed: u32) -> error::Result<LoginConnection>
    async fn connect_game(&self, addr: &str, seed: u32, auth_key: u32) -> error::Result<GameConnection>
}
```

### LoginConnection — additional methods

```rust
impl LoginConnection {
    async fn recv(&mut self) -> RecvResult
    async fn send(&mut self, packet: RawPacket) -> error::Result<()>
    async fn send_packet<T: BasicPacket>(&mut self, packet: &T) -> error::Result<()>
    async fn close(&mut self)
    fn session_mut(&mut self) -> &mut Session
}
```

### GameConnection — additional methods

```rust
impl GameConnection {
    async fn recv(&mut self) -> RecvResult
    async fn send(&mut self, packet: RawPacket) -> error::Result<()>
    async fn send_packet<T: BasicPacket>(&mut self, packet: &T) -> error::Result<()>
    async fn send_all(&mut self, packets: Vec<RawPacket>) -> error::Result<()>
    async fn close(&mut self)
    fn session_mut(&mut self) -> &mut Session
    fn features(&self) -> u32
    async fn receive_character_list(&mut self) -> error::Result<CharacterList>
    async fn select_character(&mut self, name: &str, slot: u32) -> error::Result<()>
    async fn wait_for_login_complete(&mut self) -> error::Result<Option<CharacterLocaleAndBody>>
}
```

### NetworkError — all variants

```rust
enum NetworkError {
    Detection(DetectionError),
    TransportBuild(TransportBuildError),
    Transport(TransportError),
    Rejected(String),
    Io(std::io::Error),
    NoPendingConnection(u32),
    NoGameServerAddress,
    LoginDenied(LoginDenied),
    LoginRejected(LoginRejected),
    Disconnected,
    ProtocolError(String),
}
```

### Log Targets — for `env_logger` / `log` filtering

```rust
"network::listener"   // listener lifecycle
"network::session"    // per-packet recv/send
"network::handler"    // handler chain processing
"network::filter"     // LogHandler, SubcommandFilter
"network::redirect"   // RedirectHandler
"network::relay"      // bidirectional relay
"network::client"     // PacketClient, LoginConnection, GameConnection
"network::proxy"      // connect_upstream
```

## Architecture

```
                          Listener
                              |
               ┌──────────────┼──────────────┐
               v              v              v
          TCP accept    detect protocol   check allowed
               |              |
               v              v
        ListenerHandler::build_transport()
               |
               v
        ListenerHandler::configure_handlers()
               |
               v
           Session (transport + inbound/outbound HandlerChain)
               |
               v
        ListenerHandler::handle_session()
```

Proxy relay wiring:

```
  UO Client  <-->  Session (server role)  <--relay-->  Session (client role)  <-->  Real Server
                   inbound: RedirectHandler             inbound: LogHandler, etc.
```

Client connection flow:

```
PacketClient::connect_login()  -->  LoginConnection
    .authenticate()
    .select_server()       -->  ServerRedirect
    .into_game()           -->  GameConnection
        .enter_world()     -->  CharacterLoginInfo
        .recv() loop
```
