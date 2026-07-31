//! Per-client game session for path-server.
//!
//! Handles character spawn, movement, view streaming, and basic interaction.
//! Mirrors demo-server's `game_session/mod.rs` but uses `PathServerCommand`
//! instead of `DemoCommand`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{error, info, trace};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};
use protocol::packets::system::Ping;

use network::error::{self, NetworkError};
use network::session::{Session, SessionEvent};

use packets::interaction::{PickUpItem, RejectMoveItem, RejectMoveItemReason};
use packets::mobile_flags::MobileFlags;
use packets::movement::{MoveAck, MoveReject, MoveRequest, Notoriety};
use packets::speech::{SendSpeech, SpeechType};
use packets::system::EnableFeatures;
use packets::gump::GumpMenuSelection;

use framework::continuum::{WorkerCommand, WorldEvent};
use framework::ecumene::{TileRect, StaticDataProvider as EcumeneStaticDataProvider, Entity};

use u_core::{Facing, Heading, ProtocolVersion};

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::rpc::EngineProxy;

use crate::worker::{PathServerCommand, PathServerWorkerTx};
use crate::state::AppState;

use super::world_events::{collect_world_event_packets, handle_world_event};

// ── Configuration ─────────────────────────────────────────────────────────

/// Chebyshev view range (tiles in each direction from the player).
const VIEW_RANGE: u16 = 18;

/// Gump ID for the `.menu` / `.commands` command menu.
///
/// Uses the shared command-gump base so it never collides with the
/// `PendingTarget` cursor IDs (which use `CMD_CURSOR_BASE`).
const MENU_GUMP_ID: u32 = common::dot_commands::CMD_GUMP_BASE | 0x701;

// ── PlayerState ───────────────────────────────────────────────────────────

/// Pending target cursor request (click-to-teleport, pathvis, losvis, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingTarget {
    /// Single teleport — cursor disappears after one click.
    Teleport,
    /// Multi-teleport — cursor re-appears after each click (chain).
    MultiTeleport,
    /// Visual pathfinding — click a tile to run `.pathvis` to it.
    PathVis,
    /// LOS ray — click a tile to trace LOS from player position.
    LosVis,
    /// LOS ray — click a tile to trace LOS from an explicit point.
    LosVisFrom { x: u16, y: u16, z: i8 },
    /// LOS field — click an entity (or tile) to visualise its LOS field.
    LosField { radius: u16, delay_us: u64 },
}

impl PendingTarget {
    /// Unique cursor ID sent in the `TargetCursor` S→C packet.
    fn cursor_id(self) -> u32 {
        common::dot_commands::CMD_CURSOR_BASE
            | match self {
                Self::Teleport      => 0x10,
                Self::MultiTeleport => 0x11,
                Self::PathVis       => 0x12,
                Self::LosVis        => 0x13,
                Self::LosVisFrom { .. } => 0x14,
                Self::LosField { .. }   => 0x15,
            }
    }
}

/// Authoritative player state for a connected session.
///
/// Uses the shared `PlayerState` from common with `Option<PendingTarget>`
/// as server-specific extension.
pub type PlayerState = common::world_events::PlayerState<Option<PendingTarget>>;

// ── Public entry point ────────────────────────────────────────────────────

pub async fn run_game_session(
    session: &mut Session,
    state: &Arc<AppState>,
    addr: std::net::SocketAddr,
    client_version: ProtocolVersion,
    mut event_rx: tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
) -> error::Result<()> {
    let worker_tx = &state.worker_tx;

    let mut player: Option<PlayerState> = None;
    let mut account_name: Option<String> = None;

    // Channel for returning path marker serials from fire-and-forget pathvis tasks.
    let (pathvis_marker_tx, mut pathvis_marker_rx) =
        tokio::sync::mpsc::channel::<Vec<u32>>(4);
    // Serials of path markers left alive from the last `.pathvis` run.
    let mut pathvis_serials: Vec<u32> = Vec::new();

    loop {
        tokio::select! {
            biased;

            event = session.recv() => {
                match event.event {
                    SessionEvent::Seed(_) => {}
                    SessionEvent::Packet(packet) => {
                        // LoginCharacter (0x5D) — spawn phase
                        if packet.id() == 0x5D {
                            handle_spawn(
                                &packet,
                                &mut player,
                                &account_name,
                                session,
                                worker_tx,
                                state,
                                &mut event_rx,
                                &event_tx_for_observer,
                                addr,
                                client_version,
                            )
                            .await?;
                            continue;
                        }

                        // Dot-command interception (speech packets)
                        if packet.id() == 0x03 || packet.id() == 0xAD {
                            if let Some(p) = &mut player {
                                if handle_speech(&packet, p, worker_tx, state, session, &mut pathvis_serials, &pathvis_marker_tx).await? {
                                    // View rect may have changed after teleport.
                                    sync_view_rect(p, worker_tx).await;
                                    continue;
                                }
                            }
                        }

                        // TargetCursor response (0x6C) — click-to-teleport / pathvis
                        if packet.id() == 0x6C {
                            if let Some(p) = &mut player {
                                if handle_target_response(&packet, p, worker_tx, state, session, &mut pathvis_serials, &pathvis_marker_tx).await? {
                                    sync_view_rect(p, worker_tx).await;
                                    continue;
                                }
                            }
                        }

                        // GumpMenuSelection response (0xB1) — command menu buttons
                        if packet.id() == 0xB1 {
                            if let Some(p) = &mut player {
                                if handle_gump_response(&packet, p, worker_tx, session).await? {
                                    sync_view_rect(p, worker_tx).await;
                                    continue;
                                }
                            }
                        }

                        let response = handle_packet(
                            &packet,
                            &mut player,
                            &mut account_name,
                            worker_tx,
                            state,
                            addr,
                            client_version,
                        )
                        .await;

                        if let Some(packets) = response {
                            for pkt in packets {
                                session.send(pkt).await?;
                            }
                        }

                        // Sync view rect after any packet that may have changed position.
                        if let Some(p) = &mut player {
                            sync_view_rect(p, worker_tx).await;
                        }
                    }
                    SessionEvent::Stopped | SessionEvent::Disconnected => {
                        trace!("[{addr}] disconnected");
                        cleanup_session(&player, worker_tx).await;
                        break;
                    }
                    SessionEvent::Error(e) => {
                        error!("[{addr}] error: {e}");
                        cleanup_session(&player, worker_tx).await;
                        if !pathvis_serials.is_empty() {
                            let world = player.as_ref().map(|p| p.world).unwrap_or(0);
                            crate::pf::visual::cleanup_markers(&pathvis_serials, worker_tx, world).await;
                        }
                        return Err(NetworkError::Transport(e));
                    }
                }
            }

            world_event = event_rx.recv() => {
                if let Some(event) = world_event {
                    if let Some(p) = &mut player {
                        const MAX_DRAIN: usize = 64;
                        let mut batch = Vec::new();
                        collect_world_event_packets(p, &event, &mut batch);

                        for _ in 0..MAX_DRAIN {
                            match event_rx.try_recv() {
                                Ok(extra) => collect_world_event_packets(p, &extra, &mut batch),
                                Err(_) => break,
                            }
                        }

                        if !batch.is_empty() {
                            session.send_all(batch).await?;
                        }

                        sync_view_rect(p, worker_tx).await;
                    }
                }
            }

            serials = pathvis_marker_rx.recv() => {
                if let Some(s) = serials {
                    pathvis_serials = s;
                }
            }
        }
    }

    // Clean up leftover path markers on disconnect.
    if !pathvis_serials.is_empty() {
        let world = player.as_ref().map(|p| p.world).unwrap_or(0);
        crate::pf::visual::cleanup_markers(&pathvis_serials, worker_tx, world).await;
    }

    Ok(())
}

// ── Packet dispatch ───────────────────────────────────────────────────────

