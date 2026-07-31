//! Virtual client session — unified loop for all UO client connections.
//!
//! Every client that connects — whether through the Source port
//! (initiating a session) or the Mirror port (joining an existing one)
//! — ultimately runs [`run_virtual_client`].
//!
//! The virtual client is a **transport layer** only.  All packet logic
//! (movement arbitration, target cursor management, etc.) lives in the
//! [`HeadlessClient`](super::headless).  The virtual client:
//!
//! - Forwards C→S packets via `ClientCommand::RawPacket`
//! - Receives per-client responses via its `sink_rx`
//! - Receives broadcast S→C packets via the `packet_tx` broadcast
//!
//! # Login Modes
//!
//! - [`LoginMode::InitiateSession`] — the first client that creates the
//!   session.  Relays the login handshake to the headless session, then
//!   receives a bootstrap via the command channel.
//!
//! - [`LoginMode::JoinExisting`] — a client joining an existing session.
//!   Performs a synthetic handshake (0x91 -> 0xB9/0xA9 -> 0x5D) and
//!   receives a bootstrap via the command channel.

use std::sync::Arc;

use log::{debug, info, warn};
use u_core::PacketDirection;
use framework::rythmos::ClientId;
use packets::login::{CharacterList, CharacterSlot, StartingLocation};
use packets::traits::BasicPacket;
use tokio::sync::{mpsc, oneshot};

use network::error as fw_error;
use network::session::{Session, SessionEvent};
use protocol::RawPacket;

use crate::registry::SessionEntry;
use crate::session::commands::ClientCommand;
use crate::session::headless::{LoginRelayC2S, LoginRelayS2C};
use crate::types::SessionId;

// ── LoginMode ─────────────────────────────────────────────────────────────

/// Determines how a virtual client handles the login handshake.
pub enum LoginMode {
    /// First client — relay login through HeadlessSession.
    InitiateSession {
        login_relay_tx: mpsc::Sender<LoginRelayC2S>,
        login_relay_rx: mpsc::Receiver<LoginRelayS2C>,
    },
    /// Joining an existing session — synthetic handshake.
    JoinExisting,
}

// ── Stable numeric IDs ────────────────────────────────────────────────────

