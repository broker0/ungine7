//! Headless session — owns the upstream server connection.
//!
//! [`run_headless_session`] runs as a background task and is the sole
//! consumer of S→C packets from the real game server.  It feeds the
//! diorama, dispatches packets through [`MoveArbiter`] and
//! [`TargetManager`], and broadcasts results to all connected
//! [`VirtualClient`](super::virtual_client)s.
//!
//! All client interaction flows through a single
//! `mpsc::Receiver<ClientCommand>` — there is no shared mutable state
//! between the headless loop and its consumers.
//!
//! # Lifecycle
//!
//! 1. **Login Relay** — the first client picks a character.  Login
//!    handshake packets (0xB9, 0xA9, 0x5D) are relayed between the
//!    server and the first client via dedicated channels.
//!
//! 2. **Main Loop** — S→C packets are fed to the diorama, dispatched
//!    through managers, and broadcast/delivered to clients.  C→S
//!    packets arrive via `ClientCommand::RawPacket` and are parsed,
//!    dispatched through managers, and forwarded to the server.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use log::{debug, info, warn};
use u_core::PacketDirection;
use u_core::position::{Facing, Heading};
use framework::rythmos::{ClientId, ClientResponse, MoveArbiter};
use framework::diorama::{ObserverPipeline, CompositeTileProvider};
use framework::ecumene::{BBoxSpatialIndex, EntityRegistry, MovementValidator};
use framework::diorama::WorldEntity;
use packets::character::DrawGamePlayer;
use packets::interaction::{DoubleClick, EquipItem};
use packets::layer::Layer;
use packets::login::{CharacterList, LoginCharacter};
use packets::movement::{MoveAck, MoveReject, MoveRequest};
use packets::registry::{DecodedOutcome, DecodedResult, OutputFormat, PacketRegistry};
use packets::traits::BasicPacket;
use packets::world::DrawMobile;
use tokio::sync::mpsc;

use network::session::{Session, SessionEvent};
use protocol::RawPacket;

use crate::logic::bootstrap;
use crate::managers::target::{TargetManager, TargetServerResult, TargetSubmitResult};
use crate::registry::SessionEntry;
use crate::rpc::protocol::{PacketEvent, ServerMessage, WorldItemData, WorldMobileData};
use crate::session::commands::ClientCommand;
use crate::session::dot_commands::{self, DotCommands, Handled};
use framework::diorama::ObserverEvent;
use crate::session::paced_sender::{PacedSender, WaitingStep};
use crate::types::{FullSessionState, PacketFrame, SessionId, WsClientId};

// ── Packet registry (shared, initialized once) ────────────────────────────

static PACKET_REGISTRY: OnceLock<PacketRegistry> = OnceLock::new();

#[inline]
fn packet_registry() -> &'static PacketRegistry {
    PACKET_REGISTRY.get_or_init(PacketRegistry::default)
}

// ── Login relay channel types ─────────────────────────────────────────────

/// C→S messages from the first client to HeadlessSession during login relay.
#[derive(Debug)]
pub enum LoginRelayC2S {
    /// Seed bytes from the client (must be forwarded before any packets).
    Seed(bytes::Bytes),
    /// Raw packet from the client (0x91 GameLogin, 0x5D LoginCharacter, etc.).
    Packet(RawPacket),
}

/// S→C messages from HeadlessSession to the first client during login relay.
#[derive(Debug)]
pub enum LoginRelayS2C {
    /// Raw packet to forward to the client (0xB9, 0xA9, etc.).
    Packet(RawPacket),
    /// The server has sent 0x55 LoginComplete — world is ready.
    WorldReady,
}

// ── Wire-encoding helpers (moved from arbiter/movement.rs) ────────────────

fn encode_move_ack(their_seq: u8, notoriety: packets::movement::Notoriety) -> RawPacket {
    let ack = MoveAck {
        id: MoveAck::ID,
        sequence: their_seq,
        notoriety,
    };
    RawPacket::s2c(ack.to_bytes())
}

fn encode_move_reject(their_seq: u8, draw: &DrawGamePlayer) -> Vec<RawPacket> {
    let reject = MoveReject {
        id: MoveReject::ID,
        sequence: their_seq,
        x: draw.x,
        y: draw.y,
        z: draw.z,
        direction: draw.direction,
    };
    vec![
        RawPacket::s2c(reject.to_bytes()),
        RawPacket::s2c(draw.to_bytes()),
    ]
}

fn encode_draw(draw: &DrawGamePlayer) -> RawPacket {
    RawPacket::s2c(draw.to_bytes())
}

/// Encode a [`ClientResponse`] into wire-ready [`RawPacket`]s.
fn encode_client_response(response: ClientResponse) -> Vec<RawPacket> {
    match response {
        ClientResponse::Ack {
            their_seq,
            notoriety,
        } => vec![encode_move_ack(their_seq, notoriety)],
        ClientResponse::Reject { their_seq, draw } => encode_move_reject(their_seq, &draw),
        ClientResponse::Draw { draw } => vec![encode_draw(&draw)],
    }
}

// ── HeadlessSession ───────────────────────────────────────────────────────

