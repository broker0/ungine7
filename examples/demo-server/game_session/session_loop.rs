//! Unified session loop — shared by all session modes (`rust` / `lua` /
//! `controller`).
//!
//! The handler (`GameLogicHandler` impl) is created based on the compile-time
//! feature flag.  All infrastructure (login, movement, items, containers,
//! entity streaming, cleanup) is handled by `infra.rs`.  Game-logic
//! (spells, combat, skills, bandaging, mounting, Lua forwarding) is
//! delegated to the handler.

use std::sync::Arc;

use log::{error, trace};

use protocol::RawPacket;

use network::error::NetworkError;
use network::session::{Session, SessionEvent};

use framework::continuum::WorldEvent;
use framework::diorama::ObserverPipeline;
use framework::ecumene::StaticDataProvider;

use common::uo_engine::auth::SimpleSessionManager;
use common::uo_engine::serial_alloc::SerialAllocator;

use crate::{DemoWorkerTx, WorldData};

use super::{
    CROSS_VALIDATE,
    SessionMode,
    dot_commands, movement, infra,
    parsed_packet::{self, ParsedPacket},
    game_logic::GameLogicHandler,
    session_state::SessionContext,
    rust_handler::RustGameLogicHandler,
};

#[cfg(feature = "lua")]
use super::lua_handler::LuaGameLogicHandler;

#[cfg(feature = "lua")]
use super::controller_handler::ControllerGameLogicHandler;

// ── Public entry point ────────────────────────────────────────────────────

/// Run the game session for a connected client.
///
/// Creates the appropriate `GameLogicHandler` based on the compile-time
/// feature flag, then runs the unified session loop.
pub(crate) async fn run_game_session(
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    static_data: &Option<Arc<dyn StaticDataProvider>>,
    session_manager: &Arc<SimpleSessionManager>,
    serial_alloc: &Arc<SerialAllocator>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
    mut event_rx: tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    #[cfg(feature = "lua")]
    lua_cmd_tx: tokio::sync::mpsc::Sender<crate::lua_script::LuaCommand>,
) -> network::error::Result<()> {
    let observer = if CROSS_VALIDATE {
        Some(ObserverPipeline::new(static_data.clone()))
    } else {
        None
    };

    // ── Create handler based on the session's resolved mode ──────────
    //
    // The mode is taken from the server's current default at connect time.
    // Already-running sessions are unaffected by later `.session` changes.
    let mode = world_data.session_mode();
    let mut handler: Box<dyn GameLogicHandler> = match mode {
        SessionMode::Rust => Box::new(RustGameLogicHandler::new(
            SessionContext::new(observer, serial_alloc.clone(), worker_tx, static_data.clone(), client_version),
        )),
        #[cfg(feature = "lua")]
        SessionMode::Lua => Box::new(LuaGameLogicHandler::new(worker_tx.clone(), observer)),
        #[cfg(feature = "lua")]
        SessionMode::Controller => Box::new(ControllerGameLogicHandler::new(
            worker_tx.clone(),
            observer,
            world_data.controller_script.clone(),
        )),
    };
    log::debug!("[{addr}] session mode: {mode}");

    // ── Main session loop ────────────────────────────────────────────
    loop {
        tokio::select! {
            biased;

            // ── Client packet ────────────────────────────────────────
            event = session.recv() => {
                match event.event {
                    SessionEvent::Seed(_) => {}
                    SessionEvent::Packet(packet) => {
                        let parsed = parsed_packet::parse_packet(&packet);
                        handle_client_packet(
                            &parsed, &packet, handler.as_mut(),
                            session, worker_tx, world_data,
                            session_manager, serial_alloc, addr,
                            &mut event_rx, &event_tx_for_observer,
                            #[cfg(feature = "lua")]
                            &lua_cmd_tx,
                        ).await?;
                        process_pending_teleport(
                            handler.as_mut(), session, worker_tx, world_data,
                            &mut event_rx, &event_tx_for_observer,
                        ).await?;
                    }
                    SessionEvent::Stopped | SessionEvent::Disconnected => {
                        trace!("[{addr}] disconnected");
                        let inf = handler.infra();
                        infra::drop_held_item_on_disconnect(
                            &inf.player, &inf.held_item, worker_tx,
                        ).await;
                        handler.shutdown().await;
                        let inf = handler.infra();
                        infra::cleanup_session(
                            &inf.player, &inf.test_account, &inf.account_name,
                            worker_tx, world_data, addr,
                        ).await;
                        break;
                    }
                    SessionEvent::Error(e) => {
                        error!("[{addr}] error: {e}");
                        let inf = handler.infra();
                        infra::drop_held_item_on_disconnect(
                            &inf.player, &inf.held_item, worker_tx,
                        ).await;
                        handler.shutdown().await;
                        let inf = handler.infra();
                        infra::cleanup_session(
                            &inf.player, &inf.test_account, &inf.account_name,
                            worker_tx, world_data, addr,
                        ).await;
                        return Err(NetworkError::Transport(e));
                    }
                }
            }

            // ── World events ─────────────────────────────────────────
            world_event = event_rx.recv() => {
                if let Some(event) = world_event {
                    handle_world_event_batch(
                        event, handler.as_mut(), &mut event_rx,
                        session, worker_tx,
                    ).await?;
                    // A cross-world teleporter controller may have queued a
                    // pending teleport while collecting this batch; execute it.
                    process_pending_teleport(
                        handler.as_mut(), session, worker_tx, world_data,
                        &mut event_rx, &event_tx_for_observer,
                    ).await?;
                }
            }

            // ── Game-logic timers ────────────────────────────────────
            timer_event = handler.poll_timer() => {
                handler.handle_timer_event(timer_event, session, worker_tx).await?;
                process_pending_teleport(
                    handler.as_mut(), session, worker_tx, world_data,
                    &mut event_rx, &event_tx_for_observer,
                ).await?;
            }
        }
    }

    Ok(())
}

