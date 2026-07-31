//! High-level UO client for connecting to servers.
//!
//! Provides two levels of abstraction:
//!
//! - **Low-level:** [`PacketClient::connect_login`] and [`PacketClient::connect_game`]
//!   give you raw [`LoginConnection`] / [`GameConnection`] objects with full
//!   control over the packet flow.
//!
//! - **Transition:** [`LoginConnection::into_game`] handles the login->game
//!   server transition using redirect information from packet 0x8C.
//!
//! # Example: Low-level login
//!
//! ```rust,no_run
//! use network::client::{PacketClient, ClientConfig};
//! use u_core::ProtocolVersion;
//!
//! # async fn example() -> Result<(), network::error::NetworkError> {
//! let client = PacketClient::new(ClientConfig {
//!     version: ProtocolVersion::SA_CLIENT,
//!     encrypted: false,
//!     ..Default::default()
//! });
//!
//! let mut login = client.connect_login("127.0.0.1:2593", 0xDEADBEEF).await?;
//!
//! // Send login credentials, receive server list, etc.
//! // ...
//!
//! login.close().await;
//! # Ok(())
//! # }
//! ```

pub mod connection;

use protocol::Protocol;
use protocol::transport::builder::TransportBuilder;
use protocol::connector::{ConnectorConfig, connect};

use crate::error;
use crate::session::Session;

pub use connection::{LoginConnection, GameConnection, CharacterLoginInfo};
use u_core::ProtocolVersion;

/// Configuration for [`PacketClient`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Client version to use for encryption and packet table selection.
    pub version: ProtocolVersion,

    /// Whether to use encryption. Set to `false` for unofficial shards
    /// that disable encryption.
    pub encrypted: bool,

    /// Connection method: direct or through a proxy.
    /// Defaults to [`ConnectorConfig::Direct`].
    pub connector: ConnectorConfig,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            version: ProtocolVersion::SA_CLIENT,
            encrypted: false,
            connector: ConnectorConfig::Direct,
        }
    }
}


/// High-level UO client.
///
/// Creates connections to login and game servers with the correct
/// encryption and packet framing configured automatically.
#[derive(Debug, Clone)]
pub struct PacketClient {
    config: ClientConfig,
}

impl PacketClient {
    /// Create a new client with the given configuration.
    pub fn new(config: ClientConfig) -> Self {
        Self { config }
    }

    /// Connect to a login server.
    ///
    /// Returns a [`LoginConnection`] that can be used to send/receive
    /// login-phase packets (account login, server list, etc.).
    ///
    /// # Arguments
    /// * `addr` - Server address (e.g. `"127.0.0.1:2593"`)
    /// * `seed` - Login seed value (typically a non-zero random u32)
    pub async fn connect_login(
        &self,
        addr: &str,
        seed: u32,
    ) -> error::Result<LoginConnection> {
        let stream = connect(&self.config.connector, addr).await?;

        let protocol = Protocol::login(seed, self.config.version, self.config.encrypted);
        let (transport, direction) = TransportBuilder::client(stream, &protocol).build()?;
        let session = Session::new(transport, direction);

        Ok(LoginConnection {
            session,
            seed,
            version: self.config.version,
            encrypted: self.config.encrypted,
            connector: self.config.connector.clone(),
        })
    }

    /// Connect directly to a game server.
    ///
    /// This is a low-level method for connecting to a game server
    /// without going through the login phase. Useful for testing
    /// or when you already have the game server address and auth key.
    ///
    /// # Arguments
    /// * `addr` - Game server address (e.g. `"127.0.0.1:5003"`)
    /// * `seed` - Game seed value
    /// * `auth_key` - Authentication key (from login phase redirect)
    pub async fn connect_game(
        &self,
        addr: &str,
        seed: u32,
        auth_key: u32,
    ) -> error::Result<GameConnection> {
        let stream = connect(&self.config.connector, addr).await?;

        let protocol = Protocol::game(seed, auth_key, self.config.version, self.config.encrypted);
        let (transport, direction) = TransportBuilder::client(stream, &protocol).build()?;
        let session = Session::new(transport, direction);

        Ok(GameConnection { session, auth_key, features: 0 })
    }
}
