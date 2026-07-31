//! Active connections for login and game phases.

use log::{debug, info};

use protocol::RawPacket;
use protocol::packets::character::CharacterLocaleAndBody;
use protocol::packets::login::*;
use protocol::packets::redirect::ServerRedirect;
use protocol::packets::seed::{ExtendedSeed, Seed};
use protocol::packets::system::EnableFeatures;
use u_io::{BE, BinaryWriter, Encode};
use u_core::ProtocolVersion;
use protocol::prelude::{ManualPacket, BasicPacket, encode_packet};
use protocol::Protocol;
use protocol::transport::builder::TransportBuilder;
use protocol::connector::{ConnectorConfig, connect};

use crate::error::{self, NetworkError};
use crate::logs;
use crate::session::{RecvResult, Session, SessionEvent};

/// An active login-phase connection.
///
/// Provides both low-level packet access and high-level flow methods
/// for completing the login sequence.
///
/// # High-level usage
///
/// ```rust,no_run
/// # async fn example(login: &mut network::client::LoginConnection) -> network::error::Result<()> {
/// login.authenticate("account", "password").await?;
/// let redirect = login.select_server(0).await?;
/// # Ok(())
/// # }
/// ```
pub struct LoginConnection {
    pub(crate) session: Session,
    pub(crate) seed: u32,
    pub(crate) version: ProtocolVersion,
    pub(crate) encrypted: bool,
    pub(crate) connector: ConnectorConfig,
}

impl LoginConnection {
    /// Receive the next event from the login server.
    pub async fn recv(&mut self) -> RecvResult {
        self.session.recv().await
    }

    /// Send a raw packet to the login server.
    pub async fn send(&mut self, packet: RawPacket) -> error::Result<()> {
        self.session.send(packet).await?;
        Ok(())
    }

    /// Send a typed packet to the login server.
    pub async fn send_packet<T: BasicPacket>(&mut self, packet: &T) -> error::Result<()> {
        self.session.send_packet(packet).await?;
        Ok(())
    }

    /// Close the login connection.
    pub async fn close(&mut self) {
        self.session.close().await;
    }

    /// Get a mutable reference to the underlying session.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    // ── High-level flow methods ────────────────────────────────────────

    /// Send the login seed and account credentials.
    ///
    /// For versions >= EXT_SEED (7.0.0), sends the extended 21-byte seed (packet 0xEF).
    /// For older versions, sends the standard 4-byte seed.
    /// Then sends packet 0x80 (AccountLogin).
    pub async fn authenticate(
        &mut self,
        account: &str,
        password: &str,
    ) -> error::Result<()> {
        if self.version >= ProtocolVersion::EXT_SEED_CLIENT {
            let ext_seed = ExtendedSeed::from_version(self.seed, self.version);
            self.session.send_seed(encode_packet(&ext_seed)).await?;
        } else {
            let seed_packet = Seed::new(self.seed);
            let mut writer = BinaryWriter::<BE>::new();
            seed_packet.encode(&mut writer);
            self.session.send_seed(writer.finish()).await?;
        }

        let login_packet = AccountLogin::new(account, password);
        self.session.send(RawPacket::c2s(encode_packet(&login_packet))).await?;

        Ok(())
    }

    /// Wait for the server list, select a server, and wait for the redirect.    ///
    /// Handles the following exchange:
    /// 1. Receives 0xA8 (ServerList) from server
    /// 2. Sends 0xA0 (SelectServer) with the given index
    /// 3. Receives 0x8C (ServerRedirect) from server
    ///
    /// Returns the parsed [`ServerRedirect`] on success.
    /// Returns an error if 0x82 (LoginDenied) is received or the server disconnects.
    pub async fn select_server(
        &mut self,
        server_index: u16,
    ) -> error::Result<ServerRedirect> {
        loop {
            match self.session.recv().await.event {
                SessionEvent::Packet(p) => {
                    match p.id() {
                        0xA8 => {
                            debug!(target: logs::CLIENT, "received server list ({} bytes), selecting server {server_index}",
                                p.data.len());
                            let select = SelectServer::new(server_index);
                            self.session.send(RawPacket::c2s(encode_packet(&select))).await?;
                        }
                        0x8C => {
                            let redirect = ServerRedirect::from_bytes(&p.data)
                                .map_err(|e| NetworkError::ProtocolError(format!("failed to parse redirect 0x8C: {}", e)))?;
                            return Ok(redirect);
                        }
                        0x82 => {
                            let denied = LoginDenied::from_bytes(&p.data)
                                .map_err(|e| NetworkError::ProtocolError(format!("failed to parse login denied packet 0x82: {}", e)))?;
                            return Err(NetworkError::LoginDenied(denied));
                        }
                        id => {
                            debug!(target: logs::CLIENT, "login: recv packet 0x{id:02X} ({} bytes)", p.data.len());
                        }
                    }
                }
                SessionEvent::Disconnected => return Err(NetworkError::Disconnected),
                SessionEvent::Error(e) => return Err(NetworkError::Transport(e)),
                _ => {}
            }
        }
    }

