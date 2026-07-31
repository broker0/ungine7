# protocol

Ultima Online wire protocol library: connection detection, encryption/decryption, packet framing, and async transport — everything needed to speak the UO binary protocol over TCP.

## Key Types

- **`Protocol`** (`protocol::Protocol`) — Login or Game protocol descriptor carrying seed, client version, and encryption flag. Constructed via `Protocol::login(seed, version, encrypted)` or `Protocol::game(seed, auth_key, version, encrypted)`.

- **`TcpConnectionDetector`** (`detection::tcp_detector::TcpConnectionDetector`) — Peeks at a TCP stream (without consuming bytes) to auto-detect the protocol phase and client version using pluggable strategies.

- **`VersionDatabase`** (`detection::version_db::VersionDatabase`) — Collection of known client versions used for brute-force encryption detection. `VersionDatabase::common()` covers popular versions out of the box.

- **`TransportBuilder`** (`transport::builder::TransportBuilder`) — Builder for `PacketTransport`. Creates the full TCP -> Crypto -> Codec pipeline via `TransportBuilder::server(stream, &protocol)` or `::client(stream, &protocol)`.

- **`PacketTransport`** (trait, `transport::PacketTransport`) — Async packet-level I/O: `recv()` returns `TransportEvent`, `send()` writes one, and `close()` shuts down the transport. The main abstraction applications interact with.

- **`TransportEvent`** (`transport::TransportEvent`) — Either `Seed(Bytes)` (the initial handshake seed) or `Packet(Bytes)` (a framed packet).

- **`RawPacket`** (`RawPacket`, also `codec::packet::RawPacket`) — Packet bytes + direction. Factory methods: `RawPacket::s2c(bytes)`, `RawPacket::c2s(bytes)`, `RawPacket::new(bytes, direction)`. Access via `.id()`, `.data`, `.len()`.

- **`ConnectionBinder`** (`binder::ConnectionBinder`) — Thread-safe map that links login-phase auth keys to game-phase connections. `register()` stores a `PendingConnection` after login; `bind()` consumes it when the game phase arrives.

- **`PendingConnection`** / **`BoundConnection`** (`binder`) — Data carried through the binder: `auth_key`, `client_version`, `encrypted`, `seed_size`, `game_server_address`, and a `context: Box<dyn Any + Send + Sync>` slot for application data.

- **`ConnectorConfig`** (`connector::ConnectorConfig`) — `Direct` (plain TCP) or `Socks5 { proxy_addr, auth }` (requires `proxy` feature). Used with `connect(&config, addr).await`.

- **`ProtocolVersion`** (`ProtocolVersion`, also `protocol::ProtocolVersion`) — Client version tuple `(major, minor, patch, build)`. Has constants like `ProtocolVersion::BLOWFISH_TWOFISH_CLIENT` for version thresholds.

## Usage

### Server — detect, transport, recv/send, binder

```rust
use protocol::detection::tcp_detector::TcpConnectionDetector;
use protocol::detection::version_db::VersionDatabase;
use protocol::transport::builder::TransportBuilder;
use protocol::transport::{TransportEvent, TransportError};
use protocol::binder::{ConnectionBinder, BinderConfig, PendingConnection};
use protocol::protocol::Protocol;
use protocol::RawPacket;

// 1. Accept TCP and detect protocol (login or game phase)
let detector = TcpConnectionDetector::standard(VersionDatabase::common());
let protocol = detector.detect(&stream).await?;

// 2. Build server-side transport (TCP -> Crypto -> Codec)
let (mut transport, direction) = TransportBuilder::server(stream, &protocol)
    .build()?;

// 3. Packet loop
loop {
    match transport.recv().await {
        Ok(TransportEvent::Seed(seed)) => { /* initial seed bytes */ }
        Ok(TransportEvent::Packet(data)) => {
            let packet = RawPacket::new(data, direction);
            // parse typed packet from raw bytes, build response, etc.
        }
        Err(TransportError::Closed) => break,
        Err(e) => { /* handle error */ }
    }
}

// 4. Register login->game binding (after intercepting 0x8C redirect).
// `auth_key` and `addr` come from that redirect packet.
let binder = ConnectionBinder::new(BinderConfig::default());
binder.register(PendingConnection {
    auth_key,
    client_version: protocol.client_version(),
    game_server_address: Some(addr),
    encrypted: protocol.is_encrypted(),
    seed_size: protocol.seed_size(),
    created_at: std::time::Instant::now(),
    context: Box::new(()), // or any application-specific data
})?;

// 5. On the separately accepted game connection, resolve the binding from
// its detected protocol.
let game_protocol = detector.detect(&game_stream).await?;
let Protocol::Game(game_info) = game_protocol else { unreachable!() };
let bound = binder.bind(game_info.auth_key);
```