async fn handle_packet(
    packet: &RawPacket,
    player: &mut Option<PlayerState>,
    account_name: &mut Option<String>,
    worker_tx: &PathServerWorkerTx,
    state: &Arc<AppState>,
    addr: std::net::SocketAddr,
    client_version: ProtocolVersion,
) -> Option<Vec<RawPacket>> {
    match packet.id() {
        // GameLogin (0x91) — send EnableFeatures + CharacterList
        0x91 => handle_game_login(packet, account_name, worker_tx, addr, client_version).await,

        // LoginCharacter — handled in main loop
        0x5D => None,

        // Ping — echo back
        0x73 => Ping::from_bytes(&packet.data)
            .ok()
            .map(|ping| vec![RawPacket::s2c(encode_packet(&ping))]),

        // MoveRequest
        0x02 => {
            if let Some(p) = player {
                handle_move(packet, p, worker_tx).await
            } else {
                None
            }
        }

        // SingleClick — name label
        0x09 => {
            if let Some(p) = player {
                handle_single_click(packet, p, worker_tx).await
            } else {
                None
            }
        }

        // GetMobileStatus
        0x34 => {
            if let Some(p) = player {
                handle_get_status(packet, p, worker_tx).await
            } else {
                None
            }
        }

        // DoubleClick — paperdoll / door toggle / describe
        0x06 => {
            if let Some(p) = player {
                handle_double_click(packet, p, worker_tx, state).await
            } else {
                None
            }
        }

        // PickUpItem — reject (read-only)
        0x07 => {
            if let Ok(_) = PickUpItem::from_bytes(&packet.data) {
                Some(vec![RawPacket::s2c(
                    RejectMoveItem::new(RejectMoveItemReason::CannotLift).to_bytes(),
                )])
            } else {
                None
            }
        }

        // ClientVersionResponse (0xBD) — silently ignore.
        0xBD => None,

        // GeneralInfo (0xBF) — silently ignore.
        0xBF => None,

        _ => None,
    }
}

// ── 0x91 GameLogin ────────────────────────────────────────────────────────

async fn handle_game_login(
    packet: &RawPacket,
    account_name: &mut Option<String>,
    _worker_tx: &PathServerWorkerTx,
    addr: std::net::SocketAddr,
    client_version: ProtocolVersion,
) -> Option<Vec<RawPacket>> {
    use protocol::packets::login::*;

    let name = if let Ok(login) = GameLogin::from_bytes(&packet.data) {
        trace!(
            "[{addr}] game login: '{}' (auth=0x{:08X})",
            &*login.account, login.auth_key
        );
        login.account.to_string()
    } else {
        return None;
    };

    *account_name = Some(name.clone());

    let mut packets = vec![RawPacket::s2c(EnableFeatures::new(0x0002, client_version).to_bytes())];

    // Single character slot — "PathServer Player"
    let display_name = if name.is_empty() {
        "PathServer Player".to_string()
    } else {
        name.clone()
    };

    let char_list = CharacterList::new(
        {
            let mut slots = vec![CharacterSlot::new(&display_name)];
            while slots.len() < 5 {
                slots.push(CharacterSlot::new(""));
            }
            slots
        },
        vec![StartingLocation {
            index: 0,
            city_name: "World".into(),
            area_name: "Path Server".into(),
        }],
        1024,
    );
    packets.push(RawPacket::s2c(encode_packet(&char_list)));

    Some(packets)
}

// ── 0x5D LoginCharacter / Spawn ───────────────────────────────────────────

async fn handle_spawn(
    _packet: &RawPacket,
    player: &mut Option<PlayerState>,
    account_name: &Option<String>,
    session: &mut Session,
    worker_tx: &PathServerWorkerTx,
    _state: &Arc<AppState>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    addr: std::net::SocketAddr,
    client_version: ProtocolVersion,
) -> error::Result<()> {
    // ── Deterministic, collision-resistant serials ─────────────────────
    //
    // The shadow world is fed from a replay/mirror, whose entities keep
    // their ORIGINAL serials.  Real UO serials cluster low: mobiles in
    // `0x0000_0001..` (players are often tiny — even `1`), items in
    // `0x4000_0000..`.  pf markers live high in the item range
    // (`0x6000_0000` losvis, `0x7000_0000` pathvis).
    //
    // To keep a live player from colliding with an ingested replay mobile
    // (which previously turned the player into "an orc" and dropped the
    // mount), we place player + mount + backpack in reserved windows that
    // real captures and markers practically never reach.  The serials are
    // DETERMINISTIC per account so a player keeps the same entity (and its
    // saved position) across relogins.

    /// Reserved base for player mobile serials — top of the mobile range,
    /// where real/replay mobiles practically never land.
    const PLAYER_SERIAL_BASE: u32 = 0x3F00_0000;
    /// Reserved base for player mount items — a quiet gap between gameplay
    /// items (`0x4000_0000..`) and pf markers (`0x6000_0000..`).
    const MOUNT_SERIAL_BASE: u32 = 0x5000_0000;
    /// Reserved base for player backpacks — adjacent to the mount window.
    const BACKPACK_SERIAL_BASE: u32 = 0x5800_0000;

    // Per-account index: `testN` → N; other accounts → hashed.
    let account_index: u32 = {
        let name = account_name.as_deref().unwrap_or("");
        let raw = if let Some(suffix) = name.strip_prefix("test") {
            suffix.parse::<u32>().unwrap_or(1)
        } else {
            name.bytes()
                .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32))
        };
        // Keep each per-account window well clear of its neighbour bases.
        raw & 0x00FF_FFFF
    };

    let player_serial: u32 = PLAYER_SERIAL_BASE + account_index;
    let mount_serial: u32 = MOUNT_SERIAL_BASE + account_index;
    let backpack_serial: u32 = BACKPACK_SERIAL_BASE + account_index;

    let world: u8 = 0; // Felucca
    let graphic: u16 = 0x0190; // male human
    let hue: u16 = 0x0481;
    let name = account_name.clone().unwrap_or_else(|| "PathServer Player".to_string());

    // Try to find existing entity or fall back to Britain bank.
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), world);
    let (x, y, z, direction) = match engine.get_entity(player_serial).await {
        Some(DemoEntity::Mobile(m)) => (m.x, m.y, m.z, m.direction),
        _ => {
            // Spawn at Britain bank.
            let spawn_x = 1438u16;
            let spawn_y = 1697u16;
            let spawn_z = engine.resolve_z(spawn_x, spawn_y, 0, Heading::South)
                .await
                .unwrap_or(0);

            // Create the entity with a horse mount.  Mount/backpack serials
            // are deterministic (reserved windows above) so they stay stable
            // across relogins and never collide with replay items or markers.
            let entity = common::spawn::new_player_entity(
                player_serial, spawn_x, spawn_y, spawn_z, 0,
                &name, graphic, hue,
                100, 100, 100, // hits, mana, stamina
                75, 25, 50,    // str, dex, int
                backpack_serial,
                mount_serial,
                std::collections::BTreeMap::new(), // path-server has no skills
            );
            engine.spawn_entity(player_serial, entity).await;

            (spawn_x, spawn_y, spawn_z, 0u8)
        }
    };

    info!(
        "[{addr}] '{}' ({:#010X}) entering world at ({},{},{}) world={}",
        name, player_serial, x, y, z, world
    );

    // Initialize player state.
    let view_rect = TileRect::from_view(x, y, VIEW_RANGE);
    *player = Some(PlayerState {
        serial: player_serial,
        world,
        x,
        y,
        z,
        direction,
        view_rect,
        view_range: VIEW_RANGE,
        move_throttle: HashMap::new(),
        throttle_interval: Duration::ZERO,
        notoriety_ctx: None,
        client_version,
        extra: None,
    });

    // ── Build spawn packets ────────────────────────────────────────────
    //
    // Order must follow the bootstrap protocol exactly:
    //   0x1B CharacterLocaleAndBody
    //   0xB9 EnableFeatures
    //   0xBF SetMap  (sub-command 0x0008)
    //   0x20 DrawGamePlayer
    //   entity packets  (from observer initial stream)
    //   0x55 LoginComplete

    let spawn_pkts: Vec<RawPacket> = vec![
        // 1. Tell the client who/where the player is and map dimensions.
        common::spawn::build_character_locale_and_body(
            player_serial, graphic, x, y, z, direction, 0x1800, 0x1000,
        ),
        // 2. EnableFeatures — choose legacy (3 bytes, u16) or extended (5 bytes, u32)
        //    depending on the real client version (boundary: 6.0.14.2).
        RawPacket::s2c(EnableFeatures::new(0x0002, client_version).to_bytes()),
        // 3. SetMap — switch to the correct facet (world 0 = Felucca).
        {
            use packets::system::GeneralInfo;
            use packets::traits::ManualPacket;
            RawPacket::s2c(GeneralInfo::SetMap { world }.to_bytes())
        },
        // 4. DrawGamePlayer — authoritative position.
        common::spawn::build_draw_game_player(
            player_serial, graphic, hue, x, y, z, direction,
        ),
    ];

    for pkt in &spawn_pkts {
        session.send(pkt.clone()).await?;
    }

    // ── Register observer & stream initial entities ───────────────────
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            world,
            PathServerCommand::RegisterObserver(
                player_serial,
                world,
                view_rect,
                event_tx_for_observer.clone(),
                reply_tx,
            ),
        ))
        .await;
    let _ = reply_rx.await;

    // Drain initial EntitySpawned events.
    {
        let p = player.as_mut().unwrap();
        while let Ok(event) = event_rx.try_recv() {
            handle_world_event(session, p, &event).await?;
        }
    }

    // LoginComplete (0x55)
    {
        use packets::system::LoginComplete;
        session.send(RawPacket::s2c(encode_packet(&LoginComplete::new()))).await?;
    }

    // Post-login packets
    let mut post = common::spawn::build_post_login_defaults(0x00, 0x00);
    // Welcome message
    post.push(common::spawn::build_welcome_message(
        player_serial,
        graphic,
        "System",
        &format!(
            "Welcome to Path Server, {name} ({x}, {y}, {z}). \
             Use .path X Y to run pathfinding, or .menu for the command menu.",
        ),
    ));

    for pkt in post {
        session.send(pkt).await?;
    }

    // Full status bar.
    if let Some(entity) = engine.get_entity(player_serial).await {
        if let Some(sbi_pkt) = common::spawn::build_status_bar(&entity, true) {
            session.send(sbi_pkt).await?;
        }
    }

    // Open the command menu automatically so the player can pick an action.
    if let Some(p) = player.as_ref() {
        send_command_menu_gump(p, session).await?;
    }

    Ok(())
}

