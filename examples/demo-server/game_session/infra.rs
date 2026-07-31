//! Shared infrastructure handlers used by both `rust-session` and `lua-session`.
//!
//! These functions extract the duplicated infrastructure code that was
//! previously copy-pasted between `rust_session.rs` and `lua_session.rs`:
//!
//! - **Packet handling**: ping, movement, interactions, view range, resync
//! - **Login flow**: GameLogin (0x91), LoginCharacter (0x5D), backpack registration
//! - **World events**: batch drain + packet collection
//! - **Cleanup**: held-item drop, observer unregister, test entity removal

use std::sync::Arc;

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use framework::continuum::{WorkerCommand, WorldEvent};
use framework::diorama::ObserverPipeline;
use framework::ecumene::TileRect;

use common::uo_engine::auth::{AccessLevel, SimpleSessionManager};
use common::uo_engine::base_handler::BaseCommand;
use common::uo_engine::handler::{DropTarget, EngineCommand};
use common::uo_engine::serial_alloc::SerialAllocator;

use crate::{DemoCommand, DemoWorkerTx, WorldData};

use super::{
    PlayerState,
    containers, interaction, items, movement, spawn, world_events,
    parsed_packet::ParsedPacket,
    game_logic::PendingTeleport,
};

// ── Infrastructure packet dispatch ────────────────────────────────────────

/// Handle infrastructure packets using pre-parsed [`ParsedPacket`].
///
/// Covers: Ping, MoveRequest, SingleClick, GetStatus, DoubleClick
/// (containers/paperdoll), ClientViewRange, ResyncRequest.
///
/// Returns response packets to send to the client, or `None`.
pub(super) async fn handle_infra_packet(
    parsed: &ParsedPacket,
    packet: &RawPacket,
    player: &mut Option<PlayerState>,
    open_containers: &mut containers::OpenContainers,
    held_item: &Option<items::HeldItem>,
    blocking_gump: &mut Option<(u32, u32)>,
    access_level: AccessLevel,
    worker_tx: &DemoWorkerTx,
    observer: &mut Option<ObserverPipeline>,
    addr: std::net::SocketAddr,
) -> Option<Vec<RawPacket>> {
    match parsed {
        // Already handled elsewhere (login, items, game-logic).
        ParsedPacket::GameLogin
        | ParsedPacket::LoginCharacter
        | ParsedPacket::CreateCharacter
        | ParsedPacket::ItemPacket { .. } => None,

        // Ping — echo back.
        ParsedPacket::Ping(ping) => {
            Some(vec![RawPacket::s2c(encode_packet(ping))])
        }

        // MoveRequest — delegate to movement handler.
        ParsedPacket::MoveRequest { .. } => {
            if let Some(p) = player {
                movement::handle_move(packet, p, worker_tx, observer).await
            } else {
                None
            }
        }

        // SingleClick — name label.
        ParsedPacket::SingleClick { .. } => {
            if let Some(p) = player {
                interaction::handle_single_click(packet, p, worker_tx).await
            } else {
                None
            }
        }

        // GetMobileStatus — status bar.
        ParsedPacket::GetStatus { .. } => {
            if let Some(p) = player {
                interaction::handle_get_status(packet, p, held_item, worker_tx).await
            } else {
                None
            }
        }

        // DoubleClick — containers, paperdoll.
        ParsedPacket::DoubleClick { .. } => {
            if let Some(p) = player {
                if let Some(result) = interaction::handle_double_click(packet, p, access_level, worker_tx).await {
                    if let Some(opened) = result.opened_container {
                        open_containers.open(opened.serial, opened.kind);
                    }
                    Some(result.packets)
                } else {
                    None
                }
            } else {
                None
            }
        }

        // GumpMenuSelection — forward to controller (fire-and-forget).
        // Clear blocking_gump if this response matches it.
        ParsedPacket::GumpMenuSelection { serial, gump_id, button_id, switches } => {
            if let Some(p) = player {
                // Clear blocking state when the player answers or closes the gump.
                if *blocking_gump == Some((*serial, *gump_id)) {
                    *blocking_gump = None;
                }
                let cmd = DemoCommand::Base(BaseCommand::ObjectGumpResponse {
                    item_serial: *serial,
                    player_serial: p.serial,
                    gump_id: *gump_id,
                    button_id: *button_id,
                    switches: switches.clone(),
                    text_entries: Vec::new(),
                });
                let _ = worker_tx.send(WorkerCommand::MapCommand(p.world, cmd)).await;
            }
            Some(vec![])
        }

        // ClientViewRange — clamp, persist, update observer, echo.
        ParsedPacket::ClientViewRange { range } => {
            let clamped = range.clamp(
                &packets::system::ClientViewRange::MIN,
                &packets::system::ClientViewRange::MAX,
            );
            log::info!(
                "[session:{addr}] ClientViewRange: requested={}, clamped={}",
                range, clamped,
            );

            // Persist the new range per-session and immediately notify the
            // worker so the ObserverRegistry updates the visible strips.
            if let Some(p) = player.as_mut() {
                let new_range = *clamped as u16;
                if p.view_range != new_range {
                    p.view_range = new_range;
                    let new_rect = TileRect::from_view(p.x, p.y, new_range);
                    p.view_rect = new_rect;
                    let _ = worker_tx
                        .send(WorkerCommand::MapCommand(
                            p.world,
                            crate::DemoCommand::UpdateObserverView(p.serial, new_rect),
                        ))
                        .await;
                }
            }

            let reply = packets::system::ClientViewRange::new(*clamped);
            Some(vec![RawPacket::s2c(encode_packet(&reply))])
        }

        // ResyncRequest — resend world state.
        ParsedPacket::ResyncRequest => {
            handle_resync(player, worker_tx, addr).await
        }

        // SetSkillLock — just store the new lock state and echo back a
        // single-skill update so the client reflects the change.
        ParsedPacket::SetSkillLock { skill_id, lock } => {
            if let Some(p) = player {
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                let entity_lock = crate::skills::lock_from_wire(*lock);
                if let Some(sv) = engine.set_skill_lock(p.serial, *skill_id, entity_lock).await {
                    let upd = crate::skills::build_single_update(*skill_id, &sv);
                    return Some(vec![RawPacket::s2c(upd.to_bytes())]);
                }
            }
            Some(vec![])
        }

        // Not an infrastructure packet.
        _ => None,
    }
}