### Client — connect, build transport, authenticate

```rust
use bytes::Bytes;
use protocol::connector::{ConnectorConfig, connect};
use protocol::protocol::Protocol;
use protocol::transport::builder::TransportBuilder;
use protocol::transport::TransportEvent;
use protocol::{ProtocolVersion, RawPacket};

// Connect and build client-side transport
let stream = connect(&ConnectorConfig::Direct, "127.0.0.1:2593").await?;
let seed = 0xDEADBEEF;
let protocol = Protocol::login(seed, ProtocolVersion::new(7, 0, 95, 0), true);
let (mut transport, direction) = TransportBuilder::client(stream, &protocol)
    .build()?;

// Send seed, then packets
transport.send(TransportEvent::Seed(Bytes::copy_from_slice(&seed.to_be_bytes()))).await?;
// Encode the appropriate login packet for the selected client version.
let login_packet_bytes = Bytes::from_static(&[0x80 /* LoginRequest payload */]);
transport.send(TransportEvent::Packet(login_packet_bytes)).await?;
transport.flush().await?;

// Receive response
match transport.recv().await? {
    TransportEvent::Packet(data) => {
        let packet = RawPacket::new(data, direction);
        // parse server list, character list, etc.
    }
    _ => {}
}
```

### Proxy — two transports, redirect rewrite, relay

```rust
use protocol::binder::{ConnectionBinder, PendingConnection};
use protocol::transport::builder::TransportBuilder;
use protocol::transport::TransportEvent;

// Client-side: proxy acts as SERVER to the UO client
let (mut client_tr, _) = TransportBuilder::server(client_stream, &protocol).build()?;

// Server-side: proxy acts as CLIENT to the real server
let (mut server_tr, _) = TransportBuilder::client(server_stream, &protocol).build()?;

// Bidirectional relay with packet inspection
loop {
    tokio::select! {
        event = client_tr.recv() => {
            // forward client -> server
            server_tr.send(event?).await?;
        }
        event = server_tr.recv() => {
            let ev = event?;
            if let TransportEvent::Packet(ref data) = ev {
                if data[0] == 0x8C {
                    // Intercept ServerRedirect: rewrite address to proxy,
                    // register real server address in binder for game phase.
                    binder.register(PendingConnection { /* redirect-derived fields */ })?;
                    client_tr.send(TransportEvent::Packet(rewritten)).await?;
                    continue;
                }
            }
            client_tr.send(ev).await?;
        }
    }
}
```

### Testing — MemoryTransport (in-process, no TCP)

```rust
use protocol::transport::memory::MemoryTransport;
use protocol::transport::{PacketTransport, TransportEvent};

// Create bidirectional in-memory channel
let (mem_transport, mut handle) = MemoryTransport::channel(32);
let mut transport: Box<dyn PacketTransport> = Box::new(mem_transport);

// One side uses `transport.recv()` / `transport.send()`
// Other side uses `handle.recv()` / `handle.send()`
handle.send(TransportEvent::Packet(data)).await?;
let event = transport.recv().await?;
```

### Custom Transport Pipeline — manual layer composition

```rust
use protocol::codec::encryption::cipher_pair;
use protocol::protocol::Role;
use protocol::transport::builder::TransportBuilder;
use protocol::transport::crypto_stream::CryptoStream;
use protocol::transport::tcp::TcpByteStream;

// Build layers manually (e.g. to insert logging between layers)
let tcp = TcpByteStream::new(stream);
let (enc, dec) = cipher_pair(&protocol, Role::Server);
let crypto = CryptoStream::new(tcp, enc, dec);

let (transport, direction) = TransportBuilder::server_with_stream(crypto, &protocol)
    .build()?;
```

## Secondary API

### Transport

```rust
// `transport::factory` functions used internally by TransportBuilder
fn server_transport(stream: TcpStream, protocol: &Protocol) -> TcpTransport
fn client_transport(stream: TcpStream, protocol: &Protocol) -> TcpTransport

// `transport::tcp` type alias for the standard 3-layer stack
type TcpTransport = CodecTransport<CryptoStream<TcpByteStream>>
```

### PacketTable — version-aware packet length lookup

```rust
impl PacketTable {
    fn new(version: ProtocolVersion) -> Self  // selects table by version
    fn get_packet_length(&self, data: &[u8]) -> Result<usize, PacketLengthError>
    fn is_dynamic(&self, cmd: u8) -> bool
    fn is_unknown(&self, cmd: u8) -> bool
}
```

### ProtocolStream (trait) — low-level async byte I/O