// ── Movement ─────────────────────────────────────────────────────────────

async fn handle_move(
    packet: &RawPacket,
    player: &mut PlayerState,
    worker_tx: &PathServerWorkerTx,
) -> Option<Vec<RawPacket>> {
    let req = MoveRequest::from_bytes(&packet.data).ok()?;
    let heading = Heading::from_raw(req.heading())?;
    let running = req.is_running();
    let facing = Facing::from_heading(heading).with_running(running);

    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let result = engine.mobile_step(player.serial, facing).await;

    match result {
        Some(step) => {
            player.x = step.x;
            player.y = step.y;
            player.z = step.z;
            player.direction = step.direction;

            Some(vec![RawPacket::s2c(encode_packet(&MoveAck {
                id: MoveAck::ID,
                sequence: req.sequence,
                notoriety: Notoriety::Innocent,
            }))])
        }
        None => Some(vec![RawPacket::s2c(encode_packet(&MoveReject {
            id: 0x21,
            sequence: req.sequence,
            x: player.x,
            y: player.y,
            direction: player.direction,
            z: player.z,
        }))]),
    }
}

/// Recompute view rect and notify worker if it changed.
async fn sync_view_rect(player: &mut PlayerState, worker_tx: &PathServerWorkerTx) {
    let new_rect = TileRect::from_view(player.x, player.y, VIEW_RANGE);
    if new_rect != player.view_rect {
        player.view_rect = new_rect;
        let _ = worker_tx
            .send(WorkerCommand::MapCommand(
                player.world,
                PathServerCommand::UpdateObserverView(
                    player.serial,
                    new_rect,
                ),
            ))
            .await;
    }
}

// ── Interaction ───────────────────────────────────────────────────────────

async fn handle_single_click(
    packet: &RawPacket,
    player: &PlayerState,
    worker_tx: &PathServerWorkerTx,
) -> Option<Vec<RawPacket>> {
    use packets::interaction::SingleClick;
    let click = SingleClick::from_bytes(&packet.data).ok()?;
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let entity = engine.get_entity(click.serial).await?;

    let (name, graphic) = match &entity {
        DemoEntity::Mobile(m) => {
            let label = if m.name.is_empty() {
                format!("[mob 0x{:04X}]", m.graphic)
            } else {
                m.name.clone()
            };
            (label, m.graphic)
        }
        DemoEntity::Item { graphic, .. } => (format!("[item 0x{:04X}]", graphic), *graphic),
        DemoEntity::Multi { graphic, .. } => (format!("[multi 0x{:04X}]", graphic), *graphic),
    };

    Some(vec![RawPacket::s2c(
        SendSpeech {
            serial: click.serial,
            model: graphic,
            speech_type: SpeechType::Normal,
            color: 0x03B2,
            font: 3,
            name: name.clone(),
            message: name,
        }
        .to_bytes(),
    )])
}

async fn handle_get_status(
    packet: &RawPacket,
    player: &PlayerState,
    worker_tx: &PathServerWorkerTx,
) -> Option<Vec<RawPacket>> {
    use packets::interaction::GetMobileStatus;
    let req = GetMobileStatus::from_bytes(&packet.data).ok()?;
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let entity = engine.get_entity(req.serial).await?;
    let is_self = entity.serial() == player.serial;
    let pkt = common::spawn::build_status_bar(&entity, is_self)?;
    Some(vec![pkt])
}

async fn handle_double_click(
    packet: &RawPacket,
    player: &PlayerState,
    worker_tx: &PathServerWorkerTx,
    state: &Arc<AppState>,
) -> Option<Vec<RawPacket>> {
    use packets::interaction::DoubleClick;
    let dc = DoubleClick::from_bytes(&packet.data).ok()?;
    let is_paperdoll = dc.serial & 0x8000_0000 != 0;
    let clean_serial = dc.serial & 0x7FFF_FFFF;

    if is_paperdoll {
        return open_paperdoll(clean_serial, player, worker_tx).await;
    }

    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let entity = engine.get_entity(clean_serial).await;
    if matches!(&entity, Some(DemoEntity::Mobile(_))) {
        return open_paperdoll(clean_serial, player, worker_tx).await;
    }

    // Door toggle — if the double-clicked item is a door, open/close it.
    if let Some(DemoEntity::Item { graphic, x, y, z, color, amount, is_container, hidden, facing, .. }) = &entity {
        if try_toggle_door(
            clean_serial,
            *graphic, *x, *y, *z, *color, *amount, *is_container, *hidden, *facing,
            player, worker_tx, state, &engine,
        )
        .await
        {
            return None;
        }
    }

    let desc = match &entity {
        Some(DemoEntity::Item { graphic, serial, .. }) => {
            format!("item {:#06X} ({:#010X})", graphic, serial)
        }
        Some(DemoEntity::Multi { graphic, serial, .. }) => {
            format!("multi {:#06X} ({:#010X})", graphic, serial)
        }
        _ => format!("unknown object {:#010X}", clean_serial),
    };

    Some(vec![RawPacket::s2c(
        SendSpeech {
            serial: 0xFFFF_FFFF,
            model: 0xFFFF,
            speech_type: SpeechType::System,
            color: 0x03B2,
            font: 3,
            name: String::new(),
            message: format!("[path-server] {}", desc),
        }
        .to_bytes(),
    )])
}

/// Open or close a door item.  Returns `true` if `graphic` was a door and the
/// toggle was handled (so the caller should not fall through to the
/// describe-item path), `false` if it was not a door.
#[allow(clippy::too_many_arguments)]
async fn try_toggle_door(
    serial: u32,
    graphic: u16,
    x: u16,
    y: u16,
    z: i8,
    color: u16,
    amount: u16,
    is_container: bool,
    hidden: bool,
    facing: Option<u8>,
    player: &PlayerState,
    worker_tx: &PathServerWorkerTx,
    state: &Arc<AppState>,
    engine: &EngineProxy<PathServerCommand>,
) -> bool {
    use crate::doors;

    // Is this graphic a door?  Authoritative tiledata flag first, falling
    // back to the arithmetic block test when no static data is loaded.
    let is_door = match state.static_data.0.as_deref() {
        Some(sd) => sd
            .static_tile_def(graphic)
            .map(|d| d.flags.has(files::tiledata::TileFlags::DOOR))
            .unwrap_or(false),
        None => doors::is_door_graphic(graphic),
    };
    if !is_door {
        return false;
    }

    let state_ = doors::classify(graphic);
    let opening = !state_.is_open;
    let (new_graphic, dx, dy) = doors::toggle_target(graphic);
    let new_x = (x as i32 + dx as i32) as u16;
    let new_y = (y as i32 + dy as i32) as u16;

    // A door cannot close onto a mobile standing in the doorway.  If the tile
    // the leaf would return to is occupied, leave it open and reschedule a
    // prompt retry so it shuts soon after the doorway clears.
    if !opening {
        let rect = TileRect { x_min: new_x, y_min: new_y, x_max: new_x, y_max: new_y };
        let blocked = engine.query_area(rect).await.iter().any(|e| e.is_mobile());
        if blocked {
            let close_at = crate::worker::door_clock_now_ms() + doors::DOOR_RETRY_CLOSE_MS;
            schedule_door_close(worker_tx, player.world, serial, Some(close_at)).await;
            return true;
        }
    }

    let updated = DemoEntity::Item {
        serial,
        graphic: new_graphic,
        color,
        amount,
        x: new_x,
        y: new_y,
        z,
        is_container,
        hidden,
        facing,
    };
    engine.update_entity(serial, updated).await;

    // Opening schedules the auto-close; closing cancels any pending close.
    let at = opening.then(|| crate::worker::door_clock_now_ms() + doors::DOOR_AUTO_CLOSE_MS);
    schedule_door_close(worker_tx, player.world, serial, at).await;

    true
}