/// Handle a resync request (0x22) — resend the player's DrawGamePlayer
/// and all visible entities.
async fn handle_resync(
    player: &mut Option<PlayerState>,
    worker_tx: &DemoWorkerTx,
    addr: std::net::SocketAddr,
) -> Option<Vec<RawPacket>> {
    let p = player.as_ref()?;
    log::info!("[session:{addr}] ResyncRequest — resending world state");
    let mut pkts = Vec::new();

    let engine = crate::game_util::engine_for(worker_tx, p.world);

    let entity = engine.get_entity(p.serial).await;
    let (graphic, color) = match entity.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.graphic, m.color),
        _ => (crate::constants::body::MALE_HUMAN, 0),
    };
    let dgp = packets::character::DrawGamePlayer {
        id: 0x20,
        serial: p.serial,
        body_type: graphic,
        _pad0: (),
        hue: color,
        flags: packets::mobile_flags::MobileFlags(0),
        x: p.x,
        y: p.y,
        _pad1: (),
        direction: p.direction,
        z: p.z,
    };
    pkts.push(RawPacket::s2c(encode_packet(&dgp)));

    let entities = engine.query_area(p.view_rect).await;
    for ent in &entities {
        let serial = framework::ecumene::Entity::serial(ent);
        if serial == p.serial {
            continue;
        }
        pkts.push(RawPacket::s2c(ent.to_raw_bytes()));
    }
    log::info!(
        "[session:{addr}] ResyncRequest — sent {} entities",
        entities.len(),
    );
    Some(pkts)
}

// ── Login flow ────────────────────────────────────────────────────────────

/// Handle the GameLogin (0x91) packet.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_game_login(
    packet: &RawPacket,
    parsed: &ParsedPacket,
    test_account: &mut Option<spawn::TestAccountInfo>,
    account_name: &mut Option<String>,
    access_level: &mut AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    session_manager: &Arc<SimpleSessionManager>,
    observer: &mut Option<ObserverPipeline>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
) -> error::Result<bool> {
    if !matches!(parsed, ParsedPacket::GameLogin) {
        return Ok(false);
    }

    if let Some((pkts, level)) = spawn::handle_game_login(
        packet, test_account, account_name, worker_tx, world_data, session_manager, addr,
        client_version,
    ).await {
        *access_level = level;
        for pkt in &pkts {
            if let Some(obs) = observer {
                obs.ingest_s2c(&pkt.data);
            }
            session.send(pkt.clone()).await?;
        }
    }
    Ok(true)
}