    /// Transition to a game-phase connection using redirect info.
    ///
    /// Closes the login session, connects to the game server specified
    /// in the redirect, and returns a [`GameConnection`]. Uses
    /// `redirect.auth_key` as both the game seed and authentication key
    /// (matching real UO client behavior).
    ///
    /// # Arguments
    /// * `redirect` - Server redirect parsed from packet 0x8C
    pub async fn into_game(
        mut self,
        redirect: &ServerRedirect,
    ) -> error::Result<GameConnection> {
        self.session.close().await;

        let addr = redirect.address().to_string();
        let auth_key = redirect.auth_key;
        let stream = connect(&self.connector, &addr).await?;

        let protocol = Protocol::game(auth_key, auth_key, self.version, self.encrypted);
        let (transport, direction) = TransportBuilder::client(stream, &protocol).build()?;
        let session = Session::new(transport, direction);

        Ok(GameConnection { session, auth_key, features: 0 })
    }
}


/// Result of a successful game login via [`GameConnection::enter_world`].
///
/// Contains the selected character's name and its initial world position
/// as reported by the server's `0x1B CharacterLocaleAndBody` packet.
#[derive(Debug, Clone)]
pub struct CharacterLoginInfo {
    /// Character name.
    pub name: String,
    /// Serial (unique id) of the character.
    pub serial: u32,
    /// Body type / graphic id.
    pub body_type: u16,
    /// X coordinate.
    pub x: u16,
    /// Y coordinate.
    pub y: u16,
    /// Z coordinate.
    pub z: i8,
    /// Facing direction (0-7).
    pub facing: u8,
}


/// An active game-phase connection.
///
/// Provides both low-level packet access and high-level flow methods
/// for completing game login (character selection).
pub struct GameConnection {
    pub(crate) session: Session,
    pub(crate) auth_key: u32,
    /// Feature flags received from packet 0xB9 (EnableFeatures).
    /// Populated during [`Self::receive_character_list`] when the server
    /// sends 0xB9 before the character list.
    pub(crate) features: u32,
}

impl GameConnection {
    /// Receive the next event from the game server.
    pub async fn recv(&mut self) -> RecvResult {
        self.session.recv().await
    }

    /// Send a raw packet to the game server.
    pub async fn send(&mut self, packet: RawPacket) -> error::Result<()> {
        self.session.send(packet).await?;
        Ok(())
    }

    /// Send a typed packet to the game server.
    pub async fn send_packet<T: BasicPacket>(&mut self, packet: &T) -> error::Result<()> {
        self.session.send_packet(packet).await?;
        Ok(())
    }

    /// Send multiple raw packets to the game server.
    pub async fn send_all(&mut self, packets: Vec<RawPacket>) -> error::Result<()> {
        self.session.send_all(packets).await?;
        Ok(())
    }

    /// Close the game connection.
    pub async fn close(&mut self) {
        self.session.close().await;
    }

    /// Get a mutable reference to the underlying session.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Feature flags received from the server (packet 0xB9).
    ///
    /// Populated during [`Self::receive_character_list`] when the server sends
    /// 0xB9 before the character list. Returns 0 if not yet received.
    pub fn features(&self) -> u32 {
        self.features
    }

    // ── High-level flow methods ────────────────────────────────────────

    /// Send the game seed and game login credentials.
    ///
    /// Sends the 4-byte seed followed by packet 0x91 (GameLogin).
    /// Uses the auth_key stored from the login phase redirect.
    pub async fn authenticate(
        &mut self,
        account: &str,
        password: &str,
    ) -> error::Result<()> {
        let seed_packet = Seed::new(self.auth_key);
        let mut writer = BinaryWriter::<BE>::new();
        seed_packet.encode(&mut writer);
        self.session.send_seed(writer.finish()).await?;

        let game_login = GameLogin::new(self.auth_key, account, password);
        self.session.send(RawPacket::c2s(encode_packet(&game_login))).await?;

        Ok(())
    }