/// Send a [`PathServerCommand::ScheduleDoorClose`] to the worker for `world`.
async fn schedule_door_close(
    worker_tx: &PathServerWorkerTx,
    world: u8,
    serial: u32,
    at: Option<i64>,
) {
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            world,
            PathServerCommand::ScheduleDoorClose { serial, at },
        ))
        .await;
}

async fn open_paperdoll(
    serial: u32,
    player: &PlayerState,
    worker_tx: &PathServerWorkerTx,
) -> Option<Vec<RawPacket>> {
    use packets::character::OpenPaperdoll;
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let entity = engine.get_entity(serial).await;
    let Some(DemoEntity::Mobile(m)) = &entity else {
        return None;
    };

    let is_self = m.serial == player.serial;
    let label = if m.name.is_empty() {
        format!("[mob 0x{:04X}]", m.serial)
    } else {
        m.name.clone()
    };
    let mut flags = m.status;
    if is_self {
        flags = MobileFlags(flags.0 | 0x02);
    }

    Some(vec![RawPacket::s2c(encode_packet(&OpenPaperdoll {
        id: OpenPaperdoll::ID,
        serial: m.serial,
        text: packets::u_io::FixedString::new(&label),
        flags,
    }))])
}

// ── Dot commands ──────────────────────────────────────────────────────────

async fn handle_speech(
    packet: &RawPacket,
    player: &mut PlayerState,
    worker_tx: &PathServerWorkerTx,
    state: &Arc<AppState>,
    session: &mut Session,
    pathvis_serials: &mut Vec<u32>,
    pathvis_marker_tx: &tokio::sync::mpsc::Sender<Vec<u32>>,
) -> error::Result<bool> {
    let text = match extract_speech_text(packet) {
        Some(t) => t,
        None => return Ok(false),
    };
    let text = text.trim();
    if !text.starts_with('.') {
        return Ok(false);
    }

    let rest = &text[1..];
    let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));

    match cmd.to_ascii_lowercase().as_str() {
        "menu" | "commands" => {
            send_command_menu_gump(player, session).await?;
        }
        "where" => {
            common::dot_commands::send_system_message(
                session,
                &format!(
                    "Position: ({},{},{}) direction={} world={}",
                    player.x, player.y, player.z, player.direction, player.world
                ),
            )
            .await?;
        }
        "path" => {
            if let Some(pkts) = handle_dot_path(args, player, state).await {
                session.send_all(pkts).await?;
            }
        }
        "save" => {
            handle_save(player, args, session, worker_tx).await?;
        }
        "load" => {
            handle_load(player, args, session, worker_tx).await?;
        }
        "clear" => {
            handle_clear(player, session, worker_tx).await?;
        }
        "pathvis" => {
            // Clean up leftover path markers from previous run before starting
            // a new one, or when the user explicitly requests `.pathvis clear`.
            if !pathvis_serials.is_empty() {
                crate::pf::visual::cleanup_markers(pathvis_serials, worker_tx, player.world).await;
                pathvis_serials.clear();
            }
            handle_dot_pathvis(args, player, state, session, pathvis_marker_tx).await?;
        }
        "losvis" => {
            handle_dot_losvis(args, player, state, session).await?;
        }
        "tele" | "teleport" => {
            if args.is_empty() {
                // No args: send target cursor for single click-to-teleport.
                player.extra = Some(PendingTarget::Teleport);
                common::dot_commands::send_target_cursor(
                    PendingTarget::Teleport.cursor_id(),
                    1, // ground/tile target
                    session,
                ).await?;
            } else {
                handle_dot_tele(args, player, worker_tx, session).await?;
            }
        }
        "mtele" => {
            if args.is_empty() {
                // No args: send target cursor for multi click-to-teleport chain.
                player.extra = Some(PendingTarget::MultiTeleport);
                common::dot_commands::send_target_cursor(
                    PendingTarget::MultiTeleport.cursor_id(),
                    1, // ground/tile target
                    session,
                ).await?;
            } else {
                handle_dot_tele(args, player, worker_tx, session).await?;
            }
        }
        _ => return Ok(false),
    }

    Ok(true)
}

/// Extract text from Speech (0x03) or UnicodeSpeech (0xAD) packets.
fn extract_speech_text(packet: &RawPacket) -> Option<String> {
    packets::speech::extract_speech_text(packet)
}

/// Handle `.path X Y [Z]` and `.path clear` dot-commands.
async fn handle_dot_path(
    args: &str,
    player: &PlayerState,
    state: &Arc<AppState>,
) -> Option<Vec<RawPacket>> {
    if args == "clear" {
        return Some(vec![RawPacket::s2c(
            SendSpeech {
                serial: player.serial,
                model: 0,
                speech_type: SpeechType::System,
                color: 90,
                font: 3,
                name: "System".to_string(),
                message: "Path cleared.".to_string(),
            }
            .to_bytes(),
        )]);
    }

    // Parse "X Y [Z]"
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        return Some(vec![RawPacket::s2c(
            SendSpeech {
                serial: player.serial,
                model: 0,
                speech_type: SpeechType::System,
                color: 90,
                font: 3,
                name: "System".to_string(),
                message: "Usage: .path X Y [Z]".to_string(),
            }
            .to_bytes(),
        )]);
    }

    let dest_x: i64 = parts[0].parse().ok()?;
    let dest_y: i64 = parts[1].parse().ok()?;

    // Optional Z: explicit or resolved from the map.
    let dest_z: i8 = if let Some(zs) = parts.get(2) {
        zs.parse().ok()?
    } else {
        let engine = EngineProxy::<PathServerCommand>::new(
            state.worker_tx.clone(),
            player.world,
        );
        engine.resolve_z(dest_x as u16, dest_y as u16, 0, Heading::South)
            .await
            .unwrap_or(0)
    };

    use crate::pf::{TraceOptions, TraceRequest};
    use crate::pf::task::{run_pathfind, PathfindResult};

    let opts = TraceOptions::default();

    let (map_width, map_height) = state
        .static_data
        .0
        .as_deref()
        .and_then(|sd| EcumeneStaticDataProvider::map_tile_dimensions(sd, player.world))
        .map(|(w, h)| (w as isize, h as isize))
        .unwrap_or((6144, 4096));

    let request = TraceRequest {
        world: player.world,
        sx: player.x as isize,
        sy: player.y as isize,
        sz: player.z,
        sdir: player.direction,
        dx: dest_x as isize,
        dy: dest_y as isize,
        dz: dest_z,
        ddir: 0,
        options: opts,
    };

    // Run pathfinding via worker task (non-blocking, separate spawn_blocking).
    let result = run_pathfind(&state.worker_tx, request, map_width, map_height).await;

    let msg = match result {
        PathfindResult::Found(points) => {
            if points.is_empty() {
                format!("No path found to ({dest_x}, {dest_y}).")
            } else {
                format!("Path found: {} steps to ({dest_x}, {dest_y}).", points.len())
            }
        }
        PathfindResult::Cancelled => "Pathfinding cancelled.".to_string(),
        PathfindResult::WorkerGone => "Pathfinding failed: worker unavailable.".to_string(),
    };

    Some(vec![RawPacket::s2c(
        SendSpeech {
            serial: player.serial,
            model: 0,
            speech_type: SpeechType::System,
            color: 90,
            font: 3,
            name: "System".to_string(),
            message: msg,
        }
        .to_bytes(),
    )])
}

// ── .pathvis ──────────────────────────────────────────────────────────────