/// Handle the LoginCharacter (0x5D) packet: spawn + backpack registration.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_login_character(
    packet: &RawPacket,
    parsed: &ParsedPacket,
    player: &mut Option<PlayerState>,
    test_account: &Option<spawn::TestAccountInfo>,
    account_name: &Option<String>,
    access_level: AccessLevel,
    open_containers: &mut containers::OpenContainers,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    serial_alloc: &Arc<SerialAllocator>,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
) -> error::Result<bool> {
    if !matches!(parsed, ParsedPacket::LoginCharacter) {
        return Ok(false);
    }

    spawn::handle_spawn(
        packet, player, test_account, account_name, access_level, session, worker_tx, world_data,
        serial_alloc, observer, event_rx, event_tx_for_observer, addr, client_version,
    ).await?;

    register_player_backpack(player, open_containers, worker_tx).await;
    Ok(true)
}

/// Handle the CreateCharacter (0x00) packet: create entity + enter world.
///
/// Returns `true` if the packet was consumed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_create_character(
    packet: &RawPacket,
    parsed: &ParsedPacket,
    player: &mut Option<PlayerState>,
    account_name: &Option<String>,
    access_level: AccessLevel,
    open_containers: &mut containers::OpenContainers,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    serial_alloc: &Arc<SerialAllocator>,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
) -> error::Result<bool> {
    if !matches!(parsed, ParsedPacket::CreateCharacter) {
        return Ok(false);
    }

    spawn::handle_create_character(
        packet, player, account_name, access_level, session, worker_tx, world_data,
        serial_alloc, observer, event_rx, event_tx_for_observer, addr, client_version,
    ).await?;

    // Only register the backpack if a player was actually spawned (creation
    // may have been rejected, leaving `player` as `None`).
    if player.is_some() {
        register_player_backpack(player, open_containers, worker_tx).await;
    }
    Ok(true)
}

/// Auto-register the player's backpack as an open container.
async fn register_player_backpack(
    player: &mut Option<PlayerState>,
    open_containers: &mut containers::OpenContainers,
    worker_tx: &DemoWorkerTx,
) {
    if let Some(p) = player {
        let engine = crate::game_util::engine_for(worker_tx, p.world);
        if let Some(entity) = engine.get_entity(p.serial).await {
            if let Some(m) = entity.mobile() {
                if let Some(bp) = m.items
                    .iter()
                    .find(|eq| eq.layer == packets::layer::Layer::Backpack)
                {
                    open_containers.open(bp.serial, containers::ContainerKind::OwnBackpack);
                }
            }
        }
    }
}

// ── Item packet dispatch ──────────────────────────────────────────────────

/// Handle item manipulation packets (0x07 pick up, 0x08 drop, 0x13 wear).
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_item_packets(
    packet: &RawPacket,
    parsed: &ParsedPacket,
    player: &Option<PlayerState>,
    held_item: &mut Option<items::HeldItem>,
    access_level: AccessLevel,
    open_containers: &containers::OpenContainers,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if !matches!(parsed, ParsedPacket::ItemPacket { .. }) {
        return Ok(false);
    }

    if items::handle_pick_up(
        packet, player, held_item, session, worker_tx,
        access_level, open_containers,
    ).await? {
        return Ok(true);
    }
    if items::handle_drop(
        packet, player, held_item, session, worker_tx,
        access_level, open_containers,
    ).await? {
        return Ok(true);
    }
    if items::handle_wear(
        packet, player, held_item, session, worker_tx,
        access_level,
    ).await? {
        return Ok(true);
    }
    Ok(true) // Consumed regardless (it's an item packet).
}

// ── Post-packet hooks ─────────────────────────────────────────────────────

/// Sync view rect + auto-close containers after position change.
pub(super) async fn sync_view_and_containers(
    player: &mut Option<PlayerState>,
    open_containers: &mut containers::OpenContainers,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    if let Some(p) = player {
        movement::sync_view_rect(p, worker_tx).await;
        let closed = open_containers.close_on_move(p.x, p.y);
        for serial in closed {
            session.send(containers::close_container_gump_packet(serial)).await?;
        }
    }
    Ok(())
}

/// Send infra response packets + feed observer.
pub(super) async fn send_infra_response(
    response: Option<Vec<RawPacket>>,
    observer: &mut Option<ObserverPipeline>,
    session: &mut Session,
) -> error::Result<()> {
    if let Some(packets) = &response {
        for pkt in packets {
            if let Some(obs) = observer {
                obs.ingest_s2c(&pkt.data);
            }
            session.send(pkt.clone()).await?;
        }
    }
    Ok(())
}

// ── World event batch processing ──────────────────────────────────────────

/// Maximum number of extra events to drain from the channel per batch.
///
/// Raised from 64 to 512 so the session can keep up with bursts of
/// world activity (e.g. hundreds of NPCs moving simultaneously) without
/// letting the observer channel back up and drop events.
pub(super) const MAX_DRAIN: usize = 512;