// ── Pending teleport processing ───────────────────────────────────────────

/// Drain and execute a pending/standing teleport for the player, if any.
///
/// Runs at the top level of the session loop (after client-packet handling and
/// after timer events) because cross-world transfers need ownership of
/// `event_rx`, the observer event sender and the observer pipeline — which are
/// only available here.  Detects three triggers:
///
/// 1. An explicit [`PendingTeleport`](super::game_logic::PendingTeleport)
///    queued by recall-to-rune or a teleporter double-click.
/// 2. The player standing on a teleporter object's tile (after a move).
///
/// After a successful teleport the view and open containers are re-synced.
async fn process_pending_teleport(
    handler: &mut dyn GameLogicHandler,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
) -> network::error::Result<()> {
    let inf = handler.infra_mut();
    // Nothing to do until the player has spawned.
    if inf.player.is_none() {
        return Ok(());
    }
    let access_level = inf.access_level;

    // Split-borrow the InfraState fields needed by the teleport routine.
    let teleported = {
        let player = inf.player.as_mut().expect("player checked above");
        super::transfer::maybe_handle_teleport(
            session,
            player,
            &mut inf.pending_teleport,
            access_level,
            worker_tx,
            world_data,
            &mut inf.observer,
            event_rx,
            event_tx_for_observer,
        ).await?
    };

    if teleported {
        let inf = handler.infra_mut();
        infra::sync_view_and_containers(
            &mut inf.player, &mut inf.open_containers,
            session, worker_tx,
        ).await?;
        inf.sync_engine_world();
    }
    Ok(())
}

// ── Client packet dispatch ────────────────────────────────────────────────