/// Handle `.pathvis [X Y [Z]] [delay=N]` — visual pathfinding with real-time markers.
///
/// Options:
///   `.pathvis`                  — open a target cursor; click a tile to pathfind
///   `.pathvis X Y`              — default delay, Z resolved from map
///   `.pathvis X Y Z`            — explicit destination Z
///   `.pathvis X Y delay=500`    — custom delay in microseconds
///   `.pathvis X Y Z delay=500`  — explicit Z + custom delay
///   `.pathvis clear`            — remove leftover path markers
///
/// The search runs in the background so the session event loop is not blocked.
/// Results are delivered as system messages via the world event broadcast.
async fn handle_dot_pathvis(
    args: &str,
    player: &mut PlayerState,
    state: &Arc<AppState>,
    session: &mut Session,
    pathvis_marker_tx: &tokio::sync::mpsc::Sender<Vec<u32>>,
) -> error::Result<()> {
    use crate::pf::visual::VisualConfig;
    use crate::pf::task::run_pathfind_visual;

    if args == "clear" {
        // Cleanup is handled by the caller after receiving serials via the channel.
        common::dot_commands::send_system_message(session, "Path markers cleared.").await?;
        return Ok(());
    }

    // No args — open a target cursor; the actual pathfinding starts when
    // the client responds with a tile click (handled in handle_target_response).
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        player.extra = Some(PendingTarget::PathVis);
        common::dot_commands::send_target_cursor(
            PendingTarget::PathVis.cursor_id(),
            1, // ground/tile only
            session,
        ).await?;
        common::dot_commands::send_system_message(
            session,
            "Click a tile to run visual pathfinding to it.",
        ).await?;
        return Ok(());
    }

    if parts.len() < 2 {
        common::dot_commands::send_system_message(
            session,
            "Usage: .pathvis [X Y [Z]] [delay=N]  (N in microseconds, default 200)",
        ).await?;
        return Ok(());
    }

    let dest_x: isize = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => {
            common::dot_commands::send_system_message(session, "Invalid X coordinate").await?;
            return Ok(());
        }
    };
    let dest_y: isize = match parts[1].parse() {
        Ok(v) => v,
        Err(_) => {
            common::dot_commands::send_system_message(session, "Invalid Y coordinate").await?;
            return Ok(());
        }
    };

    // Optional Z (third positional arg) and delay=N parameter.
    let mut explicit_z: Option<i8> = None;
    let mut config = VisualConfig::default();
    for &part in &parts[2..] {
        if let Some(val) = part.strip_prefix("delay=") {
            if let Ok(us) = val.parse::<u64>() {
                config.step_delay = std::time::Duration::from_micros(us);
            }
        } else if explicit_z.is_none() {
            // First non-keyword token after X Y is treated as Z.
            if let Ok(z) = part.parse::<i8>() {
                explicit_z = Some(z);
            }
        }
    }

    // Resolve destination Z: use explicit value, or ask the engine.
    let dest_z: i8 = match explicit_z {
        Some(z) => z,
        None => {
            let engine = EngineProxy::<PathServerCommand>::new(
                state.worker_tx.clone(),
                player.world,
            );
            engine.resolve_z(dest_x as u16, dest_y as u16, 0, Heading::South)
                .await
                .unwrap_or(0)
        }
    };

    let opts = crate::pf::TraceOptions::default();

    let (map_width, map_height) = state
        .static_data
        .0
        .as_deref()
        .and_then(|sd| EcumeneStaticDataProvider::map_tile_dimensions(sd, player.world))
        .map(|(w, h)| (w as isize, h as isize))
        .unwrap_or((6144, 4096));

    let request = crate::pf::TraceRequest {
        world: player.world,
        sx: player.x as isize,
        sy: player.y as isize,
        sz: player.z,
        sdir: player.direction,
        dx: dest_x,
        dy: dest_y,
        dz: dest_z,
        ddir: 0,
        options: opts,
    };

    common::dot_commands::send_system_message(
        session,
        &format!(
            "Visual pathfinding to ({}, {}, {}) started (delay={}us)...",
            dest_x, dest_y, dest_z, config.step_delay.as_micros()
        ),
    ).await?;

    // Fire-and-forget: spawn the visual search in the background so the
    // session event loop keeps processing packets and world events.
    // The result is delivered as a system speech message via broadcast.
    // Path marker serials are sent back to the session via pathvis_marker_tx.
    let worker_tx = state.worker_tx.clone();
    let event_tx = state.event_tx.clone();
    let world = player.world;
    let player_serial = player.serial;
    let marker_tx = pathvis_marker_tx.clone();

    tokio::spawn(async move {
        let result = run_pathfind_visual(
            &worker_tx,
            request,
            map_width,
            map_height,
            world,
            config,
        ).await;

        let msg = match &result.pathfind {
            crate::pf::task::PathfindResult::Found(points) => {
                if points.is_empty() {
                    format!(
                        "No path to ({dest_x}, {dest_y}). Vis: {} spawned, {} removed.",
                        result.stats.total_spawned, result.stats.total_removed,
                    )
                } else {
                    format!(
                        "Path found: {} steps to ({dest_x}, {dest_y}). \
                         Vis: {} frontier, {} visited, {} path markers. \
                         {} spawned, {} removed.",
                        points.len(),
                        result.stats.frontier_count,
                        result.stats.visited_count,
                        result.stats.path_count,
                        result.stats.total_spawned,
                        result.stats.total_removed,
                    )
                }
            }
            crate::pf::task::PathfindResult::Cancelled => {
                "Visual pathfinding cancelled.".to_string()
            }
            crate::pf::task::PathfindResult::WorkerGone => {
                "Worker unavailable.".to_string()
            }
        };

        // Send path marker serials back to the session for later cleanup.
        let _ = marker_tx.send(result.stats.path_serials).await;

        // Deliver result as a system speech message via broadcast.
        let _ = event_tx.send(WorldEvent::Speech {
            map_id: world,
            serial: 0xFFFF_FFFF,
            graphic: 0xFFFF,
            speech_type: 0x06, // System
            color: 90,
            font: 3,
            name: String::new(),
            message: msg,
            x: 0,
            y: 0,
        });

        log::info!(
            "[pathvis] done for {:#010X}: {} spawned, {} removed, {} path markers kept",
            player_serial,
            result.stats.total_spawned,
            result.stats.total_removed,
            result.stats.path_count,
        );
    });

    Ok(())
}

// ── .losvis ───────────────────────────────────────────────────────────────