```rust
// Fundamental stream abstraction; CryptoStream and TcpByteStream implement it.
// Each method returns `impl Future<...> + Send` in the actual trait.
trait ProtocolStream: Send + Debug {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()>;
    async fn read_into(&mut self, buf: &mut BytesMut, len: usize) -> io::Result<()>;
    async fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    async fn flush(&mut self) -> io::Result<()>;
    async fn shutdown(&mut self);
    async fn read_seed(&mut self, buf: &mut [u8]) -> io::Result<()>;   // bypass crypto
    async fn write_seed(&mut self, data: &[u8]) -> io::Result<()>;     // bypass crypto
}
```

### DetectionStrategy (trait) — pluggable detection logic

```rust
trait DetectionStrategy: Send + Sync + Debug {
    fn name(&self) -> &str;
    fn min_bytes(&self) -> usize;
    fn detect(&self, buf: &[u8]) -> Result<Option<Protocol>, DetectionError>;
}

// Built-in implementations
LoginDetectionStrategy::new(version_db).with_expected_version(Some(v))
GameDetectionStrategy::new().with_version(v).with_binder(binder)
```

### TcpConnectionDetector — convenience constructors

```rust
impl TcpConnectionDetector {
    fn standard(version_db: VersionDatabase) -> Self
    fn standard_with_binder(version_db, binder: ConnectionBinder) -> Self
    fn standard_with_version(version_db, binder, expected_version) -> Self
    fn for_game_reconnect(version: ProtocolVersion) -> Self
}
```

### VersionDatabase — version list constructors

```rust
impl VersionDatabase {
    fn common() -> Self                                          // popular versions
    fn exhaustive(major_range, max_minor, max_rev) -> Self       // all combinations
    fn new(versions: Vec<ProtocolVersion>) -> Self
}
```

### ConnectionBinder — additional methods

```rust
impl ConnectionBinder {
    fn contains(&self, auth_key: u32) -> bool
    fn peek_version(&self, auth_key: u32) -> Option<ProtocolVersion>  // non-consuming
    fn pending_count(&self) -> usize
    fn remove(&self, auth_key: u32) -> bool
    fn gc(&self)                                                       // manual TTL cleanup
}
```

### Encryption — factory

```rust
// Returns (Encryptor, Decryptor) pair based on protocol phase, version, and role
fn cipher_pair(protocol: &Protocol, role: Role) -> (Box<dyn Encryptor>, Box<dyn Decryptor>)
fn no_encryption_encryptor() -> Box<dyn Encryptor>
fn no_encryption_decryptor() -> Box<dyn Decryptor>
```

### Typed Packet Helpers

```rust
// `protocol::packets::traits::from_raw_packet`: decode a typed packet from RawPacket
fn from_raw_packet<T: BasicPacket>(packet: &RawPacket) -> Result<T, PacketError>

// Re-exported packet modules
pub use protocol::packets::{character, login, redirect, seed, system};
```

### Connector

```rust
async fn connect(config: &ConnectorConfig, addr: &str) -> io::Result<TcpStream>

enum ConnectorConfig {
    Direct,
    Socks5 { proxy_addr: String, auth: Option<(String, String)> },  // feature = "proxy"
}
```

## Encoding Internals (not for direct use)

Modules `encoding::blowfish`, `encoding::twofish`, `encoding::huffman`, `encoding::md5`, `encoding::login` (XorLoginCrypt), `encoding::game` (TwofishGameCrypt, BlowfishGameCrypt, MD5GameCrypt), and `encoding::encoders` implement the UO encryption/compression stack. These are wired automatically by `cipher_pair()` and `TransportBuilder`; direct use is only needed for custom transport pipelines or debugging.

## Architecture

```
Application
    |
    v
PacketTransport::recv() / send()       <-- TransportEvent (Seed | Packet)
    |
CodecTransport                          <-- packet framing via PacketTable
    |
CryptoStream                           <-- encrypt/decrypt via cipher_pair()
    |
TcpByteStream                          <-- async TCP with BufWriter
    |
TcpStream (tokio)
```

Detection runs separately before transport is built:

```
TcpStream.peek()  -->  TcpConnectionDetector  -->  Protocol (Login | Game)
                            |
                   LoginDetectionStrategy   (seed parse + XOR trial decrypt)
                   GameDetectionStrategy    (seed parse + Twofish/Blowfish trial decrypt)
                            |
                       VersionDatabase      (brute-force version matching)
```

Login-to-game phase linking:

```
Login phase: intercept 0x8C  -->  ConnectionBinder::register(PendingConnection)
Game  phase: read auth_key   -->  ConnectionBinder::bind(auth_key) -> BoundConnection
```