    /// Wait for the character list (0xA9) from the server.    ///
    /// Also captures packet 0xB9 (EnableFeatures) if the server sends it
    /// before the character list, storing the feature flags in [`Self::features`].
    ///
    /// Returns the parsed [`CharacterList`] on success.
    /// Returns an error if 0x53 (LoginRejected) is received or the server disconnects.
    pub async fn receive_character_list(&mut self) -> error::Result<CharacterList> {
        loop {
            match self.session.recv().await.event {
                SessionEvent::Packet(p) => {
                    match p.id() {
                        0xB9 => {
                            let features = EnableFeatures::from_bytes(&p.data)
                                .map_err(|e| NetworkError::ProtocolError(format!("failed to parse features 0xB9: {}", e)))?;
                            self.features = features.flags();
                            debug!(target: logs::CLIENT, "received features flags: 0x{:04X}", self.features);
                        }
                        0xA9 => {
                            let list = CharacterList::from_bytes(&p.data)
                                .map_err(|e| NetworkError::ProtocolError(format!("failed to parse character list 0xA9: {}", e)))?;
                            return Ok(list);
                        }
                        0x53 => {
                            let rejected = LoginRejected::from_bytes(&p.data)
                                .map_err(|e| NetworkError::ProtocolError(format!("failed to parse login rejected 0x53: {}", e)))?;
                            return Err(NetworkError::LoginRejected(rejected));
                        }
                        id => {
                            debug!(target: logs::CLIENT, "game login: recv packet 0x{id:02X} ({} bytes)", p.data.len());
                        }
                    }
                }
                SessionEvent::Disconnected => return Err(NetworkError::Disconnected),
                SessionEvent::Error(e) => return Err(NetworkError::Transport(e)),
                _ => {}
            }
        }
    }

    /// Select a character by name and slot index.
    ///
    /// Sends packet 0x5D (LoginCharacter).
    pub async fn select_character(
        &mut self,
        name: &str,
        slot: u32,
    ) -> error::Result<()> {
        let login_char = LoginCharacter::new(name, slot);
        self.session.send(RawPacket::c2s(encode_packet(&login_char))).await?;
        Ok(())
    }

    /// Wait for 0x55 (LoginComplete) from the server.
    ///
    /// After selecting a character, the server sends a series of world-state
    /// packets (0x1B, 0x20, 0xBF, etc.) before the final 0x55 marker.
    /// This method consumes all of them, logging each, until 0x55 arrives.
    ///
    /// Returns the parsed `CharacterLocaleAndBody` (0x1B) if the server
    /// sent it, or `None` otherwise.
    /// Returns an error if the server disconnects before sending 0x55.
    pub async fn wait_for_login_complete(&mut self) -> error::Result<Option<CharacterLocaleAndBody>> {
        let mut locale = None;
        loop {
            match self.session.recv().await.event {
                SessionEvent::Packet(p) => {
                    match p.id() {
                        0x1B => {
                            match CharacterLocaleAndBody::from_bytes(&p.data) {
                                Ok(body) => {
                                    debug!(
                                        target: logs::CLIENT,
                                        "character locale: serial=0x{:08X} body={} pos=({},{},{}) facing={}",
                                        body.serial, body.body_type, body.x, body.y, body.z, body.facing,
                                    );
                                    locale = Some(body);
                                }
                                Err(e) => {
                                    debug!(target: logs::CLIENT, "failed to parse 0x1B: {e}");
                                }
                            }
                        }
                        0x55 => {
                            debug!(target: logs::CLIENT, "login complete (0x55)");
                            return Ok(locale);
                        }
                        id => {
                            debug!(target: logs::CLIENT, "world entry: recv packet 0x{id:02X} ({} bytes)", p.data.len());
                        }
                    }
                }
                SessionEvent::Disconnected => return Err(NetworkError::Disconnected),
                SessionEvent::Error(e) => return Err(NetworkError::Transport(e)),
                _ => {}
            }
        }
    }

    /// Full game entry sequence: authenticate, receive character list,
    /// select the first non-empty character, and wait for login complete.
    ///
    /// Returns [`CharacterLoginInfo`] with the character's name and
    /// initial position as reported by the server.
    pub async fn enter_world(
        &mut self,
        account: &str,
        password: &str,
    ) -> error::Result<CharacterLoginInfo> {
        self.authenticate(account, password).await?;

        let chars = self.receive_character_list().await?;
        let (slot, character) = chars.first_character()
            .ok_or_else(|| NetworkError::ProtocolError("no characters found on account".into()))?;

        let name = character.name.to_string();
        info!(target: logs::CLIENT, "selecting character '{name}' (slot {slot})");
        self.select_character(&name, slot as u32).await?;
        let locale = self.wait_for_login_complete().await?;

        let info = if let Some(body) = locale {
            CharacterLoginInfo {
                name,
                serial: body.serial,
                body_type: body.body_type,
                x: body.x,
                y: body.y,
                z: body.z,
                facing: body.facing,
            }
        } else {
            CharacterLoginInfo {
                name,
                serial: 0,
                body_type: 0,
                x: 0,
                y: 0,
                z: 0,
                facing: 0,
            }
        };

        Ok(info)
    }
}