async fn handle_client_packet(
    parsed: &ParsedPacket,
    packet: &RawPacket,
    handler: &mut dyn GameLogicHandler,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    session_manager: &Arc<SimpleSessionManager>,
    serial_alloc: &Arc<SerialAllocator>,
    addr: std::net::SocketAddr,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    #[cfg(feature = "lua")]
    lua_cmd_tx: &tokio::sync::mpsc::Sender<crate::lua_script::LuaCommand>,
) -> network::error::Result<()> {
    // ── Dot-command interception ──────────────────────────────────────
    {
        let inf = handler.infra_mut();
        if let Some(handled) = dot_commands::handle_dot_commands(
            packet,
            &mut inf.player,
            &mut inf.pending_cursor,
            inf.access_level,
            session, worker_tx, world_data,
            &mut inf.observer,
            event_rx, event_tx_for_observer,
        ).await? {
            if handled {
                let inf = handler.infra_mut();
                infra::sync_view_and_containers(
                    &mut inf.player, &mut inf.open_containers,
                    session, worker_tx,
                ).await?;
                return Ok(());
            }
        }
    }

    // ── .slua dot-commands (lua-session only, no-op for Rust) ────────
    if handler.handle_session_command(packet, session).await? {
        return Ok(());
    }

    // ── Item manipulation (0x07, 0x08, 0x13) ─────────────────────────
    {
        let inf = handler.infra_mut();
        // Ghosts cannot pick up, drop, or wear items.
        if inf.dead {
            if let ParsedPacket::ItemPacket { id } = parsed {
                // A bare "system message + ignore" leaves the client holding
                // the item on the cursor (it already lifted it visually),
                // which then vanishes.  For a pick-up (0x07) we must send
                // RejectMoveItem (0x27) so the client snaps the item back to
                // its original slot.
                use packets::interaction::{RejectMoveItem, RejectMoveItemReason};
                use packets::traits::BasicPacket;
                if *id == 0x07 {
                    session.send(RawPacket::s2c(
                        RejectMoveItem::new(RejectMoveItemReason::CannotLift).to_bytes(),
                    )).await?;
                }
                session.send(crate::game_util::system_message(
                    "You are dead and cannot do that.",
                )).await?;
                return Ok(());
            }
        }
        if infra::handle_item_packets(
            packet, parsed, &inf.player,
            &mut inf.held_item, inf.access_level,
            &inf.open_containers, session, worker_tx,
        ).await? {
            return Ok(());
        }
    }

    // ── Lua dot-command (.lua) ────────────────────────────────────────
    #[cfg(feature = "lua")]
    if super::lua_commands::handle_lua_dot_command(packet, session, lua_cmd_tx, &world_data.scripts_dir).await? {
        return Ok(());
    }

    // ── Player speech broadcast ──────────────────────────────────────
    // If we reach this point with a speech packet (0x03 or 0xAD), it
    // was not consumed by any dot-command handler.  Broadcast it to all
    // nearby observers as a WorldEvent::Speech.  Additionally, the
    // keywords "buy" / "sell" open a nearby vendor's window (the speech
    // is still broadcast either way, matching classic UO behaviour).
    if packet.id() == 0x03 || packet.id() == 0xAD {
        broadcast_player_speech(packet, handler.infra(), worker_tx).await;
        maybe_open_vendor(packet, handler, session, worker_tx).await?;
        maybe_open_bank(packet, handler, session, worker_tx).await?;
        maybe_pet_command(packet, handler, session, worker_tx).await?;
        super::shipping::maybe_ship_command(packet, handler.infra(), session, worker_tx).await?;
        return Ok(());
    }

    // ── Login flow ───────────────────────────────────────────────────
    {
        let inf = handler.infra_mut();
        if infra::handle_game_login(
            packet, parsed,
            &mut inf.test_account, &mut inf.account_name, &mut inf.access_level,
            session, worker_tx, world_data, session_manager,
            &mut inf.observer, addr,
            inf.client_version,
        ).await? {
            return Ok(());
        }
    }

    {
        let inf = handler.infra_mut();
        if infra::handle_create_character(
            packet, parsed,
            &mut inf.player, &inf.account_name,
            inf.access_level,
            &mut inf.open_containers, session, worker_tx, world_data,
            serial_alloc, &mut inf.observer,
            event_rx, event_tx_for_observer, addr,
            inf.client_version,
        ).await? {
            if handler.infra().player.is_some() {
                handler.on_player_spawned(world_data, addr).await;
            }
            return Ok(());
        }
    }

    {
        let inf = handler.infra_mut();
        if infra::handle_login_character(
            packet, parsed,
            &mut inf.player, &inf.test_account, &inf.account_name,
            inf.access_level,
            &mut inf.open_containers, session, worker_tx, world_data,
            serial_alloc, &mut inf.observer,
            event_rx, event_tx_for_observer, addr,
            inf.client_version,
        ).await? {
            handler.on_player_spawned(world_data, addr).await;
            return Ok(());
        }
    }

    // ── Game-logic handler ───────────────────────────────────────────
    let consumed = handler.handle_packet(parsed, packet, session, worker_tx).await?;

    // ── Infrastructure (if handler didn't consume) ───────────────────
    if !consumed {
        let inf = handler.infra_mut();
        let response = infra::handle_infra_packet(
            parsed, packet,
            &mut inf.player, &mut inf.open_containers,
            &inf.held_item, &mut inf.blocking_gump,
            inf.access_level,
            worker_tx, &mut inf.observer, addr,
        ).await;
        let inf = handler.infra_mut();
        infra::send_infra_response(response, &mut inf.observer, session).await?;
    }

    // ── Post-packet hooks ────────────────────────────────────────────
    {
        let inf = handler.infra_mut();
        infra::sync_view_and_containers(
            &mut inf.player, &mut inf.open_containers,
            session, worker_tx,
        ).await?;
    }

    handler.post_packet(session, worker_tx).await?;

    Ok(())
}