/// Handle `.losvis` — LOS visualisation with real-time markers.
///
/// Options:
///   `.losvis`                          — target cursor, LOS from player pos
///   `.losvis x y z`                    — target cursor for second point, LOS from (x,y,z)
///   `.losvis x1 y1 z1 x2 y2 z2 [delay=N]` — immediate ray trace
///   `.losvis field [radius=N] [delay=N]`   — target cursor, LOS field
///   `.losvis clear`                    — remove all LOS markers
///
/// Internal (from target cursor resolution, not typed by user):
///   `.losvis field x y z radius=N [delay=N] [mobile=0|1] [self=0|1]`
async fn handle_dot_losvis(
    args: &str,
    player: &mut PlayerState,
    state: &Arc<AppState>,
    session: &mut Session,
) -> error::Result<()> {
    use crate::pf::los_visual::{LosVisualConfig, FieldStrategy, run_los_ray_blocking, run_los_field_blocking, cleanup_los_markers};
    use crate::pf::preloaded::LazyBlockProvider;

    let parts: Vec<&str> = args.split_whitespace().collect();

    // ── .losvis clear ─────────────────────────────────────────────────
    if parts.first() == Some(&"clear") {
        // Remove all LOS markers in the serial range.
        // We don't track individual serials globally — use RemoveEntitiesBatch
        // with the full range scan. For simplicity, just send a message.
        common::dot_commands::send_system_message(
            session,
            "Use .clear to reset the zone (removes all markers).",
        ).await?;
        return Ok(());
    }

    // ── .losvis field ... ─────────────────────────────────────────────
    if parts.first() == Some(&"field") {
        let field_parts = &parts[1..];

        // Check if this is a fully resolved call (from target cursor):
        //   field X Y Z radius=N [delay=N] [mobile=0|1]
        // Or a user-typed call:
        //   field [radius=N] [delay=N]
        let mut radius: u16 = 18;
        let mut delay_us: u64 = 0;
        let mut explicit_pos: Option<(u16, u16, i8)> = None;
        let mut is_mobile = true; // default: assume mobile for entity clicks
        let mut is_self = true;   // default: own perspective

        // Try to parse positional X Y Z from field_parts
        let mut positional_count = 0;
        let mut pos_x: u16 = 0;
        let mut pos_y: u16 = 0;
        let mut pos_z: i8 = 0;

        for &part in field_parts {
            if let Some(val) = part.strip_prefix("radius=") {
                if let Ok(r) = val.parse::<u16>() {
                    radius = r;
                }
            } else if let Some(val) = part.strip_prefix("delay=") {
                if let Ok(d) = val.parse::<u64>() {
                    delay_us = d;
                }
            } else if let Some(val) = part.strip_prefix("mobile=") {
                is_mobile = val == "1";
            } else if let Some(val) = part.strip_prefix("self=") {
                is_self = val == "1";
            } else {
                // Positional argument
                match positional_count {
                    0 => {
                        if let Ok(v) = part.parse::<u16>() { pos_x = v; positional_count += 1; }
                    }
                    1 => {
                        if let Ok(v) = part.parse::<u16>() { pos_y = v; positional_count += 1; }
                    }
                    2 => {
                        if let Ok(v) = part.parse::<i8>() { pos_z = v; positional_count += 1; }
                    }
                    _ => {}
                }
            }
        }

        if positional_count >= 3 {
            explicit_pos = Some((pos_x, pos_y, pos_z));
        }

        if let Some((fx, fy, fz)) = explicit_pos {
            // Fully resolved — run field directly.
            let mut config = LosVisualConfig::default();
            config.step_delay = Duration::from_micros(delay_us);

            common::dot_commands::send_system_message(
                session,
                &format!(
                    "LOS field at ({}, {}, {}) radius={} mobile={} started...",
                    fx, fy, fz, radius, is_mobile
                ),
            ).await?;

            let worker_tx = state.worker_tx.clone();
            let event_tx = state.event_tx.clone();
            let world = player.world;
            let linger = config.linger;

            let invert_hues = !is_self;

            tokio::spawn(async move {
                let handle = tokio::runtime::Handle::current();
                let wtx = worker_tx.clone();

                let result = tokio::task::spawn_blocking(move || {
                    let provider = LazyBlockProvider::new(world, handle.clone(), wtx.clone());
                    run_los_field_blocking(
                        &provider, fx, fy, fz, radius, is_mobile,
                        FieldStrategy::Perimeter,
                        invert_hues,
                        &config, &handle, &wtx, world,
                    )
                }).await;

                match result {
                    Ok(res) => {
                        let msg = format!(
                            "LOS field: {} visible, {} blocked / {} tiles.",
                            res.clear_count, res.blocked_count, res.total_tiles,
                        );
                        let _ = event_tx.send(WorldEvent::Speech {
                            map_id: world,
                            serial: 0xFFFF_FFFF,
                            graphic: 0xFFFF,
                            speech_type: 0x06,
                            color: 90,
                            font: 3,
                            name: String::new(),
                            message: msg,
                            x: 0,
                            y: 0,
                        });

                        // Linger then cleanup.
                        tokio::time::sleep(linger).await;
                        cleanup_los_markers(&res.marker_serials, &worker_tx, world).await;
                    }
                    Err(e) => {
                        log::error!("[losvis] field task failed: {e}");
                    }
                }
            });

            return Ok(());
        }

        // No explicit position — open target cursor (click entity or tile).
        let pending = PendingTarget::LosField { radius, delay_us };
        player.extra = Some(pending);
        common::dot_commands::send_target_cursor(
            pending.cursor_id(),
            0, // any target (entity or ground)
            session,
        ).await?;
        common::dot_commands::send_system_message(
            session,
            &format!(
                "Click an entity (or tile) to visualise its LOS field (radius={}).",
                radius
            ),
        ).await?;
        return Ok(());
    }

    // ── .losvis (no args) — target cursor, LOS from player ────────────
    if parts.is_empty() {
        let pending = PendingTarget::LosVis;
        player.extra = Some(pending);
        common::dot_commands::send_target_cursor(
            pending.cursor_id(),
            1, // ground/tile only
            session,
        ).await?;
        common::dot_commands::send_system_message(
            session,
            "Click a tile to trace LOS from your position.",
        ).await?;
        return Ok(());
    }

    // ── Parse positional args ─────────────────────────────────────────
    // Try to parse: x1 y1 z1 [x2 y2 z2] [delay=N]
    let mut coords: Vec<i64> = Vec::new();
    let mut delay_us: u64 = 0;

    for &part in &parts {
        if let Some(val) = part.strip_prefix("delay=") {
            if let Ok(d) = val.parse::<u64>() {
                delay_us = d;
            }
        } else if let Ok(v) = part.parse::<i64>() {
            coords.push(v);
        } else {
            common::dot_commands::send_system_message(
                session,
                "Usage: .losvis [x1 y1 z1 [x2 y2 z2]] [delay=N] | .losvis field [radius=N]",
            ).await?;
            return Ok(());
        }
    }

    match coords.len() {
        // .losvis x y z — from (x,y,z), open target cursor for second point
        3 => {
            let pending = PendingTarget::LosVisFrom {
                x: coords[0] as u16,
                y: coords[1] as u16,
                z: coords[2] as i8,
            };
            player.extra = Some(pending);
            common::dot_commands::send_target_cursor(
                pending.cursor_id(),
                1, // ground
                session,
            ).await?;
            common::dot_commands::send_system_message(
                session,
                &format!("Click a tile to trace LOS from ({}, {}, {}).", coords[0], coords[1], coords[2]),
            ).await?;
            return Ok(());
        }
        // .losvis x1 y1 z1 x2 y2 z2 — immediate ray trace
        6 => {
            let x1 = coords[0] as u16;
            let y1 = coords[1] as u16;
            let z1 = coords[2] as i8;
            let x2 = coords[3] as u16;
            let y2 = coords[4] as u16;
            let z2 = coords[5] as i8;

            let mut config = LosVisualConfig::default();
            config.step_delay = Duration::from_micros(delay_us);

            common::dot_commands::send_system_message(
                session,
                &format!(
                    "LOS trace ({},{},{}) → ({},{},{}) started...",
                    x1, y1, z1, x2, y2, z2,
                ),
            ).await?;

            let worker_tx = state.worker_tx.clone();
            let event_tx = state.event_tx.clone();
            let world = player.world;
            let linger = config.linger;

            // Eye-height offset for both endpoints (assume humanoid mobiles).
            let z1_los = z1 as i16 + crate::pf::los_visual::EYE_HEIGHT;
            let z2_los = z2 as i16 + crate::pf::los_visual::EYE_HEIGHT;

            tokio::spawn(async move {
                let handle = tokio::runtime::Handle::current();
                let wtx = worker_tx.clone();

                let result = tokio::task::spawn_blocking(move || {
                    let provider = LazyBlockProvider::new(world, handle.clone(), wtx.clone());
                    run_los_ray_blocking(
                        &provider,
                        x1, y1, z1_los, x2, y2, z2_los,
                        z1, // z_hint = source standing Z for marker placement
                        &config, &handle, &wtx, world,
                    )
                }).await;

                match result {
                    Ok(res) => {
                        let status = if res.has_los { "CLEAR" } else { "BLOCKED" };
                        let msg = format!(
                            "LOS {status}: {} tiles ({} clear, {} blocked).",
                            res.total_tiles, res.clear_count, res.blocked_count,
                        );
                        let _ = event_tx.send(WorldEvent::Speech {
                            map_id: world,
                            serial: 0xFFFF_FFFF,
                            graphic: 0xFFFF,
                            speech_type: 0x06,
                            color: 90,
                            font: 3,
                            name: String::new(),
                            message: msg,
                            x: 0,
                            y: 0,
                        });

                        // Linger then cleanup.
                        tokio::time::sleep(linger).await;
                        cleanup_los_markers(&res.marker_serials, &worker_tx, world).await;
                    }
                    Err(e) => {
                        log::error!("[losvis] ray task failed: {e}");
                    }
                }
            });

            return Ok(());
        }
        _ => {
            common::dot_commands::send_system_message(
                session,
                "Usage: .losvis [x1 y1 z1 [x2 y2 z2]] [delay=N] | .losvis field [radius=N]",
            ).await?;
        }
    }

    Ok(())
}

// ── .tele / .mtele ────────────────────────────────────────────────────────

/// Handle `.tele X Y [Z]` / `.mtele X Y [Z]` — instant teleport.
///
/// If Z is omitted, it is resolved via the engine (standing Z at the target).
async fn handle_dot_tele(
    args: &str,
    player: &mut PlayerState,
    worker_tx: &PathServerWorkerTx,
    session: &mut Session,
) -> error::Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        common::dot_commands::send_system_message(
            session,
            "Usage: .tele X Y [Z]",
        ).await?;
        return Ok(());
    }

    let x: u16 = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => {
            common::dot_commands::send_system_message(session, "Invalid X").await?;
            return Ok(());
        }
    };
    let y: u16 = match parts[1].parse() {
        Ok(v) => v,
        Err(_) => {
            common::dot_commands::send_system_message(session, "Invalid Y").await?;
            return Ok(());
        }
    };

    // Z: explicit or resolved from the map.
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let z: i8 = if let Some(zs) = parts.get(2) {
        match zs.parse() {
            Ok(v) => v,
            Err(_) => {
                common::dot_commands::send_system_message(session, "Invalid Z").await?;
                return Ok(());
            }
        }
    } else {
        engine.resolve_z(x, y, 0, Heading::South)
            .await
            .unwrap_or(0)
    };

    let old_x = player.x;
    let old_y = player.y;
    let old_z = player.z;

    // Teleport the entity in the engine (no passability check).
    engine.teleport(player.serial, x, y, z, None).await;

    // Update local player state.
    player.x = x;
    player.y = y;
    player.z = z;

    // Tell the client where we are now.
    if let Some(DemoEntity::Mobile(m)) =
        engine.get_entity(player.serial).await
    {
        session
            .send(common::spawn::build_draw_game_player(
                player.serial, m.graphic, m.color, player.x, player.y, player.z, player.direction,
            ))
            .await?;
    }

    info!(
        "[cmd] .tele ({},{},{}) → ({},{},{})",
        old_x, old_y, old_z, x, y, z
    );
    common::dot_commands::send_system_message(
        session,
        &format!("Teleported to ({}, {}, {})", x, y, z),
    ).await?;

    Ok(())
}