/// Drain events from the channel and collect into a buffer.
pub(super) fn drain_events(
    first_event: Arc<WorldEvent>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
) -> Vec<Arc<WorldEvent>> {
    let mut events = Vec::with_capacity(MAX_DRAIN + 1);
    events.push(first_event);
    for _ in 0..MAX_DRAIN {
        match event_rx.try_recv() {
            Ok(extra) => events.push(extra),
            Err(_) => break,
        }
    }
    events
}

/// Collect infrastructure packets for a batch of world events.
///
/// Handles: entity streaming, container updates, entity removal,
/// and targeted controller events (gumps, messages, target cursors).
pub(super) fn collect_infra_event_packets(
    events: &[Arc<WorldEvent>],
    player: &mut PlayerState,
    access_level: AccessLevel,
    observer: &mut Option<ObserverPipeline>,
    open_containers: &mut containers::OpenContainers,
    pending_cursor: &mut Option<super::pending_cursor::PendingCursor>,
    blocking_gump: &mut Option<(u32, u32)>,
    pending_teleport: &mut Option<PendingTeleport>,
    out: &mut Vec<RawPacket>,
) {
    for event in events {
        world_events::collect_world_event_packets(player, event, access_level, observer, out);
        containers::collect_container_update_packets(event, open_containers, player.serial, out, player.client_version);
        if let WorldEvent::EntityRemoved { serial, .. } = event.as_ref() {
            for s in open_containers.close_with_children(*serial) {
                out.push(containers::close_container_gump_packet(s));
            }
        }

        // ── Targeted controller events → UO packets ──────────────
        match event.as_ref() {
            WorldEvent::TargetedGump {
                target_player, source_serial, gump_id,
                gump_x, gump_y, layout, text_lines, blocking, ..
            } if *target_player == player.serial => {
                if *blocking {
                    *blocking_gump = Some((*source_serial, *gump_id));
                }
                let gump = packets::gump::SendGumpDialog {
                    serial: *source_serial,
                    gump_id: *gump_id,
                    x: *gump_x,
                    y: *gump_y,
                    layout: layout.clone(),
                    text_lines: text_lines.iter()
                        .map(|s| packets::gump::GumpTextLine(s.clone()))
                        .collect(),
                    trailing_pad: Vec::new(),
                };
                let compressed = packets::gump::SendCompressedGump::from(&gump);
                out.push(RawPacket::s2c(compressed.to_bytes()));
            }
            WorldEvent::TargetedMessage {
                target_player, message, color, ..
            } if *target_player == player.serial => {
                out.push(RawPacket::s2c(
                    packets::speech::SendSpeech {
                        serial: 0xFFFF_FFFF,
                        model: 0xFFFF,
                        speech_type: packets::speech::SpeechType::System,
                        color: *color,
                        font: 3,
                        name: String::new(),
                        message: message.clone(),
                    }.to_bytes(),
                ));
            }
            WorldEvent::TargetedCloseGump {
                target_player, gump_id, ..
            } if *target_player == player.serial => {
                // Clear blocking state if this close matches the blocking gump.
                if let Some((_, bg_id)) = blocking_gump {
                    if *bg_id == *gump_id {
                        *blocking_gump = None;
                    }
                }
                let gi = packets::system::GeneralInfo::CloseGump {
                    dialog_id: *gump_id,
                    button_id: 0,
                };
                out.push(RawPacket::s2c(gi.to_bytes()));
            }
            WorldEvent::TargetedTargetCursor {
                target_player, cursor_id, cursor_type, ..
            } if *target_player == player.serial => {
                log::info!(
                    "[infra] TargetedTargetCursor: sending 0x6C to player 0x{:08X}, cursor_id=0x{:08X}, cursor_type={}",
                    target_player, cursor_id, cursor_type,
                );
                let cursor = packets::interaction::TargetCursor {
                    id: packets::interaction::TargetCursor::ID,
                    cursor_target: 0,
                    cursor_id: *cursor_id,
                    cursor_type: *cursor_type,
                    target_serial: 0,
                    x: 0,
                    y: 0,
                    _pad0: (),
                    z: 0,
                    graphic: 0,
                };
                out.push(RawPacket::s2c(cursor.to_bytes()));
                // Register pending cursor so the session knows to forward
                // the 0x6C response back to the controller.
                *pending_cursor = Some(
                    super::pending_cursor::PendingCursor::controller(*cursor_id),
                );
            }
            WorldEvent::TargetedCrossWorldTeleport {
                target_player, map_id, x, y, z,
            } if *target_player == player.serial => {
                // A teleporter controller asked to move this player to
                // another world.  Queue it for the async transfer executed
                // by `process_pending_teleport` after the event batch.
                *pending_teleport = Some(PendingTeleport {
                    world: *map_id,
                    x: *x,
                    y: *y,
                    z: *z,
                });
            }
            _ => {}
        }
    }
}