// ── Player speech broadcast ───────────────────────────────────────────────

/// Broadcast a player's speech to all nearby observers.
///
/// Parses the incoming speech packet (0x03 or 0xAD), extracts text, color,
/// font, and speech type, then sends a `BroadcastSpeech` command through
/// the worker so it becomes a `WorldEvent::Speech` visible to all players
/// in range.
async fn broadcast_player_speech(
    packet: &RawPacket,
    infra: &super::game_logic::InfraState,
    worker_tx: &DemoWorkerTx,
) {
    use packets::speech::{TalkRequest, SpeechRequest};
    use packets::traits::ManualPacket;
    use framework::continuum::WorkerCommand;

    let Some(player) = &infra.player else { return };

    // Parse the speech packet to get text, color, font, speech_type.
    let (text, color, font, speech_type_wire) = match packet.id() {
        0x03 => {
            let Ok(req) = TalkRequest::from_bytes(&packet.data) else { return };
            (req.message, req.color, req.font, req.speech_type.to_wire())
        }
        0xAD => {
            let Ok(req) = SpeechRequest::from_bytes(&packet.data) else { return };
            match req {
                SpeechRequest::Plain { message, color, font, speech_type, .. } => {
                    (message.0, color, font, speech_type.to_wire())
                }
                SpeechRequest::WithKeywords { message, color, font, speech_type, .. } => {
                    (message.0, color, font, speech_type.to_wire())
                }
            }
        }
        _ => return,
    };

    if text.is_empty() { return }

    // Get the player entity for graphic and name.
    let engine = crate::game_util::engine_for(
        worker_tx, player.world,
    );
    let entity = engine.get_entity(player.serial).await;
    let (graphic, name) = match entity.as_ref().and_then(|e| e.mobile()) {
        Some(m) => {
            (m.graphic, m.name.clone())
        }
        _ => (0, String::new()),
    };

    let _ = worker_tx.send(WorkerCommand::MapCommand(
        player.world,
        crate::DemoCommand::BroadcastSpeech {
            serial: player.serial,
            graphic,
            speech_type: speech_type_wire,
            color,
            font,
            name,
            message: text,
            x: player.x,
            y: player.y,
        },
    )).await;
}

// ── Vendor keyword handling ───────────────────────────────────────────────