// ── Command menu gump ─────────────────────────────────────────────────────

/// Send the `.menu` / `.commands` command menu gump to the client.
///
/// Mirrors replay-proxy's action-menu gump: one button + label per command.
/// Button IDs map to commands in [`handle_gump_response`].
///
/// Layout (resizable background, rows 30px apart):
/// - 1 Where · 2 Teleport · 3 Multi-teleport · 4 Path-vis
/// - 5 Los-vis · 6 Los-field · 7 Save · 8 Load · 9 Clear · 0 Close
async fn send_command_menu_gump(
    player: &PlayerState,
    session: &mut Session,
) -> error::Result<()> {
    use packets::gump::GumpTextLine;

    // resizepic height = 35 (title) + 9*30 + 10 padding ≈ 320.
    let layout = "{ page 0 }{ nodispose }\
         { resizepic 0 0 2620 230 330 }\
         { text 20 12 2100 0 }\
         { button 20  45 2117 2118 1 0 1 }{ text 55  45 2100 1 }\
         { button 20  75 2117 2118 1 0 2 }{ text 55  75 2100 2 }\
         { button 20 105 2117 2118 1 0 3 }{ text 55 105 2100 3 }\
         { button 20 135 2117 2118 1 0 4 }{ text 55 135 2100 4 }\
         { button 20 165 2117 2118 1 0 5 }{ text 55 165 2100 5 }\
         { button 20 195 2117 2118 1 0 6 }{ text 55 195 2100 6 }\
         { button 20 225 2117 2118 1 0 7 }{ text 55 225 2100 7 }\
         { button 20 255 2117 2118 1 0 8 }{ text 55 255 2100 8 }\
         { button 20 285 2117 2118 1 0 9 }{ text 55 285 2100 9 }";

    common::dot_commands::send_gump(
        MENU_GUMP_ID,
        player.serial,
        layout,
        &[
            GumpTextLine("Path Server — Commands".to_string()),
            GumpTextLine("Where am I".to_string()),
            GumpTextLine("Teleport (click tile)".to_string()),
            GumpTextLine("Multi-teleport (click tiles)".to_string()),
            GumpTextLine("Path-vis (click tile)".to_string()),
            GumpTextLine("Los-vis (click tile)".to_string()),
            GumpTextLine("Los-field (click entity/tile)".to_string()),
            GumpTextLine("Save world".to_string()),
            GumpTextLine("Load world".to_string()),
            GumpTextLine("Clear zone".to_string()),
        ],
        session,
    )
    .await
}

/// Handle an incoming GumpMenuSelection (0xB1) response from the client.
///
/// Returns `true` if the packet was consumed (it was our command menu),
/// `false` if it should be passed through.
///
/// Immediate commands (Where/Save/Load/Clear) run inline and re-show the
/// menu; click-to-target commands (Teleport/Multi-teleport/Path-vis/Los-vis/
/// Los-field) arm a `PendingTarget` cursor and the menu is re-shown once the
/// target completes (or is cancelled) in `handle_target_response`.
async fn handle_gump_response(
    packet: &RawPacket,
    player: &mut PlayerState,
    worker_tx: &PathServerWorkerTx,
    session: &mut Session,
) -> error::Result<bool> {
    let resp = match GumpMenuSelection::from_bytes(&packet.data) {
        Ok(r) => r,
        Err(_) => return Ok(false),
    };

    if resp.gump_id != MENU_GUMP_ID {
        return Ok(false); // Not our gump — ignore.
    }

    match resp.button_id {
        // Close button — nothing to do.
        0 => {}

        // ── Immediate commands ────────────────────────────────────────
        // These complete synchronously, so re-show the menu afterwards.
        1 => {
            common::dot_commands::send_system_message(
                session,
                &format!(
                    "Position: ({},{},{}) direction={} world={}",
                    player.x, player.y, player.z, player.direction, player.world
                ),
            )
            .await?;
            send_command_menu_gump(player, session).await?;
        }
        7 => {
            handle_save(player, "", session, worker_tx).await?;
            send_command_menu_gump(player, session).await?;
        }
        8 => {
            handle_load(player, "", session, worker_tx).await?;
            send_command_menu_gump(player, session).await?;
        }
        9 => {
            handle_clear(player, session, worker_tx).await?;
            send_command_menu_gump(player, session).await?;
        }

        // ── Click-to-target commands ──────────────────────────────────
        // These arm a target cursor; the menu is re-shown once the target
        // completes (or is cancelled) in `handle_target_response`.
        2 => {
            player.extra = Some(PendingTarget::Teleport);
            common::dot_commands::send_target_cursor(
                PendingTarget::Teleport.cursor_id(),
                1, // ground/tile target
                session,
            )
            .await?;
        }
        3 => {
            player.extra = Some(PendingTarget::MultiTeleport);
            common::dot_commands::send_target_cursor(
                PendingTarget::MultiTeleport.cursor_id(),
                1,
                session,
            )
            .await?;
        }
        4 => {
            player.extra = Some(PendingTarget::PathVis);
            common::dot_commands::send_target_cursor(
                PendingTarget::PathVis.cursor_id(),
                1,
                session,
            )
            .await?;
            common::dot_commands::send_system_message(
                session,
                "Click a tile to run visual pathfinding to it.",
            )
            .await?;
        }
        5 => {
            player.extra = Some(PendingTarget::LosVis);
            common::dot_commands::send_target_cursor(
                PendingTarget::LosVis.cursor_id(),
                1,
                session,
            )
            .await?;
            common::dot_commands::send_system_message(
                session,
                "Click a tile to trace LOS from your position.",
            )
            .await?;
        }
        6 => {
            // Los-field — default radius 18, no extra delay.
            let pending = PendingTarget::LosField { radius: 18, delay_us: 0 };
            player.extra = Some(pending);
            common::dot_commands::send_target_cursor(
                pending.cursor_id(),
                0, // any target (entity or ground)
                session,
            )
            .await?;
            common::dot_commands::send_system_message(
                session,
                "Click an entity (or tile) to visualise its LOS field (radius=18).",
            )
            .await?;
        }

        other => {
            log::debug!("[cmd] command menu: unknown button_id={}", other);
        }
    }

    Ok(true)
}

// ── TargetCursor response ─────────────────────────────────────────────────