/// Runs the headless session that owns the upstream server connection.
///
/// This function takes ownership of the `server` session, the
/// `command_rx` channel, and all manager state.  It should be spawned
/// as a tokio task.
pub async fn run_headless_session(
    mut server: Session,
    mut command_rx: mpsc::Receiver<ClientCommand>,
    entry: Arc<SessionEntry>,
    session_id: SessionId,
    // Login relay channels — first client uses these to relay
    // the login handshake.
    mut login_relay_rx: mpsc::Receiver<LoginRelayC2S>,
    login_relay_tx: mpsc::Sender<LoginRelayS2C>,
) {
    info!(
        "[session {}] headless session started",
        session_id.0
    );

    // Observer is owned locally — no shared state.  Mirrors request bootstrap
    // and enable-features through the command channel.
    let mut observer = match (&entry.static_data, &entry.data_dir) {
        (Some(sd), Some(dir)) => ObserverPipeline::with_data_dir(sd.clone(), dir.clone()),
        _ => ObserverPipeline::new(entry.static_data.clone()),
    };

    // ── Phase 1: Login Relay ──────────────────────────────────────────────
    //
    // The first client drives the character selection.  We relay packets
    // between the server and the first client.

    #[allow(unused_assignments)]
    let mut character_names: Vec<String> = Vec::new();
    let mut character_selected = false;

    'login_relay: loop {
        tokio::select! {
            // S→C from server.
            event = server.recv() => {
                match event.event {
                    SessionEvent::Packet(raw) => {
                        let pkt_id = raw.data.first().copied().unwrap_or(0);

                        // Feed diorama with all S→C packets.
                        observer.ingest_s2c(&raw.data);

                        match pkt_id {
                            // 0xA9 CharacterList — extract character names.
                            0xA9 => {
                                if let Ok(char_list) = CharacterList::from_bytes(&raw.data) {
                                    character_names = char_list
                                        .characters
                                        .iter()
                                        .map(|slot| slot.name.to_string())
                                        .collect();
                                    debug!(
                                        "[session {}] headless: parsed 0xA9, {} character(s)",
                                        session_id.0,
                                        character_names.len()
                                    );
                                }
                                let pkt = RawPacket::new(raw.data, raw.direction);
                                if login_relay_tx
                                    .send(LoginRelayS2C::Packet(pkt))
                                    .await
                                    .is_err()
                                {
                                    warn!("[session {}] headless: login relay tx closed", session_id.0);
                                    return;
                                }
                            }

                            // 0x55 LoginComplete — world is ready.
                            0x55 => {
                                info!(
                                    "[session {}] headless: received 0x55 LoginComplete, transitioning to main loop",
                                    session_id.0
                                );
                                let _ = login_relay_tx.send(LoginRelayS2C::WorldReady).await;
                                break 'login_relay;
                            }

                            // All other S→C packets during login relay.
                            _ => {
                                if character_selected {
                                    debug!(
                                        "[session {}] headless: diorama-only 0x{:02X} (post character select)",
                                        session_id.0, pkt_id
                                    );
                                } else {
                                    let pkt = RawPacket::new(raw.data, raw.direction);
                                    if login_relay_tx
                                        .send(LoginRelayS2C::Packet(pkt))
                                        .await
                                        .is_err()
                                    {
                                        warn!(
                                            "[session {}] headless: login relay tx closed during 0x{:02X}",
                                            session_id.0, pkt_id
                                        );
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    SessionEvent::Seed(_) => {}
                    SessionEvent::Disconnected | SessionEvent::Stopped => {
                        warn!("[session {}] headless: server disconnected during login relay", session_id.0);
                        let _ = login_relay_tx.send(LoginRelayS2C::WorldReady).await;
                        return;
                    }
                    SessionEvent::Error(e) => {
                        warn!("[session {}] headless: server error during login relay: {}", session_id.0, e);
                        return;
                    }
                }
            }

            // C→S from first client — forward to server.
            Some(msg) = login_relay_rx.recv() => {
                match msg {
                    LoginRelayC2S::Seed(data) => {
                        if let Err(e) = server.send_seed(data).await {
                            warn!(
                                "[session {}] headless: seed forward failed: {}",
                                session_id.0, e
                            );
                            return;
                        }
                    }
                    LoginRelayC2S::Packet(pkt) => {
                        let pkt_id = pkt.data.first().copied().unwrap_or(0);

                        if pkt_id == 0x5D {
                            if let Ok(login_char) = LoginCharacter::from_bytes(&pkt.data) {
                                let name = login_char.name.to_string();
                                info!(
                                    "[session {}] headless: character selected: '{}'",
                                    session_id.0, name
                                );
                                *entry.character_name.write().await = Some(name);
                            }
                            character_selected = true;
                        }

                        if let Err(e) = server.send(pkt).await {
                            warn!(
                                "[session {}] headless: forward C→S 0x{:02X} failed: {}",
                                session_id.0, pkt_id, e
                            );
                            return;
                        }
                    }
                }
            }

            // First client disconnected during login relay.
            else => {
                warn!("[session {}] headless: login relay rx closed", session_id.0);
                return;
            }
        }
    }

    // ── Phase 2: Main Loop ────────────────────────────────────────────────
    //
    // Managers are owned by this loop — no shared mutable state.

    info!("[session {}] headless: entering main loop", session_id.0);

    let mut movement = MoveArbiter::new(4);
    let mut target = TargetManager::new();
    let mut paced = PacedSender::new(16);
    let mut dot_commands: HashMap<ClientId, DotCommands> = HashMap::new();
    let mut client_sinks: HashMap<ClientId, mpsc::Sender<RawPacket>> = HashMap::new();
    let mut ws_sinks: HashMap<WsClientId, mpsc::Sender<ServerMessage>> = HashMap::new();
    // Per-WS packet filter: None = all packets, Some([]) = none, Some(ids) = only those ids.
    let mut ws_filters: HashMap<WsClientId, Option<Vec<u8>>> = HashMap::new();

    // Typed event broadcast — consumed by Lua scripts and (in the future)
    // WebSocket observers that want high-level events instead of raw packets.
    let event_tx = entry.event_tx.clone();

    // ── Mirror streaming (optional) ──────────────────────────────────
    // If --mirror-url is configured, spawn a background task that
    // streams all S2C packets to the remote mirror endpoint.
    let _mirror_handle = entry.mirror_url.as_ref().map(|url| {
        crate::rpc::ws_mirror::spawn_mirror_task(
            entry.clone(),
            session_id,
            url.clone(),
        )
    });

    // Resync state.
    let mut resync_pending = false;
    let mut last_move_sent: Option<Instant> = None;
    let mut last_move_recv: Option<Instant> = None;
    let mut last_resync_time: Option<Instant> = None;

    loop {
        // Compute the sleep future for the paced sender.  If the outbound
        // queue is empty this resolves to a very long sleep (effectively
        // disabled).
        let paced_deadline = paced
            .next_send_instant()
            .map(tokio::time::Instant::from_std)
            .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(86400));

        // Compute timeout for stalled movement (resync trigger).
        let stall_deadline = if !resync_pending
            && movement.mover().pending_count() > 0
            && last_resync_time
                .map_or(true, |t| t.elapsed() > std::time::Duration::from_millis(1500))
        {
            // If we have pending steps and the last ack/reject was > 30s ago
            // (or we never received one), fire.
            match last_move_recv {
                Some(t) => {
                    let deadline = t + std::time::Duration::from_secs(30);
                    tokio::time::Instant::from_std(deadline)
                }
                None => {
                    // Never received anything — use last_move_sent + 30s.
                    match last_move_sent {
                        Some(t) => tokio::time::Instant::from_std(
                            t + std::time::Duration::from_secs(30),
                        ),
                        None => tokio::time::Instant::now()
                            + std::time::Duration::from_secs(86400),
                    }
                }
            }
        } else {
            tokio::time::Instant::now() + std::time::Duration::from_secs(86400)
        };

        tokio::select! {
            // S→C from upstream server.
            event = server.recv() => {
                match event.event {
                    SessionEvent::Packet(raw) => {
                        if raw.direction == PacketDirection::ServerToClient {
                            // Broadcast to WS observers unconditionally — before
                            // any arbitration logic that might early-return.
                            broadcast_packet_to_ws(
                                raw.data.first().copied().unwrap_or(0),
                                &raw.data,
                                &ws_sinks,
                                &ws_filters,
                            );
                            handle_s2c(
                                raw,
                                &entry,
                                session_id,
                                &mut observer,
                                &mut movement,
                                &mut target,
                                &mut paced,
                                &mut resync_pending,
                                &mut last_move_recv,
                                &client_sinks,
                                &mut server,
                                &event_tx,
                            )
                            .await;
                        }
                    }
                    SessionEvent::Seed(_) => {}
                    SessionEvent::Disconnected | SessionEvent::Stopped => {
                        info!("[session {}] headless: server disconnected", session_id.0);
                        break;
                    }
                    SessionEvent::Error(e) => {
                        warn!("[session {}] headless: server error: {}", session_id.0, e);
                        break;
                    }
                }
            }

            // Commands from clients / WS / bot.
            Some(cmd) = command_rx.recv() => {
                match cmd {
                    ClientCommand::RawPacket { client_id, data } => {
                        dispatch_c2s(
                            client_id,
                            &data,
                            &mut server,
                            &entry,
                            session_id,
                            &mut observer,
                            &mut movement,
                            &mut target,
                            &mut paced,
                            &mut resync_pending,
                            &mut last_resync_time,
                            &client_sinks,
                            &mut dot_commands,
                        )
                        .await;
                    }

                    ClientCommand::AttachClient { client_id, sink } => {
                        debug!(
                            "[session {}] attach client_id={}",
                            session_id.0, client_id
                        );
                        client_sinks.insert(client_id, sink);
                        let mut cmds = DotCommands::new();
                        #[cfg(feature = "lua")]
                        cmds.set_lua_cmd_tx(entry.lua_cmd_tx.clone());
                        // Show the action menu immediately on connect.
                        if let Some(sink) = client_sinks.get(&client_id) {
                            cmds.send_action_menu_gump(observer.pos.serial, sink).await;
                        }
                        dot_commands.insert(client_id, cmds);
                        movement.attach_client(client_id);
                        target.attach_client(client_id);
                    }

                    ClientCommand::DetachClient { client_id } => {
                        debug!(
                            "[session {}] detach client_id={}",
                            session_id.0, client_id
                        );
                        client_sinks.remove(&client_id);
                        dot_commands.remove(&client_id);
                        movement.detach_client(client_id);
                        target.detach_client(client_id);
                    }

                    ClientCommand::GetState { reply } => {
                        let state = FullSessionState {
                            character: entry.character_name.read().await.clone(),
                            position: (observer.pos.x, observer.pos.y, observer.pos.z),
                            world: observer.session.current_world,
                        };
                        let _ = reply.send(state);
                    }

                    ClientCommand::GetBootstrap { reply } => {
                        let packets = bootstrap::generate_bootstrap(
                            &observer,
                            entry.static_data.as_deref(),
                            entry.client_version,
                        );
                        let _ = reply.send(packets);
                    }

                    ClientCommand::GetEnableFeatures { reply } => {
                        let _ = reply.send(observer.session.last_enable_features.clone());
                    }

                    ClientCommand::AttachWs { ws_id, sink, filter } => {
                        ws_sinks.insert(ws_id, sink);
                        ws_filters.insert(ws_id, filter);
                    }

                    ClientCommand::DetachWs { ws_id } => {
                        ws_sinks.remove(&ws_id);
                        ws_filters.remove(&ws_id);
                    }

                    ClientCommand::GetItems { reply } => {
                        let world = observer.session.current_world;
                        let items: Vec<WorldItemData> = observer
                            .session
                            .visible
                            .iter()
                            .filter(|e| !e.is_mobile())
                            .filter_map(|e| WorldItemData::from_entity(e, world))
                            .collect();
                        let _ = reply.send(items);
                    }

                    ClientCommand::GetMobiles { reply } => {
                        let world = observer.session.current_world;
                        let mobiles: Vec<WorldMobileData> = observer
                            .session
                            .visible
                            .iter()
                            .filter(|e| e.is_mobile())
                            .filter_map(|e| WorldMobileData::from_entity(e, world))
                            .collect();
                        let _ = reply.send(mobiles);
                    }

                    ClientCommand::GetMobile { serial, reply } => {
                        let world = observer.session.current_world;
                        let result = observer
                            .session
                            .visible
                            .get(serial)
                            .filter(|e| e.is_mobile())
                            .and_then(|e| WorldMobileData::from_entity(e, world));
                        let _ = reply.send(result);
                    }

                    ClientCommand::GetEquipment { serial, reply } => {
                        let world = observer.session.current_world;
                        let equipment = observer
                            .session
                            .visible
                            .get(serial)
                            .filter(|e| e.is_mobile())
                            .and_then(|e| WorldMobileData::from_entity(e, world))
                            .map(|m| m.equipment)
                            .unwrap_or_default();
                        let _ = reply.send(equipment);
                    }

                    ClientCommand::UseObject { serial, reply } => {
                        let pkt = DoubleClick { id: DoubleClick::ID, serial };
                        let raw = RawPacket::c2s(pkt.to_bytes());
                        if let Err(e) = server.send(raw).await {
                            warn!(
                                "[session {}] headless: UseObject({:#010X}) send failed: {}",
                                session_id.0, serial, e
                            );
                        }
                        let _ = reply.send(());
                    }

                    ClientCommand::Step { heading: heading_raw, raw: is_raw, reply } => {
                        let Some(heading) = Heading::from_raw(heading_raw) else {
                            warn!(
                                "[session {}] headless: Step with invalid heading={}",
                                session_id.0, heading_raw
                            );
                            let _ = reply.send(false);
                            return;
                        };

                        let pred    = paced.predicted_pos();
                        let is_turn = heading != pred.facing.heading();

                        if is_raw {
                            // Raw step — no passability check.
                            let facing = Facing::from_heading(heading);
                            let result = movement.bot_step(facing);
                            if let Some(server_req) = result.to_server {
                                let raw_pkt = RawPacket::c2s(server_req.to_bytes());
                                paced.enqueue_outbound(raw_pkt, facing, is_turn);
                            }
                            let _ = reply.send(true);
                        } else if is_turn {
                            // Turn only — no tile crossing, no passability check needed.
                            let facing = Facing::from_heading(heading);
                            let result = movement.bot_step(facing);
                            if let Some(server_req) = result.to_server {
                                let raw_pkt = RawPacket::c2s(server_req.to_bytes());
                                paced.enqueue_outbound(raw_pkt, facing, true);
                            }
                            let _ = reply.send(true);
                        } else {
                            // Validated step — check passability.
                            let blocked = if let Some(sd) = entry.static_data.as_deref() {
                                let provider = CompositeTileProvider::new(
                                    sd,
                                    observer.session.current_world,
                                    &observer.session.visible,
                                    &observer.session.registry,
                                );
                                MovementValidator::new(&provider)
                                    .test_step(pred.x, pred.y, pred.z, heading)
                                    .is_none()
                            } else {
                                false
                            };

                            if blocked {
                                warn!(
                                    "[session {}] headless: Step blocked: heading={} at ({},{},{}) (predicted)",
                                    session_id.0, heading, pred.x, pred.y, pred.z,
                                );
                                let _ = reply.send(false);
                            } else {
                                let facing = Facing::from_heading(heading);
                                let result = movement.bot_step(facing);
                                if let Some(server_req) = result.to_server {
                                    let raw_pkt = RawPacket::c2s(server_req.to_bytes());
                                    paced.enqueue_outbound(raw_pkt, facing, false);
                                }
                                let _ = reply.send(true);
                            }
                        }
                    }
                }
            }

            // Paced sender: send the next buffered step to the server.
            _ = tokio::time::sleep_until(paced_deadline) => {
                if let Some(raw) = paced.try_flush() {
                    observer.ingest_c2s(&raw.data);
                    last_move_sent = Some(Instant::now());
                    if let Err(e) = server.send(raw).await {
                        warn!(
                            "[session {}] headless: paced send failed: {}",
                            session_id.0, e
                        );
                    }
                }
            }

            // Stall timeout: initiate resync if movement is stuck.
            _ = tokio::time::sleep_until(stall_deadline) => {
                if !resync_pending && movement.mover().pending_count() > 0 {
                    warn!(
                        "[session {}] headless: movement stall detected ({} pending, no ack for 30s), initiating resync",
                        session_id.0,
                        movement.mover().pending_count(),
                    );
                    initiate_resync(
                        session_id,
                        &mut server,
                        &mut movement,
                        &mut paced,
                        &mut resync_pending,
                        &mut last_resync_time,
                        &client_sinks,
                    )
                    .await;
                }
            }
        }
    }

    info!("[session {}] headless: session ended", session_id.0);
}

// ── S→C packet handling ───────────────────────────────────────────────────

async fn handle_s2c(
    raw: RawPacket,
    entry: &Arc<SessionEntry>,
    session_id: SessionId,
    observer: &mut ObserverPipeline,
    movement: &mut MoveArbiter,
    target: &mut TargetManager,
    paced: &mut PacedSender,
    resync_pending: &mut bool,
    last_move_recv: &mut Option<Instant>,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
    server: &mut Session,
    event_tx: &tokio::sync::broadcast::Sender<ObserverEvent>,
) {
    let pkt_id = raw.data.first().copied().unwrap_or(0);

    // Always feed diorama (also emits ObserverEvents internally).
    observer.ingest_s2c(&raw.data);

    // Drain typed events and broadcast them.
    for event in observer.drain_events() {
        let _ = event_tx.send(event);
    }

    match pkt_id {
        // ── Movement S→C ──────────────────────────────────────────────

        // 0x22 MoveAck — server confirmed a step.
        0x22 => {
            // If resync is pending, ignore stale acks until we get 0x20.
            if *resync_pending {
                debug!(
                    "[session {}] S→C MoveAck ignored (resync pending)",
                    session_id.0
                );
                return;
            }

            let Ok(ack) = MoveAck::from_bytes(&raw.data) else {
                warn!("[session {}] failed to parse MoveAck", session_id.0);
                return;
            };
            debug!(
                "[session {}] S→C MoveAck: seq={} notoriety={:?}",
                session_id.0, ack.sequence, ack.notoriety
            );

            *last_move_recv = Some(Instant::now());

            // Build Z resolver from diorama state.
            let z_resolver: Option<CompositeTileProvider<'_, EntityRegistry<WorldEntity, BBoxSpatialIndex>>> =
                entry.static_data.as_deref().map(|sd| {
                    CompositeTileProvider::new(
                        sd,
                        observer.session.current_world,
                        &observer.session.visible,
                        &observer.session.registry,
                    )
                });

            let responses = movement.on_server_ack(
                &ack,
                z_resolver
                    .as_ref()
                    .map(|r| r as &dyn framework::rythmos::ZResolver),
            );

            // Check if the ack triggered a desync (responses contain rejects).
            let had_desync = responses.iter().any(|(_, r)| matches!(r, ClientResponse::Reject { .. }))
                && movement.mover().pending_count() == 0;

            deliver_movement_responses(&responses, session_id, client_sinks).await;

            // If desync was detected, initiate a proxy resync.
            if had_desync {
                warn!(
                    "[session {}] desync detected on MoveAck seq={}, initiating resync",
                    session_id.0, ack.sequence,
                );
                let mut last_resync_time = None;
                initiate_resync(
                    session_id,
                    server,
                    movement,
                    paced,
                    resync_pending,
                    &mut last_resync_time,
                    client_sinks,
                )
                .await;
                return;
            }

            // Replay waiting steps now that an in-flight slot has freed up.
            replay_waiting_steps(
                session_id,
                movement,
                paced,
                observer,
                client_sinks,
            )
            .await;
        }

        // 0x21 MoveReject — server rejected a step.
        0x21 => {
            let Ok(reject) = MoveReject::from_bytes(&raw.data) else {
                warn!("[session {}] failed to parse MoveReject", session_id.0);
                return;
            };
            debug!(
                "[session {}] S→C MoveReject: seq={}",
                session_id.0, reject.sequence
            );

            *last_move_recv = Some(Instant::now());

            let responses = movement.on_server_reject(&reject);
            deliver_movement_responses(&responses, session_id, client_sinks).await;

            // Drain paced sender — all buffered steps are invalid after reject.
            let drained_waiting = paced.reset();
            paced.sync_predicted(movement.pos());
            reject_waiting_steps(&drained_waiting, movement, session_id, client_sinks).await;
        }

        // 0x20 DrawGamePlayer — authoritative position snap.
        0x20 => {
            let Ok(dgp) = DrawGamePlayer::from_bytes(&raw.data) else {
                warn!("[session {}] failed to parse DrawGamePlayer", session_id.0);
                return;
            };
            debug!(
                "[session {}] S→C DrawGamePlayer: x={} y={} z={}",
                session_id.0, dgp.x, dgp.y, dgp.z
            );

            *last_move_recv = Some(Instant::now());

            // Clear resync pending — 0x20 is the authoritative answer.
            if *resync_pending {
                debug!(
                    "[session {}] resync completed (received DrawGamePlayer)",
                    session_id.0
                );
                *resync_pending = false;
            }

            let responses = movement.on_position_snap(&dgp);
            deliver_movement_responses(&responses, session_id, client_sinks).await;

            // Drain paced sender — all buffered steps are invalid after snap.
            let drained_waiting = paced.reset();
            paced.sync_predicted(movement.pos());
            reject_waiting_steps(&drained_waiting, movement, session_id, client_sinks).await;
        }

        // ── Mount tracking ────────────────────────────────────────────

        // 0x78 DrawMobile — check mount status for player serial.
        0x78 => {
            if let Ok(mob) = DrawMobile::parse(&raw.data, false) {
                if mob.serial == observer.pos.serial {
                    let mounted = mob.items.iter().any(|eq| eq.layer == Layer::Mount);
                    paced.set_mounted(mounted);
                }
            }
            // Broadcast to UO clients.
            let frame = PacketFrame { data: raw.data.clone(), direction: raw.direction };
            let _ = entry.packet_tx.send(frame);
        }

        // 0x2E EquipItem — mount/unmount tracking for player serial.
        0x2E => {
            if let Ok(equip) = EquipItem::from_bytes(&raw.data) {
                if equip.player_serial == observer.pos.serial && equip.layer == Layer::Mount {
                    paced.set_mounted(true);
                }
            }
            // Broadcast to UO clients.
            let frame = PacketFrame { data: raw.data.clone(), direction: raw.direction };
            let _ = entry.packet_tx.send(frame);
        }

        // 0x1D DeleteObject — might remove a mount item.
        0x1D => {
            // Check if the deleted item was the player's mount.
            if raw.data.len() >= 5 {
                let serial = u32::from_be_bytes([raw.data[1], raw.data[2], raw.data[3], raw.data[4]]);
                if paced.is_mounted() {
                    let still_mounted = observer.session.visible.is_mounted(observer.pos.serial);
                    if !still_mounted {
                        paced.set_mounted(false);
                    }
                    let _ = serial;
                }
            }
            // Broadcast to UO clients.
            let frame = PacketFrame { data: raw.data.clone(), direction: raw.direction };
            let _ = entry.packet_tx.send(frame);
        }

        // ── Target S→C ────────────────────────────────────────────────

        // 0x6C TargetCursor from server.
        0x6C => {
            match target.on_server_packet(&raw.data) {
                Some(TargetServerResult::Request | TargetServerResult::Cancel) => {
                    // Broadcast to UO clients.
                    let frame = PacketFrame { data: raw.data.clone(), direction: raw.direction };
                    let _ = entry.packet_tx.send(frame);
                }
                None => {
                    // Parse error — already logged by TargetManager.
                }
            }
        }

        // ── All other S→C packets ─────────────────────────────────────

        _ => {
            let frame = PacketFrame { data: raw.data, direction: raw.direction };
            let _ = entry.packet_tx.send(frame);
        }
    }
}

// ── WS broadcast helper ───────────────────────────────────────────────────

/// Encode bytes as a lowercase hex string.
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Broadcast a packet event to all subscribed WS observers that pass the filter.
///
/// Decodes the packet to JSON once using [`PacketRegistry`] and shares the
/// result across all matching observers.  Non-blocking (`try_send`) — a slow
/// WS client is silently skipped rather than stalling the headless loop.
fn broadcast_packet_to_ws(
    pkt_id: u8,
    data: &bytes::Bytes,
    ws_sinks: &HashMap<WsClientId, mpsc::Sender<ServerMessage>>,
    ws_filters: &HashMap<WsClientId, Option<Vec<u8>>>,
) {
    if ws_sinks.is_empty() {
        return;
    }

    // Check if any observer actually wants this packet before doing work.
    let any_pass = ws_sinks.keys().any(|ws_id| {
        let filter = ws_filters.get(ws_id).and_then(|f| f.as_deref());
        match filter {
            None => true,
            Some(f) if f.is_empty() => false,
            Some(f) => f.contains(&pkt_id),
        }
    });
    if !any_pass {
        return;
    }

    // Build shared representations once.
    let hex    = hex_encode(data);
    let id_str = format!("0x{:02X}", pkt_id);

    // Decode to JSON once — shared across all observers.
    let parsed: Option<serde_json::Value> = match packet_registry().decode(
        pkt_id,
        data,
        PacketDirection::ServerToClient,
        OutputFormat::Json,
    ) {
        DecodedResult::Ok(DecodedOutcome::Json(v)) => Some(v),
        _ => None,
    };

    for (ws_id, sink) in ws_sinks {
        let filter = ws_filters.get(ws_id).and_then(|f| f.as_deref());
        let pass = match filter {
            None => true,
            Some(f) if f.is_empty() => false,
            Some(f) => f.contains(&pkt_id),
        };
        if pass {
            let event = ServerMessage::Packet {
                event: PacketEvent {
                    direction: "s2c".into(),
                    id: id_str.clone(),
                    hex: hex.clone(),
                    parsed: parsed.clone(),
                },
            };
            let _ = sink.try_send(event);
        }
    }
}

// ── C→S packet dispatch ───────────────────────────────────────────────────

async fn dispatch_c2s(
    client_id: ClientId,
    pkt: &RawPacket,
    server: &mut Session,
    entry: &Arc<SessionEntry>,
    session_id: SessionId,
    observer: &mut ObserverPipeline,
    movement: &mut MoveArbiter,
    target: &mut TargetManager,
    paced: &mut PacedSender,
    resync_pending: &mut bool,
    last_resync_time: &mut Option<Instant>,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
    dot_commands: &mut HashMap<ClientId, DotCommands>,
) {
    let pkt_id = pkt.data.first().copied().unwrap_or(0);

    debug!(
        "[session {}] client {} C→S: pkt_id=0x{:02X} len={}",
        session_id.0, client_id, pkt_id, pkt.data.len()
    );

    // ── Dot-command interception ──────────────────────────────────────
    //
    // Must run BEFORE the main pkt_id match so that:
    // - Speech starting with `.` is consumed and never reaches the server.
    // - Target cursor responses with our reserved cursor_id are consumed
    //   and never reach TargetManager.
    if let Some(cmds) = dot_commands.get_mut(&client_id) {
        if let Some(sink) = client_sinks.get(&client_id) {
            // handle_packet borrows diorama immutably; the returned
            // Handled value is fully owned, so the borrow ends here.
            let handled = cmds.handle_packet(pkt, sink, &*observer).await;
            match handled {
                Handled::Yes => return,
                Handled::Step { heading } => {
                    // Only validate passability when already facing the
                    // target direction.  If not facing it, the first
                    // bot_step is just a turn (no tile crossed).
                    //
                    // Use predicted position (confirmed + sent-to-server
                    // steps) for accurate passability at the character's
                    // real expected location.
                    let pred = paced.predicted_pos();
                    let is_turn = heading != pred.facing.heading();
                    if !is_turn {
                        if let Some(sd) = entry.static_data.as_deref() {
                            let provider = CompositeTileProvider::new(
                                sd,
                                observer.session.current_world,
                                &observer.session.visible,
                                &observer.session.registry,
                            );
                            if MovementValidator::new(&provider)
                                .test_step(pred.x, pred.y, pred.z, heading)
                                .is_none()
                            {
                                warn!(
                                    "[session {}] .step blocked: heading={} at ({},{},{}) (predicted)",
                                    session_id.0, heading,
                                    pred.x, pred.y, pred.z,
                                );
                                if let Some(sink) = client_sinks.get(&client_id) {
                                    let msg = format!("Step blocked: impassable ({heading})");
                                    let _ = sink
                                        .send(dot_commands::system_message_packet(&msg))
                                        .await;
                                }
                                return;
                            }
                        }
                    }

                    let facing = Facing::from_heading(heading);
                    let result = movement.bot_step(facing);
                    if let Some(server_req) = result.to_server {
                        let raw = RawPacket::c2s(server_req.to_bytes());
                        paced.enqueue_outbound(raw, facing, is_turn);
                    }
                    return;
                }
                Handled::RawStep { heading } => {
                    // No passability validation — send directly to server.
                    let is_turn = heading != paced.predicted_pos().facing.heading();
                    let facing = Facing::from_heading(heading);
                    let result = movement.bot_step(facing);
                    if let Some(server_req) = result.to_server {
                        let raw = RawPacket::c2s(server_req.to_bytes());
                        paced.enqueue_outbound(raw, facing, is_turn);
                    }
                    return;
                }
                Handled::No => {} // fall through to existing logic
            }
        }
    }

    match pkt_id {
        // ── Movement C→S ──────────────────────────────────────────────

        // 0x02 MoveRequest
        0x02 => {
            let Ok(req) = MoveRequest::from_bytes(&pkt.data) else {
                warn!("[session {}] failed to parse MoveRequest from client {}", session_id.0, client_id);
                return;
            };

            debug!(
                "[session {}] C→S MoveRequest: client_id={} seq={} dir={:?}",
                session_id.0, client_id, req.sequence, req.direction
            );

            // Client-side passability check (diagnostic only — always forward).
            // Skip check when the step is just a turn (heading != facing).
            if let Some(sd) = entry.static_data.as_deref() {
                let heading = Facing::new(req.direction).heading();
                if heading == observer.pos.facing.heading() {
                    let provider = CompositeTileProvider::new(
                        sd,
                        observer.session.current_world,
                        &observer.session.visible,
                        &observer.session.registry,
                    );
                    if MovementValidator::new(&provider)
                        .test_step(observer.pos.x, observer.pos.y, observer.pos.z, heading)
                        .is_none()
                    {
                        warn!(
                            "[session {}] client {} MoveRequest would be blocked: heading={} at ({},{},{})",
                            session_id.0, client_id, heading,
                            observer.pos.x, observer.pos.y, observer.pos.z,
                        );
                    }
                }
            }

            let facing = Facing::new(req.direction);
            let is_turn = facing.heading() != paced.predicted_pos().facing.heading();
            let result = movement.client_step(client_id, &req);

            // Forward to server via paced sender if accepted.
            if let Some(server_req) = result.to_server {
                let server_pkt = RawPacket::c2s(server_req.to_bytes());
                paced.enqueue_outbound(server_pkt, facing, is_turn);
            }

            // Queue-full — try to buffer in paced sender's waiting queue.
            if let Some((cid, response)) = result.immediate {
                // Instead of immediately rejecting, buffer the step.
                let step = WaitingStep {
                    client_id: cid,
                    facing,
                    their_seq: req.sequence,
                };
                if !paced.enqueue_waiting(step) {
                    // Waiting queue also full — reject to client.
                    let packets = encode_client_response(response);
                    deliver_to_client(cid, &packets, session_id, client_sinks).await;
                }
            }
        }

        // ── Resync interception ───────────────────────────────────────

        // 0x22 from client — resync request (seq=0, dir=0).
        0x22 => {
            debug!(
                "[session {}] C→S 0x22 resync request from client {}",
                session_id.0, client_id
            );

            // Clear paced sender and arbiter state before forwarding.
            let drained_waiting = paced.reset();
            reject_waiting_steps(&drained_waiting, movement, session_id, client_sinks).await;

            // Clear arbiter pending queue via position snap with current pos.
            let current_dgp = movement.pos().to_draw_game_player();
            let responses = movement.on_position_snap(&current_dgp);
            deliver_movement_responses(&responses, session_id, client_sinks).await;

            paced.sync_predicted(movement.pos());

            *resync_pending = true;
            *last_resync_time = Some(Instant::now());

            // Forward to server.
            let fwd = RawPacket::new(pkt.data.clone(), pkt.direction);
            if let Err(e) = server.send(fwd).await {
                warn!(
                    "[session {}] headless: resync forward failed: {}",
                    session_id.0, e
                );
            }
        }

        // ── Target C→S ────────────────────────────────────────────────

        // 0x6C TargetCursor response
        0x6C => {
            match target.on_client_response(client_id, &pkt.data) {
                TargetSubmitResult::Accepted { to_server, cancel_others } => {
                    // Feed diorama.
                    observer.ingest_c2s(&to_server.data);
                    // Forward to server.
                    if let Err(e) = server.send(to_server).await {
                        warn!(
                            "[session {}] headless: target forward failed: {}",
                            session_id.0, e
                        );
                    }
                    // Send cancel to other clients.
                    for (cid, cancel_pkt) in &cancel_others {
                        deliver_to_client(*cid, &[cancel_pkt.clone()], session_id, client_sinks).await;
                    }
                }
                TargetSubmitResult::Stale => {
                    // Already logged by TargetManager — drop silently.
                }
            }
        }

        // ── All other C→S packets ─────────────────────────────────────

        _ => {
            // Feed diorama with C→S.
            observer.ingest_c2s(&pkt.data);
            // Passthrough — forward to server.
            let fwd = RawPacket::new(pkt.data.clone(), pkt.direction);
            if let Err(e) = server.send(fwd).await {
                warn!(
                    "[session {}] headless: c2s passthrough 0x{:02X} failed: {}",
                    session_id.0, pkt_id, e
                );
            }
        }
    }
}

// ── Delivery helpers ──────────────────────────────────────────────────────

/// Deliver movement responses to individual clients via their sinks.
async fn deliver_movement_responses(
    responses: &[(ClientId, ClientResponse)],
    session_id: SessionId,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
) {
    for (cid, response) in responses {
        let packets = encode_client_response(response.clone());
        deliver_to_client(*cid, &packets, session_id, client_sinks).await;
    }
}

/// Deliver a set of packets to a specific client via its sink.
async fn deliver_to_client(
    client_id: ClientId,
    packets: &[RawPacket],
    session_id: SessionId,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
) {
    if let Some(tx) = client_sinks.get(&client_id) {
        for pkt in packets {
            if let Err(e) = tx.send(pkt.clone()).await {
                warn!(
                    "[session {}] deliver to client_id={} failed: {}",
                    session_id.0, client_id, e
                );
                break;
            }
        }
    } else {
        debug!(
            "[session {}] no sink for client_id={} — skipped",
            session_id.0, client_id
        );
    }
}

// ── Paced sender helpers ──────────────────────────────────────────────────

/// Replay buffered waiting steps through the arbiter now that in-flight
/// slots have freed up.
///
/// Each successfully replayed step is enqueued into the paced outbound queue.
/// Steps that still can't fit (arbiter queue full again) stay in the waiting
/// queue for the next ack.
async fn replay_waiting_steps(
    session_id: SessionId,
    movement: &mut MoveArbiter,
    paced: &mut PacedSender,
    observer: &ObserverPipeline,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
) {
    while paced.has_waiting() && movement.mover().can_enqueue() {
        let Some(step) = paced.dequeue_waiting() else {
            break;
        };

        let req = MoveRequest {
            id: MoveRequest::ID,
            direction: step.facing.raw(),
            sequence: step.their_seq,
            fastwalk_key: 0,
        };

        let result = movement.client_step(step.client_id, &req);

        if let Some(server_req) = result.to_server {
            let is_turn = step.facing.heading() != observer.pos.facing.heading();
            let server_pkt = RawPacket::c2s(server_req.to_bytes());
            paced.enqueue_outbound(server_pkt, step.facing, is_turn);
            debug!(
                "[paced] replayed waiting step: client_id={} seq={}",
                step.client_id, step.their_seq,
            );
        }

        // If the arbiter still rejected (shouldn't happen since we checked
        // can_enqueue, but be safe), put the step back and stop.
        if let Some((cid, response)) = result.immediate {
            // Re-buffer.
            let re_step = WaitingStep {
                client_id: cid,
                facing: step.facing,
                their_seq: step.their_seq,
            };
            if !paced.enqueue_waiting(re_step) {
                // Queue is full — reject to client.
                let packets = encode_client_response(response);
                deliver_to_client(cid, &packets, session_id, client_sinks).await;
            }
            break;
        }
    }
}

/// Reject all drained waiting steps back to their originating clients.
async fn reject_waiting_steps(
    drained: &[WaitingStep],
    movement: &MoveArbiter,
    session_id: SessionId,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
) {
    if drained.is_empty() {
        return;
    }

    let draw = movement.pos().to_draw_game_player();
    for step in drained {
        let response = ClientResponse::Reject {
            their_seq: step.their_seq,
            draw: draw.clone(),
        };
        let packets = encode_client_response(response);
        deliver_to_client(step.client_id, &packets, session_id, client_sinks).await;
    }

    debug!(
        "[session {}] rejected {} buffered waiting steps",
        session_id.0,
        drained.len(),
    );
}

/// Initiate a proxy-driven resync: send `0x22 0x00 0x00` to the server,
/// clear all movement state, and set the resync-pending flag.
async fn initiate_resync(
    session_id: SessionId,
    server: &mut Session,
    movement: &mut MoveArbiter,
    paced: &mut PacedSender,
    resync_pending: &mut bool,
    last_resync_time: &mut Option<Instant>,
    client_sinks: &HashMap<ClientId, mpsc::Sender<RawPacket>>,
) {
    // Clear paced sender.
    let drained_waiting = paced.reset();
    reject_waiting_steps(&drained_waiting, movement, session_id, client_sinks).await;

    // Clear arbiter pending queue.
    let current_dgp = movement.pos().to_draw_game_player();
    let responses = movement.on_position_snap(&current_dgp);
    deliver_movement_responses(&responses, session_id, client_sinks).await;

    paced.sync_predicted(movement.pos());

    // Send resync request to server: [0x22] [0x00] [0x00].
    let resync_pkt = RawPacket::c2s_raw(&[0x22, 0x00, 0x00]);
    if let Err(e) = server.send(resync_pkt).await {
        warn!(
            "[session {}] headless: resync send failed: {}",
            session_id.0, e
        );
    }

    *resync_pending = true;
    *last_resync_time = Some(Instant::now());

    info!(
        "[session {}] headless: resync initiated",
        session_id.0,
    );
}