// ── Session cleanup ───────────────────────────────────────────────────────

/// Drop the held item (cursor) on disconnect — places it on the ground.
pub(super) async fn drop_held_item_on_disconnect(
    player: &Option<PlayerState>,
    held_item: &Option<items::HeldItem>,
    worker_tx: &DemoWorkerTx,
) {
    if let (Some(p), Some(hi)) = (player, held_item) {
        let engine = crate::game_util::engine_for(worker_tx, p.world);
        let target = DropTarget::Ground { x: p.x, y: p.y, z: p.z };
        let _ = engine.drop_item(
            p.serial, hi.to_held_info(), target,
            None,
        ).await;
    }
}

/// Clean up after disconnect: unregister observer, arm the logout timer
/// (real accounts) or remove the entity immediately (test accounts).
///
/// - **Test accounts**: `RemoveEntity` immediately (legacy behaviour).
/// - **Playable-pool accounts**: only unregister observer; entity stays
///   resident forever (no timer, unchanged behaviour).
/// - **Real (normal) accounts**: unregister observer, then arm the logout
///   reaper.  After [`crate::logout::DEFAULT_LOGOUT_DELAY`] the reaper
///   transfers the character into the offline-storage zone (map 0xFE) via
///   `transfer_entity`, preserving the full entity state.
pub(super) async fn cleanup_session(
    player: &Option<PlayerState>,
    test_account: &Option<spawn::TestAccountInfo>,
    account_name: &Option<String>,
    worker_tx: &DemoWorkerTx,
    world_data: &std::sync::Arc<crate::WorldData>,
    addr: std::net::SocketAddr,
) {
    let Some(p) = player else { return };

    // Always unregister the observer — the session is dead regardless.
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        p.world,
        DemoCommand::UnregisterObserver(p.serial),
    )).await;

    if test_account.is_some() {
        // ── Test account: immediate despawn (legacy behaviour) ────────
        let _ = worker_tx.send(WorkerCommand::MapCommand(
            p.world,
            DemoCommand::Engine(EngineCommand::RemoveEntity {
                entity_id: p.serial,
            }),
        )).await;
        log::info!("[{addr}] removed test entity {:#010X}", p.serial);
        return;
    }

    // Playable-pool accounts: no timer — stay resident as before.
    if account_name.as_deref().map(spawn::is_playable_account).unwrap_or(false) {
        return;
    }

    // ── Real account: arm the logout reaper ───────────────────────────
    //
    // Fetch the entity to compute the correct logout delay (e.g. Camping
    // skill in a future extension).  If the entity is not found (e.g.
    // already dead/removed), skip arming — nothing to transfer.
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let entity = engine.get_entity(p.serial).await;
    let delay = match &entity {
        Some(e) => crate::logout::logout_delay(e),
        None => {
            log::warn!(
                "[{addr}] cleanup_session: entity {:#010X} not found, skipping logout timer",
                p.serial,
            );
            return;
        }
    };

    let cmd = crate::logout::ReaperCmd::Arm {
        serial: p.serial,
        world: p.world,
        x: p.x,
        y: p.y,
        z: p.z,
        dir: p.direction,
        delay,
    };
    if world_data.reaper_tx.send(cmd).await.is_err() {
        log::warn!("[{addr}] logout reaper channel closed, cannot arm timer for {:#010X}", p.serial);
    }

    // Write META_LOGOUT_PENDING so the position-of-return survives a
    // save+restart that happens before the 20-second timer fires.
    // The reaper removes this key and replaces it with META_LOGOUT_RETURN
    // when the actual transfer to the storage zone completes.
    {
        let return_addr = format!("{}|{}|{}|{}|{}", p.world, p.x, p.y, p.z, p.direction);
        let mut props = engine.get_item_props(p.serial).await.unwrap_or_default();
        props.set_meta(
            crate::logout::META_LOGOUT_PENDING,
            common::uo_engine::item_props::MetaValue::Str(return_addr),
        );
        engine.set_item_props(p.serial, Some(props)).await;
    }

    log::info!(
        "[{addr}] logout timer armed for {:#010X} ({}s)",
        p.serial, delay.as_secs(),
    );
}