/// Atomic counter for assigning unique client IDs.
static NEXT_CLIENT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn next_client_id() -> ClientId {
    NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ── VirtualClient entry point ─────────────────────────────────────────────

/// Unified session loop for ALL client connections.
///
/// After the login handshake (mode-dependent), the main loop is simple:
/// - C→S packets are forwarded to HeadlessClient via `ClientCommand::RawPacket`.
/// - S→C broadcast packets are forwarded to the UO client.
/// - Per-client arbiter responses arrive through a dedicated sink channel.
pub async fn run_virtual_client(
    mut uo_client: Session,
    entry: Arc<SessionEntry>,
    session_id: SessionId,
    login_mode: LoginMode,
) -> fw_error::Result<()> {
    let client_id = next_client_id();

    info!(
        "[session {}] virtual client {} started",
        session_id.0, client_id
    );

    // Run the login handshake based on mode.
    let login_ok = match login_mode {
        LoginMode::InitiateSession {
            login_relay_tx,
            mut login_relay_rx,
        } => {
            initiate_session_handshake(
                &mut uo_client,
                &entry,
                session_id,
                client_id,
                login_relay_tx,
                &mut login_relay_rx,
            )
            .await
        }
        LoginMode::JoinExisting => {
            join_existing_handshake(&mut uo_client, &entry, session_id, client_id).await
        }
    };

    if !login_ok {
        info!(
            "[session {}] virtual client {} login failed, exiting",
            session_id.0, client_id
        );
        return Ok(());
    }

    // ── Bootstrap ─────────────────────────────────────────────────────────
    // Request bootstrap packets from the headless loop.
    {
        let (tx, rx) = oneshot::channel();
        if entry
            .command_tx
            .send(ClientCommand::GetBootstrap { reply: tx })
            .await
            .is_err()
        {
            warn!(
                "[session {}] virtual client {}: headless session gone during bootstrap request",
                session_id.0, client_id
            );
            return Ok(());
        }
        let packets = match rx.await {
            Ok(pkts) => pkts,
            Err(_) => {
                warn!(
                    "[session {}] virtual client {}: headless dropped bootstrap reply",
                    session_id.0, client_id
                );
                return Ok(());
            }
        };
        for pkt in packets {
            if let Err(e) = uo_client.send(pkt).await {
                warn!(
                    "[session {}] virtual client {} bootstrap send failed: {}",
                    session_id.0, client_id, e
                );
                return Ok(());
            }
        }
    }

    // ── Main Loop ─────────────────────────────────────────────────────────

    // Register per-client sink for arbiter responses.
    let (sink_tx, mut sink_rx) = mpsc::channel::<RawPacket>(64);

    // Attach to HeadlessClient.
    if entry
        .command_tx
        .send(ClientCommand::AttachClient {
            client_id,
            sink: sink_tx,
        })
        .await
        .is_err()
    {
        warn!(
            "[session {}] virtual client {}: headless session gone during attach",
            session_id.0, client_id
        );
        return Ok(());
    }

    // Subscribe to the broadcast channel.
    let mut packet_rx = entry.packet_tx.subscribe();

    loop {
        tokio::select! {
            // ── C→S: packet from UO client ────────────────────────────────
            event = uo_client.recv() => {
                match event.event {
                    SessionEvent::Packet(raw) => {
                        if raw.direction == PacketDirection::ClientToServer {
                            let pkt = RawPacket::new(raw.data, raw.direction);
                            if entry
                                .command_tx
                                .send(ClientCommand::RawPacket {
                                    client_id,
                                    data: pkt,
                                })
                                .await
                                .is_err()
                            {
                                warn!(
                                    "[session {}] virtual client {}: headless session gone",
                                    session_id.0, client_id
                                );
                                break;
                            }
                        }
                    }
                    SessionEvent::Seed(_) => {}
                    SessionEvent::Disconnected | SessionEvent::Stopped => {
                        info!(
                            "[session {}] virtual client {} disconnected",
                            session_id.0, client_id
                        );
                        break;
                    }
                    SessionEvent::Error(e) => {
                        warn!(
                            "[session {}] virtual client {} error: {}",
                            session_id.0, client_id, e
                        );
                        break;
                    }
                }
            }

            // ── Per-client responses from HeadlessClient ──────────────────
            Some(pkt) = sink_rx.recv() => {
                if let Err(e) = uo_client.send(pkt).await {
                    warn!(
                        "[session {}] virtual client {} sink forward failed: {}",
                        session_id.0, client_id, e
                    );
                    break;
                }
            }

            // ── Broadcast: S→C frames from HeadlessClient ─────────────────
            Ok(frame) = packet_rx.recv() => {
                if frame.direction == PacketDirection::ServerToClient {
                    let pkt = RawPacket::new(frame.data, frame.direction);
                    if let Err(e) = uo_client.send(pkt).await {
                        warn!(
                            "[session {}] virtual client {} broadcast forward failed: {}",
                            session_id.0, client_id, e
                        );
                        break;
                    }
                }
            }
        }
    }

    // Cleanup — detach from HeadlessClient.
    let _ = entry
        .command_tx
        .send(ClientCommand::DetachClient { client_id })
        .await;

    info!(
        "[session {}] virtual client {} exited",
        session_id.0, client_id
    );
    Ok(())
}

// ── Login handshake: InitiateSession ──────────────────────────────────────

/// Relay the login handshake between the UO client and the headless session.
async fn initiate_session_handshake(
    uo_client: &mut Session,
    _entry: &SessionEntry,
    session_id: SessionId,
    client_id: ClientId,
    login_relay_tx: mpsc::Sender<LoginRelayC2S>,
    login_relay_rx: &mut mpsc::Receiver<LoginRelayS2C>,
) -> bool {
    info!(
        "[session {}] virtual client {}: InitiateSession handshake",
        session_id.0, client_id
    );

    loop {
        tokio::select! {
            // C→S from UO client — relay to headless.
            event = uo_client.recv() => {
                match event.event {
                    SessionEvent::Packet(raw) => {
                        if raw.direction == PacketDirection::ClientToServer {
                            let pkt = RawPacket::new(raw.data, raw.direction);
                            if login_relay_tx.send(LoginRelayC2S::Packet(pkt)).await.is_err() {
                                warn!(
                                    "[session {}] virtual client {}: login relay tx closed",
                                    session_id.0, client_id
                                );
                                return false;
                            }
                        }
                    }
                    SessionEvent::Seed(data) => {
                        if login_relay_tx
                            .send(LoginRelayC2S::Seed(data))
                            .await
                            .is_err()
                        {
                            warn!(
                                "[session {}] virtual client {}: login relay tx closed (seed)",
                                session_id.0, client_id
                            );
                            return false;
                        }
                    }
                    SessionEvent::Disconnected | SessionEvent::Stopped => {
                        info!(
                            "[session {}] virtual client {}: disconnected during InitiateSession handshake",
                            session_id.0, client_id
                        );
                        return false;
                    }
                    SessionEvent::Error(e) => {
                        warn!(
                            "[session {}] virtual client {}: error during InitiateSession handshake: {}",
                            session_id.0, client_id, e
                        );
                        return false;
                    }
                }
            }

            // S→C from headless — relay to UO client or handle WorldReady.
            Some(msg) = login_relay_rx.recv() => {
                match msg {
                    LoginRelayS2C::Packet(pkt) => {
                        if let Err(e) = uo_client.send(pkt).await {
                            warn!(
                                "[session {}] virtual client {}: login relay forward failed: {}",
                                session_id.0, client_id, e
                            );
                            return false;
                        }
                    }
                    LoginRelayS2C::WorldReady => {
                        info!(
                            "[session {}] virtual client {}: WorldReady received, proceeding to bootstrap",
                            session_id.0, client_id
                        );
                        return true;
                    }
                }
            }

            // Headless disconnected.
            else => {
                warn!(
                    "[session {}] virtual client {}: login relay rx closed",
                    session_id.0, client_id
                );
                return false;
            }
        }
    }
}

// ── Login handshake: JoinExisting ─────────────────────────────────────────

/// Perform a synthetic login handshake for a client joining an existing session.
async fn join_existing_handshake(
    uo_client: &mut Session,
    entry: &SessionEntry,
    session_id: SessionId,
    client_id: ClientId,
) -> bool {
    info!(
        "[session {}] virtual client {}: JoinExisting handshake",
        session_id.0, client_id
    );

    // Phase 1: Wait for 0x91 GameLogin.
    loop {
        match uo_client.recv().await.event {
            SessionEvent::Seed(_) => {}
            SessionEvent::Packet(raw) if raw.id() == 0x91 => {
                debug!(
                    "[session {}] virtual client {}: received 0x91 GameLogin",
                    session_id.0, client_id
                );
                break;
            }
            SessionEvent::Packet(raw) => {
                debug!(
                    "[session {}] virtual client {}: ignoring pre-login packet 0x{:02X}",
                    session_id.0, client_id, raw.id()
                );
            }
            SessionEvent::Disconnected | SessionEvent::Stopped => {
                info!(
                    "[session {}] virtual client {}: disconnected waiting for 0x91",
                    session_id.0, client_id
                );
                return false;
            }
            SessionEvent::Error(e) => {
                warn!(
                    "[session {}] virtual client {}: error waiting for 0x91: {}",
                    session_id.0, client_id, e
                );
                return false;
            }
        }
    }

    // Send 0xB9 EnableFeatures (cached from Source, if available).
    {
        let (tx, rx) = oneshot::channel();
        if entry
            .command_tx
            .send(ClientCommand::GetEnableFeatures { reply: tx })
            .await
            .is_err()
        {
            warn!(
                "[session {}] virtual client {}: headless session gone during enable-features request",
                session_id.0, client_id
            );
            return false;
        }
        if let Ok(Some(raw_b9)) = rx.await {
            if let Err(e) = uo_client.send(RawPacket::s2c(raw_b9)).await {
                warn!(
                    "[session {}] virtual client {}: 0xB9 send failed: {}",
                    session_id.0, client_id, e
                );
                return false;
            }
        }
    }

    // Send 0xA9 CharacterList with real character name.
    let char_name = entry
        .character_name
        .read()
        .await
        .clone()
        .unwrap_or_else(|| "Mirror".to_string());

    let char_list = CharacterList::new(
        vec![CharacterSlot::new(&char_name)],
        vec![StartingLocation {
            index: 0,
            city_name: packets::u_io::FixedString::new("Mirror"),
            area_name: packets::u_io::FixedString::new("Mirror"),
        }],
        0,
    );
    let char_list_pkt = RawPacket::s2c(char_list.to_bytes());
    if let Err(e) = uo_client.send(char_list_pkt).await {
        warn!(
            "[session {}] virtual client {}: 0xA9 send failed: {}",
            session_id.0, client_id, e
        );
        return false;
    }

    // Phase 2: Wait for 0x5D LoginCharacter.
    loop {
        match uo_client.recv().await.event {
            SessionEvent::Packet(raw) if raw.id() == 0x5D => {
                debug!(
                    "[session {}] virtual client {}: received 0x5D LoginCharacter",
                    session_id.0, client_id
                );
                return true;
            }
            SessionEvent::Packet(raw) => {
                debug!(
                    "[session {}] virtual client {}: ignoring packet 0x{:02X} (waiting for 0x5D)",
                    session_id.0, client_id, raw.id()
                );
            }
            SessionEvent::Seed(_) => {}
            SessionEvent::Disconnected | SessionEvent::Stopped => {
                info!(
                    "[session {}] virtual client {}: disconnected waiting for 0x5D",
                    session_id.0, client_id
                );
                return false;
            }
            SessionEvent::Error(e) => {
                warn!(
                    "[session {}] virtual client {}: error waiting for 0x5D: {}",
                    session_id.0, client_id, e
                );
                return false;
            }
        }
    }
}