/// If the speech text is the keyword `buy` or `sell`, find the nearest
/// vendor and open its buy/sell window for the player.
///
/// Does nothing if there is no vendor in range.  The speech itself is
/// always broadcast by the caller regardless of the outcome.
async fn maybe_open_vendor(
    packet: &RawPacket,
    handler: &mut dyn GameLogicHandler,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> network::error::Result<()> {
    let Some(text) = common::dot_commands::extract_speech_text(packet) else {
        return Ok(());
    };
    let kw = text.trim().to_ascii_lowercase();
    let want_buy = kw == "buy";
    let want_sell = kw == "sell";
    if !want_buy && !want_sell {
        return Ok(());
    }

    // Find the nearest vendor (needs only a read of the player state).
    let vendor_serial = {
        let Some(player) = handler.infra().player.as_ref() else {
            return Ok(());
        };
        super::vendor_session::find_nearest_vendor(player, worker_tx).await
    };
    let Some(vendor_serial) = vendor_serial else {
        return Ok(());
    };

    // Open the requested window.  Split-borrow the InfraState fields:
    // `player` (shared) plus `open_vendor` / `open_containers` (mutable).
    let inf = handler.infra_mut();
    let Some(player) = inf.player.as_ref() else {
        return Ok(());
    };
    if want_buy {
        super::vendor_session::open_buy_window(
            vendor_serial, player,
            &mut inf.open_vendor,
            session, worker_tx,
        ).await?;
    } else {
        super::vendor_session::open_sell_window(
            vendor_serial, player, session, worker_tx,
        ).await?;
    }
    Ok(())
}

// ── Bank keyword handling ─────────────────────────────────────────────────

/// If the speech text is the keyword `bank`, find the nearest banker
/// and open the player's bank box.
///
/// Does nothing if there is no banker in range.  The speech itself is
/// always broadcast by the caller regardless of the outcome.
async fn maybe_open_bank(
    packet: &RawPacket,
    handler: &mut dyn GameLogicHandler,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> network::error::Result<()> {
    let Some(text) = common::dot_commands::extract_speech_text(packet) else {
        return Ok(());
    };
    let kw = text.trim().to_ascii_lowercase();
    if kw != "bank" {
        return Ok(());
    }

    // Find the nearest banker (needs only a read of the player state).
    let banker_serial = {
        let Some(player) = handler.infra().player.as_ref() else {
            return Ok(());
        };
        super::bank_session::find_nearest_banker(player, worker_tx).await
    };
    let Some(banker_serial) = banker_serial else {
        return Ok(());
    };

    // Open the bank box.
    let inf = handler.infra_mut();
    let Some(player) = inf.player.as_ref() else {
        return Ok(());
    };
    super::bank_session::open_bank_box(
        banker_serial,
        player,
        &mut inf.open_containers,
        session, worker_tx,
    ).await?;
    Ok(())
}

// ── Pet command handling ──────────────────────────────────────────────────

/// If the speech text is a pet command (`all come`, `all stop`, `all release`),
/// apply it to the player's nearby pets.
///
/// Pets are mobiles carrying `meta["pet_owner"] == player.serial`.  Commands
/// are delivered via meta-state: the pet's `PetController` polls
/// `meta["pet_command"]` on its follow timer.
///
/// - `all come`    → `pet_command = "follow"`
/// - `all stop`    → `pet_command = "stay"`
/// - `all release` → un-tame: clear pet meta and revert to a wandering NPC.
///
/// The speech itself is always broadcast by the caller regardless.
async fn maybe_pet_command(
    packet: &RawPacket,
    handler: &mut dyn GameLogicHandler,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> network::error::Result<()> {
    use common::uo_engine::entity::DemoEntity;
    use common::uo_engine::item_props::MetaValue;
    use crate::taming;

    let Some(text) = common::dot_commands::extract_speech_text(packet) else {
        return Ok(());
    };
    let kw = text.trim().to_ascii_lowercase();

    enum PetAction { Follow, Stay, Release }
    let action = match kw.as_str() {
        "all come"    => PetAction::Follow,
        "all stop"    => PetAction::Stay,
        "all release" => PetAction::Release,
        _ => return Ok(()),
    };

    let (player_serial, world, px, py) = {
        let Some(player) = handler.infra().player.as_ref() else {
            return Ok(());
        };
        (player.serial, player.world, player.x, player.y)
    };

    let engine = crate::game_util::engine_for(worker_tx, world);

    // Find the player's pets within earshot.
    const PET_COMMAND_RANGE: u16 = 12;
    let area = framework::ecumene::TileRect::from_view(px, py, PET_COMMAND_RANGE);
    let entities = engine.query_area(area).await;

    let mut affected = 0u32;
    for ent in &entities {
        let DemoEntity::Mobile(m) = ent else { continue };
        let Some(mut props) = engine.get_item_props(m.serial).await else { continue };
        // Only this player's pets.
        if props.get_meta_int(taming::META_PET_OWNER) != Some(player_serial as i64) {
            continue;
        }

        match action {
            PetAction::Follow => {
                props.set_meta(taming::META_PET_COMMAND, MetaValue::Str(taming::CMD_FOLLOW.to_string()));
                engine.set_item_props(m.serial, Some(props)).await;
            }
            PetAction::Stay => {
                props.set_meta(taming::META_PET_COMMAND, MetaValue::Str(taming::CMD_STAY.to_string()));
                engine.set_item_props(m.serial, Some(props)).await;
            }
            PetAction::Release => {
                // Un-tame: drop pet meta and revert to a wandering NPC.
                props.remove_meta(taming::META_PET_OWNER);
                props.remove_meta(taming::META_PET_COMMAND);
                engine.set_item_props(m.serial, Some(props)).await;

                let controller = Box::new(crate::controller_registry::WanderController::new(
                    std::time::Duration::from_secs(3),
                ));
                let _ = worker_tx.send(framework::continuum::WorkerCommand::MapCommand(
                    world,
                    crate::DemoCommand::AttachControllerPersist {
                        serial: m.serial,
                        controller,
                        controller_id: crate::controller_registry::controller_id("wander", "3"),
                    },
                )).await;
            }
        }
        affected += 1;
    }

    if affected > 0 {
        let msg = match action {
            PetAction::Follow  => "Your pets will follow you.",
            PetAction::Stay    => "Your pets will stay.",
            PetAction::Release => "You release your pets back into the wild.",
        };
        session.send(crate::game_util::system_message_gray(msg)).await?;
    }

    Ok(())
}

// ── World event batch processing ──────────────────────────────────────────

async fn handle_world_event_batch(
    first_event: Arc<WorldEvent>,
    handler: &mut dyn GameLogicHandler,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> network::error::Result<()> {
    if handler.infra().player.is_none() { return Ok(()) }

    let events = infra::drain_events(first_event, event_rx);
    let mut batch = Vec::new();

    // Phase 1: infrastructure packet collection.
    {
        let inf = handler.infra_mut();
        let p = inf.player.as_mut().unwrap();
        infra::collect_infra_event_packets(
            &events, p, inf.access_level,
            &mut inf.observer,
            &mut inf.open_containers, &mut inf.pending_cursor,
            &mut inf.blocking_gump,
            &mut inf.pending_teleport,
            &mut batch,
        );
    }

    // Phase 2: game-logic event processing.
    handler.handle_world_events(&events, &mut batch);

    if !batch.is_empty() {
        // If this drain carried a ship (multi) movement/turn, wrap the whole
        // batch in PauseClient(1)…PauseClient(0).  This makes the client
        // freeze rendering, apply the player snap (0x20) and the hull redraw
        // (0x1A) together, and present them as one consistent frame — exactly
        // how real shards keep on-deck mobiles from jittering during a sail
        // tick.  PauseClient is harmless for any non-ship packets that happen
        // to share the batch.
        let ship_tick = events
            .iter()
            .any(|e| common::world_events::is_ship_move_event(e));

        if ship_tick {
            use packets::system::PauseClient;
            use packets::traits::{encode_packet};
            let pause = RawPacket::s2c(encode_packet(&PauseClient::pause()));
            let resume = RawPacket::s2c(encode_packet(&PauseClient::resume()));
            batch.insert(0, pause);
            batch.push(resume);
        }

        session.send_all(batch).await?;
    }

    // Phase 3: post-event hooks.
    handler.post_world_events(session, worker_tx).await?;

    // Sync view rect.
    if let Some(p) = handler.infra_mut().player.as_mut() {
        movement::sync_view_rect(p, worker_tx).await;
    }

    Ok(())
}