/// Handle an incoming TargetCursor (0x6C) response from the client.
///
/// Returns `true` if the packet was consumed (cursor_id matched a pending
/// target), `false` if it should be passed through.
async fn handle_target_response(
    packet: &RawPacket,
    player: &mut PlayerState,
    worker_tx: &PathServerWorkerTx,
    state: &Arc<AppState>,
    session: &mut Session,
    pathvis_serials: &mut Vec<u32>,
    pathvis_marker_tx: &tokio::sync::mpsc::Sender<Vec<u32>>,
) -> error::Result<bool> {
    use packets::interaction::TargetCursor;

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };

    let pending = match player.extra {
        Some(pt) if tc.cursor_id == pt.cursor_id() => pt,
        _ => return Ok(false), // Not our cursor — ignore.
    };

    // Cancel: client dismissed the cursor.
    if common::dot_commands::is_target_cancelled(&tc) {
        player.extra = None;
        common::dot_commands::send_system_message(session, "Target cancelled.").await?;
        // Re-show the command menu so the player can pick another action.
        send_command_menu_gump(player, session).await?;
        return Ok(true);
    }

    // Resolve Z if the client gave us the ground click (z may be inaccurate).
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let z = engine.resolve_z(tc.x, tc.y, tc.z, Heading::South)
        .await
        .unwrap_or(tc.z);

    // ── PathVis: run visual pathfinding to the clicked tile ───────────
    if pending == PendingTarget::PathVis {
        player.extra = None;

        // Clean up leftover path markers from previous run.
        if !pathvis_serials.is_empty() {
            crate::pf::visual::cleanup_markers(pathvis_serials, worker_tx, player.world).await;
            pathvis_serials.clear();
        }

        let dest_x = tc.x as isize;
        let dest_y = tc.y as isize;

        // Pass the already-resolved Z so handle_dot_pathvis doesn't re-query.
        let args_str = format!("{} {} {}", dest_x, dest_y, z);
        handle_dot_pathvis(&args_str, player, state, session, pathvis_marker_tx).await?;
        // Re-show the command menu so the player can pick another action.
        send_command_menu_gump(player, session).await?;
        return Ok(true);
    }

    // ── LosVis: trace LOS ray from player position to clicked tile ────
    if pending == PendingTarget::LosVis {
        player.extra = None;
        let args_str = format!(
            "{} {} {} {} {} {}",
            player.x, player.y, player.z, tc.x, tc.y, z
        );
        handle_dot_losvis(&args_str, player, state, session).await?;
        send_command_menu_gump(player, session).await?;
        return Ok(true);
    }

    // ── LosVisFrom: trace LOS ray from explicit point to clicked tile ─
    if let PendingTarget::LosVisFrom { x: fx, y: fy, z: fz } = pending {
        player.extra = None;
        let args_str = format!(
            "{} {} {} {} {} {}",
            fx, fy, fz, tc.x, tc.y, z
        );
        handle_dot_losvis(&args_str, player, state, session).await?;
        send_command_menu_gump(player, session).await?;
        return Ok(true);
    }

    // ── LosField: visualise LOS field for clicked entity/tile ─────────
    if let PendingTarget::LosField { radius, delay_us } = pending {
        player.extra = None;

        // Try to resolve entity by serial from the target cursor response.
        let (field_x, field_y, field_z, is_mobile, is_self) =
            if tc.target_serial != 0 && tc.target_serial != 0xFFFFFFFF {
                let is_self = tc.target_serial == player.serial;
                // Clicked on an entity — look up its position.
                match engine.get_entity(tc.target_serial).await {
                    Some(DemoEntity::Mobile(m)) => (m.x, m.y, m.z, true, is_self),
                    Some(entity) => {
                        let pos = framework::ecumene::Entity::pos(&entity);
                        (pos.x, pos.y, pos.z, false, is_self)
                    }
                    None => {
                        common::dot_commands::send_system_message(
                            session,
                            &format!("Entity 0x{:08X} not found.", tc.target_serial),
                        ).await?;
                        send_command_menu_gump(player, session).await?;
                        return Ok(true);
                    }
                }
            } else {
                // Clicked on ground — treat as own perspective.
                (tc.x, tc.y, z, false, true)
            };

        let delay_part = if delay_us > 0 {
            format!(" delay={}", delay_us)
        } else {
            String::new()
        };
        let args_str = format!(
            "field {} {} {} radius={}{} mobile={} self={}",
            field_x, field_y, field_z, radius, delay_part,
            if is_mobile { "1" } else { "0" },
            if is_self { "1" } else { "0" }
        );
        handle_dot_losvis(&args_str, player, state, session).await?;
        send_command_menu_gump(player, session).await?;
        return Ok(true);
    }

    // ── Teleport targets ─────────────────────────────────────────────
    let old_x = player.x;
    let old_y = player.y;
    let old_z = player.z;

    // Teleport via engine.
    engine.teleport(player.serial, tc.x, tc.y, z, None).await;

    player.x = tc.x;
    player.y = tc.y;
    player.z = z;

    // Update client position.
    if let Some(DemoEntity::Mobile(m)) =
        engine.get_entity(player.serial).await
    {
        session
            .send(common::spawn::build_draw_game_player(
                player.serial, m.graphic, m.color, player.x, player.y, player.z, player.direction,
            ))
            .await?;
    }

    info!(
        "[cmd] target tele ({},{},{}) → ({},{},{})",
        old_x, old_y, old_z, player.x, player.y, player.z
    );
    common::dot_commands::send_system_message(
        session,
        &format!("Teleported to ({}, {}, {})", player.x, player.y, player.z),
    ).await?;

    // Chain: for MultiTeleport, send the cursor again immediately.
    match pending {
        PendingTarget::MultiTeleport => {
            common::dot_commands::send_target_cursor(
                pending.cursor_id(),
                1, // ground/tile target
                session,
            ).await?;
            // extra stays set — chain continues.
        }
        PendingTarget::Teleport => {
            player.extra = None;
            // Re-show the command menu so the player can pick another action.
            send_command_menu_gump(player, session).await?;
        }
        PendingTarget::PathVis
        | PendingTarget::LosVis
        | PendingTarget::LosVisFrom { .. }
        | PendingTarget::LosField { .. } => unreachable!(), // handled above
    }

    Ok(true)
}

// ── .save ─────────────────────────────────────────────────────────────────

async fn handle_save(
    player: &PlayerState,
    args: &str,
    session: &mut Session,
    worker_tx: &PathServerWorkerTx,
) -> error::Result<()> {
    let path = if args.is_empty() { "world_save.json" } else { args };
    log::info!("[cmd] .save — saving zone {} to {}", player.world, path);

    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    match engine.save_snapshot().await {
        Some(zone_data) => {
            common::dot_commands::save_snapshot_to_file(
                zone_data, player.serial, player.world, path, session,
            ).await?;
        }
        None => {
            common::dot_commands::send_system_message(
                session,
                "Save failed: worker unavailable",
            ).await?;
        }
    }
    Ok(())
}

// ── .load ─────────────────────────────────────────────────────────────────

async fn handle_load(
    player: &PlayerState,
    args: &str,
    session: &mut Session,
    worker_tx: &PathServerWorkerTx,
) -> error::Result<()> {
    let path = if args.is_empty() { "world_save.json" } else { args };
    log::info!("[cmd] .load — loading zone {} from {}", player.world, path);

    let loaded = common::dot_commands::load_snapshot_from_file(path, player.world, session).await?;
    let Some((data, entity_count, container_count)) = loaded else {
        return Ok(());
    };

    // Collect old visible serials for deletion.
    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let old_visible = engine.query_area(player.view_rect).await;
    let old_serials: Vec<u32> = old_visible
        .iter()
        .map(|e| framework::ecumene::Entity::serial(e))
        .collect();

    // Save player entity before wipe.
    let saved_player = engine.get_entity(player.serial).await;

    engine.restore_snapshot(data).await;

    // Re-spawn player so they remain in the zone.
    if let Some(entity) = saved_player {
        engine.spawn_entity(player.serial, entity).await;
    }

    super::world_events::sync_zone_change(player, &old_serials, session, worker_tx).await?;

    common::dot_commands::send_system_message(
        session,
        &format!(
            "Loaded {} entities, {} containers from {}",
            entity_count, container_count, path
        ),
    ).await?;
    log::info!("[cmd] .load — done ({} entities)", entity_count);

    Ok(())
}

// ── .clear ────────────────────────────────────────────────────────────────

async fn handle_clear(
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &PathServerWorkerTx,
) -> error::Result<()> {
    log::info!("[cmd] .clear — clearing zone {}", player.world);

    let engine = EngineProxy::<PathServerCommand>::new(worker_tx.clone(), player.world);
    let old_visible = engine.query_area(player.view_rect).await;
    let old_serials: Vec<u32> = old_visible
        .iter()
        .map(|e| framework::ecumene::Entity::serial(e))
        .collect();

    let saved_player = engine.get_entity(player.serial).await;

    engine.reset_zone(
        Vec::new(),
        framework::continuum::HashContainerStore::new(),
    ).await;

    if let Some(entity) = saved_player {
        engine.spawn_entity(player.serial, entity).await;
    }

    super::world_events::sync_zone_change(player, &old_serials, session, worker_tx).await?;
    common::dot_commands::send_system_message(session, "Zone cleared").await?;
    log::info!("[cmd] .clear — done");
    Ok(())
}

// ── Session cleanup ───────────────────────────────────────────────────────

async fn cleanup_session(player: &Option<PlayerState>, worker_tx: &PathServerWorkerTx) {
    if let Some(p) = player {
        let _ = worker_tx
            .send(WorkerCommand::MapCommand(
                p.world,
                PathServerCommand::UnregisterObserver(
                    p.serial,
                ),
            ))
            .await;
    }
}

// ── Engine RPC helpers ────────────────────────────────────────────────────
//
// All engine RPC calls use `EngineProxy<PathServerCommand>` which wraps the
// worker channel and map id.  `PathServerCommand` implements `WrapEngineCommand`.
