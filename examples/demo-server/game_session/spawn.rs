//! Character spawn and login: test-account support, 0x91 GameLogin, 0x5D LoginCharacter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{info, trace, warn};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use rand::Rng;

use u_core::Heading;

use packets::system::{
    ClientVersionRequest, ClientViewRange, EnableFeatures,
};

use framework::continuum::{WorkerCommand, WorldEvent};
use framework::diorama::ObserverPipeline;
use framework::ecumene::TileRect;

use common::uo_engine::auth::{AccessLevel, AuthKey, SimpleSessionManager};
use common::uo_engine::handler::EngineCommand;
use common::uo_engine::serial_alloc::SerialAllocator;
pub(crate) use common::spawn::{
    TestAccountInfo, parse_test_account, is_playable_account,
};
use common::spawn::{self as spawn_builders, CharacterRecord};

use crate::{DemoCommand, DemoWorkerTx, WorldData};

use super::world_events::handle_world_event;
use super::{dev_items, PlayerState};

// Test account types re-exported from common::spawn:
// - TestAccountInfo
// - parse_test_account()

/// Flags sent in the CharacterList (0xA9) packet.
///
/// `0x400` enables UO3D-style packets.  Crucially the "one character per
/// account" bit (`0x04`) is **not** set, so the client shows the
/// "New Character" button for empty slots.
const CHAR_LIST_FLAGS: u32 = 0x0000_0400;

// ── 0x91 GameLogin ────────────────────────────────────────────────────────

/// Handle game-phase login (0x91).
///
/// Returns the response packets and the [`AccessLevel`] resolved from the
/// session manager (via the auth_key embedded in the packet).  If the
/// auth_key cannot be validated, defaults to [`AccessLevel::Player`].
pub(super) async fn handle_game_login(
    packet: &RawPacket,
    test_account: &mut Option<TestAccountInfo>,
    account_name_out: &mut Option<String>,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    session_manager: &Arc<SimpleSessionManager>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
) -> Option<(Vec<RawPacket>, AccessLevel)> {
    use protocol::packets::login::*;
    use framework::moira::SessionManager;

    let (account_name, auth_key) = if let Ok(login) = GameLogin::from_bytes(&packet.data) {
        trace!(
            "[{addr}] game login: '{}' (auth=0x{:08X})",
            &*login.account, login.auth_key
        );
        (login.account.to_string(), login.auth_key)
    } else {
        return None;
    };

    // Resolve account from the session manager to obtain the real AccessLevel.
    let resolved_level = match session_manager.validate_session(&AuthKey(auth_key)) {
        Ok(account) => {
            info!(
                "[{addr}] session validated: '{}' (level={})",
                account.username, account.access_level,
            );
            account.access_level
        }
        Err(e) => {
            info!(
                "[{addr}] session validation failed for auth=0x{:08X}: {:?}, \
                 defaulting to Player",
                auth_key, e,
            );
            AccessLevel::Player
        }
    };

    // Check if this is a test account (pattern: test\d+).
    *test_account = parse_test_account(&account_name);
    *account_name_out = Some(account_name.clone());

    let features = EnableFeatures::new(0x0002, client_version);
    let mut packets = vec![RawPacket::s2c(features.to_bytes())];

    if let Some(ta) = test_account {
        // Test account — single character slot with the test name.
        // Serial is allocated lazily on first spawn; unknown at login time.
        trace!("[{addr}] test account '{}'", ta.name);
        let mut slots = vec![CharacterSlot::new(&ta.name)];
        while slots.len() < 5 {
            slots.push(CharacterSlot::new(""));
        }
        let char_list = CharacterList::new(
            slots,
            vec![StartingLocation {
                index: 0,
                city_name: "World".into(),
                area_name: "Test spawn".into(),
            }],
            1024,
        );
        packets.push(RawPacket::s2c(encode_packet(&char_list)));
    } else if is_playable_account(&account_name) {
        // Reserved playable-pool account — always list the shared playable
        // serials loaded from the log (incl. EXTRA_PLAYABLE_SERIALS),
        // bypassing per-account character storage.  This is a read-only pool;
        // it cannot create characters (see `handle_create_character`).
        let engine = crate::game_util::engine_for(worker_tx, world_data.player_world);
        let mut slots = Vec::new();
        for &serial in &world_data.playable_serials {
            let name = match engine.get_entity(serial).await.as_ref().and_then(|e| e.mobile()) {
                Some(m) if !m.name.is_empty() => m.name.clone(),
                _ => format!("Entity {:#010X}", serial),
            };
            slots.push(CharacterSlot::new(&name));
        }
        while slots.len() < 5 {
            slots.push(CharacterSlot::new(""));
        }
        let char_list = CharacterList::new(
            slots,
            vec![StartingLocation {
                index: 0,
                city_name: "Britain".into(),
                area_name: "Sweet Dreams Inn".into(),
            }],
            CHAR_LIST_FLAGS,
        );
        packets.push(RawPacket::s2c(encode_packet(&char_list)));
    } else {
        // Normal account — list only the characters this account created via
        // the client (stored per-account).  Empty slots let the client show
        // the "New Character" button.  The shared playable pool is reserved
        // for the `replay` account only.
        let created = {
            let map = world_data.account_characters.read().await;
            map.get(&account_name).cloned().unwrap_or_default()
        };

        let mut slots = Vec::new();
        // The name is taken from the stored record (the live entity may be in
        // any world), so this is correct regardless of the character's world.
        for rec in &created {
            slots.push(CharacterSlot::new(&rec.name));
        }
        // Pad to 5 slots (UO client expects exactly 5).
        while slots.len() < 5 {
            slots.push(CharacterSlot::new(""));
        }

        let char_list = CharacterList::new(
            slots,
            vec![StartingLocation {
                index: 0,
                city_name: "Britain".into(),
                area_name: "Sweet Dreams Inn".into(),
            }],
            CHAR_LIST_FLAGS,
        );
        packets.push(RawPacket::s2c(encode_packet(&char_list)));
    }

    Some((packets, resolved_level))
}

// ── 0x5D Spawn ────────────────────────────────────────────────────────────

pub(super) async fn handle_spawn(
    packet: &RawPacket,
    player: &mut Option<PlayerState>,
    test_account: &Option<TestAccountInfo>,
    account_name: &Option<String>,
    access_level: common::uo_engine::auth::AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    serial_alloc: &Arc<SerialAllocator>,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
) -> error::Result<()> {
    use packets::login::LoginCharacter;

    let wd = world_data;
    // Default world for fresh/test spawns; for normal accounts the selected
    // character's stored world overrides this below (see
    // `resolve_normal_account_spawn`), so a character that logged out in a
    // non-zero world re-enters that same world.
    let default_world = wd.player_world;

    // Parse slot from the LoginCharacter packet.
    let slot = LoginCharacter::from_bytes(&packet.data)
        .map(|lc| lc.slot as usize)
        .unwrap_or(0);

    // ── Determine player serial, world, and entity data ───────────────
    let (world, player_serial, x, y, z, direction, graphic, color, player_name, is_fresh_spawn) =
        if let Some(ta) = test_account {
            let (s, x, y, z, d, g, c, n, fresh) =
                resolve_test_account_spawn(ta, worker_tx, wd, serial_alloc, default_world, addr).await;
            (default_world, s, x, y, z, d, g, c, n, fresh)
        } else {
            let (w, s, x, y, z, d, g, c, n) =
                resolve_normal_account_spawn(slot, account_name.as_deref(), worker_tx, wd, default_world, addr).await;
            (w, s, x, y, z, d, g, c, n, false)
        };

    info!(
        "[{addr}] '{}' ({:#010X}) entering world at ({},{},{}) world={}",
        player_name, player_serial, x, y, z, world
    );

    enter_world(
        player, player_serial, x, y, z, direction, graphic, color, &player_name,
        is_fresh_spawn, world, access_level, session, worker_tx, world_data,
        serial_alloc, observer, event_rx, event_tx_for_observer, client_version,
    ).await
}

/// Shared "enter the world" sequence used by both [`handle_spawn`] (0x5D
/// character selection) and [`handle_create_character`] (0x00 creation).
///
/// Initializes [`PlayerState`], sends the structural spawn packets,
/// registers the observer, streams initial entities, sends LoginComplete
/// and the post-login packets (features, status bar, skills, welcome), and
/// hands out starter items.
#[allow(clippy::too_many_arguments)]
async fn enter_world(
    player: &mut Option<PlayerState>,
    player_serial: u32,
    x: u16,
    y: u16,
    z: i8,
    direction: u8,
    graphic: u16,
    color: u16,
    player_name: &str,
    is_fresh_spawn: bool,
    world: u8,
    access_level: common::uo_engine::auth::AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    serial_alloc: &Arc<SerialAllocator>,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    client_version: u_core::ProtocolVersion,
) -> error::Result<()> {
    let wd = world_data;

    // Initialize player state.
    let view_range = ClientViewRange::DEFAULT as u16;
    let view_rect = TileRect::from_view(x, y, view_range);
    let throttle_interval = Duration::from_millis(wd.move_throttle_ms);
    *player = Some(PlayerState {
        serial: player_serial,
        world,
        x,
        y,
        z,
        direction,
        view_rect,
        view_range,
        move_throttle: HashMap::new(),
        throttle_interval,
        notoriety_ctx: Some(framework::continuum::NotorietyContext {
            class: common::uo_engine::notoriety::NotorietyClass::Innocent.to_u8(),
            guild_id: None,
            is_player: true,
            aggressors: Vec::new(),
        }),
        client_version,
        extra: (),
    });

    // ── Build spawn packet sequence ───────────────────────────────────
    //
    // From bootstrap, take only structural packets (0x1B, 0xB9, 0xBF,
    // 0xC8, 0x20).  Entity packets (0x78, 0x1A, 0xF3) are skipped —
    // fresh entities are queried from the engine below.
    // LoginComplete (0x55) is deferred until after entity streaming.
    let mut spawn_packets: Vec<RawPacket> = Vec::new();

    for pkt in &wd.bootstrap_packets {
        let pkt_id = pkt.data.first().copied().unwrap_or(0);
        match pkt_id {
            // Replace 0x1B with current position.
            0x1B => {
                spawn_packets.push(spawn_builders::build_character_locale_and_body(
                    player_serial, graphic, x, y, z, direction, 0x1800, 0x1000,
                ));
            }
            // Replace 0x20 with current position.
            0x20 => {
                spawn_packets.push(spawn_builders::build_draw_game_player(
                    player_serial, graphic, color, x, y, z, direction,
                ));
            }
            // Normalize 0xB9 EnableFeatures to the connecting client's version.
            // The bootstrap cached the raw bytes from the recorded log, which may
            // be 3 bytes (legacy) regardless of the actual connecting client.
            // Re-parse to extract the flags, then re-emit in the correct format.
            0xB9 => {
                let flags = EnableFeatures::from_bytes(&pkt.data)
                    .map(|f| f.flags())
                    .unwrap_or(0x0002);
                spawn_packets.push(RawPacket::s2c(
                    EnableFeatures::new(flags, client_version).to_bytes(),
                ));
            }
            // Skip entity packets — replaced with fresh query below.
            0x78 | 0x1A | 0xF3 => continue,
            // Defer LoginComplete — sent after entity streaming.
            0x55 => continue,
            // All other structural packets pass through.
            _ => {
                spawn_packets.push(pkt.clone());
            }
        }
    }

    // ── Send structural packets ────────────────────────────────────────
    //
    // These must arrive before entity packets and LoginComplete.
    for pkt in &spawn_packets {
        if let Some(obs) = &mut *observer {
            obs.ingest_s2c(&pkt.data);
        }
        session.send(pkt.clone()).await?;
    }

    // ── Register observer & stream initial entities ───────────────────
    //
    // Register the observer with the worker.  The worker will send
    // EntitySpawned events for all entities in the view rectangle into
    // the session's mpsc channel, then signal completion via the oneshot.
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            world,
            DemoCommand::RegisterObserver(
                player_serial,
                world,
                view_rect,
                event_tx_for_observer.clone(),
                reply_tx,
            ),
        ))
        .await;

    // Wait for the worker to finish streaming initial entities.
    let _ = reply_rx.await;

    // Drain all EntitySpawned events that the worker just pushed into
    // the mpsc channel and send them to the client as entity packets.
    {
        let p = player.as_mut().unwrap();
        while let Ok(event) = event_rx.try_recv() {
            handle_world_event(session, p, &event, access_level, observer).await?;
        }
    }

    // LoginComplete — must come after all entity packets.
    {
        use packets::system::LoginComplete;
        let pkt = RawPacket::s2c(encode_packet(&LoginComplete::new()));
        if let Some(obs) = &mut *observer {
            obs.ingest_s2c(&pkt.data);
        }
        session.send(pkt).await?;
    }

    // Additional game-state packets after bootstrap.
    let mut post_packets = spawn_builders::build_post_login_defaults(0x00, 0x00);
    post_packets.push(RawPacket::s2c(EnableFeatures::new(0x0002, client_version).to_bytes()));
    post_packets.push(RawPacket::s2c(encode_packet(
        &ClientVersionRequest::new(),
    )));

    // ── Send full StatusBarInfo with stats ─────────────────────────────
    {
        let engine = crate::game_util::engine_for(worker_tx, world);
        let entity = engine.get_entity(player_serial).await;
        if let Some(ref e) = entity {
            if let Some(sbi_pkt) = spawn_builders::build_status_bar(e, true) {
                post_packets.push(sbi_pkt);
            }
        }
    }

    // ── Send skill list ───────────────────────────────────────────────
    {
        use packets::skills::SendSkills;
        let engine = crate::game_util::engine_for(worker_tx, world);
        let entity = engine.get_entity(player_serial).await;
        let send = match entity.as_ref().and_then(|e| e.mobile()) {
            Some(m) => {
                // Apply bonuses from any already-equipped "plus" weapons.
                let bonuses = engine.query_skill_bonuses(player_serial).await;
                crate::skills::build_full_list_with_bonuses(&m.skills, &bonuses)
            }
            // Fallback: an empty list (e.g. legacy entity without skills).
            None => SendSkills::FullListWithCap { skills: Vec::new() },
        };
        post_packets.push(RawPacket::s2c(send.to_bytes()));
    }

    // Welcome message — prefer the live entity's name, fall back to the
    // resolved player name.
    let welcome_name = {
        let engine = crate::game_util::engine_for(worker_tx, world);
        match engine.get_entity(player_serial).await.as_ref().and_then(|e| e.mobile()) {
            Some(m) if !m.name.is_empty() => m.name.clone(),
            _ => player_name.to_string(),
        }
    };
    post_packets.push(spawn_builders::build_welcome_message(
        player_serial,
        graphic,
        "System",
        &format!("Welcome to demo server, {welcome_name} ({x}, {y}, {z})"),
    ));

    for pkt in &post_packets {
        if let Some(obs) = &mut *observer {
            obs.ingest_s2c(&pkt.data);
        }
        session.send(pkt.clone()).await?;
    }

    // ── DEV: give starter items — only on the very first spawn ───────
    //
    // On reconnect or relogin of a persistent character the backpack is
    // preserved, so we must NOT re-grant the set (it would accumulate on
    // every login).  `is_fresh_spawn` is true only for a brand-new entity:
    // the first spawn of a test account or a newly created normal character.
    if is_fresh_spawn {
        if let Some(p) = player {
            dev_items::give_starter_items(p, worker_tx, serial_alloc).await;
        }
    }

    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────

/// Resolve spawn data for a test account.
///
/// On the first login the account gets a fresh mobile serial from
/// `serial_alloc` and the mapping is stored in `wd.test_serials` so
/// every reconnect reuses the same serial.  This prevents collisions
/// with Lua-spawned NPC serials which also use `alloc_mobile()`.
async fn resolve_test_account_spawn(
    ta: &TestAccountInfo,
    worker_tx: &DemoWorkerTx,
    wd: &Arc<WorldData>,
    serial_alloc: &Arc<SerialAllocator>,
    world: u8,
    addr: std::net::SocketAddr,
) -> (u32, u16, u16, i8, u8, u16, u16, String, bool) {
    let body: u16 = crate::constants::body::MALE_HUMAN;
    let hue: u16 = crate::constants::hue::TEST_PLAYER;
    let name = ta.name.clone();

    // ── Resolve or allocate serial ────────────────────────────────────────
    //
    // Look up an existing serial for this account name.  If none exists yet,
    // allocate one now and persist it for future reconnects.
    let serial = {
        // Fast path: already allocated (read lock).
        let r = wd.test_serials.read().await;
        r.get(&ta.name).copied()
    };
    let serial = match serial {
        Some(s) => s,
        None => {
            // Slow path: first login — allocate and store (write lock).
            let mut w = wd.test_serials.write().await;
            // Double-check after acquiring the write lock.
            if let Some(&s) = w.get(&ta.name) {
                s
            } else {
                let s = serial_alloc
                    .alloc_mobile()
                    .expect("mobile serial space exhausted");
                w.insert(ta.name.clone(), s);
                trace!(
                    "[{addr}] test account '{}' → allocated serial {:#010X}",
                    ta.name, s
                );
                s
            }
        }
    };

    let engine = crate::game_util::engine_for(worker_tx, world);

    // Check if the entity already exists (reconnecting player).
    let existing = engine.get_entity(serial).await;

    if let Some(m) = existing.as_ref().and_then(|e| e.mobile())
    {
        // Entity already lives in the world — reuse its position.
        let display_name = if m.name.is_empty() {
            name.clone()
        } else {
            m.name.clone()
        };
        trace!(
            "[{addr}] test account '{}' ({:#010X}) reconnected at ({},{},{})",
            display_name, serial, m.x, m.y, m.z
        );
        (serial, m.x, m.y, m.z, m.direction, m.graphic, m.color, display_name, false)
    } else {
        // New test account — pick a random *valid* spawn position.
        let (rx, ry, rz) = pick_valid_spawn(&engine, wd, addr).await;
        let dir: u8 = 0;

        // Create the entity and spawn it in the engine.
        spawn_player_entity(
            serial, rx, ry, rz, dir, "White Walker", body, hue,
            // hits, mana, stamina, str_, dex, int — all 100 (no desync).
            100, 100, 100, 100, 100, 100,
            serial_alloc, worker_tx, world,
        ).await;

        trace!(
            "[{addr}] test account '{}' ({:#010X}) spawned at ({},{},{})",
            name, serial, rx, ry, rz
        );

        (serial, rx, ry, rz, dir, body, hue, name, true)
    }
}

/// Resolve spawn data for a normal (non-test) account.
///
/// Returns `(world, serial, x, y, z, direction, graphic, color, name)`.
///
/// Two cases:
/// - **Playable account** (`replay`): pick the serial from the shared
///   `playable_serials` pool and read its current world/position straight
///   from the engine.  Not tracked per-account.
/// - **Normal account**: use the per-account [`CharacterRecord`] (with its
///   stored `world`, so a character that logged out in a non-zero world
///   re-enters there).
async fn resolve_normal_account_spawn(
    slot: usize,
    account_name: Option<&str>,
    worker_tx: &DemoWorkerTx,
    wd: &Arc<WorldData>,
    default_world: u8,
    addr: std::net::SocketAddr,
) -> (u8, u32, u16, u16, i8, u8, u16, u16, String) {
    // ── Reserved playable-pool account ────────────────────────────────
    if account_name.map(is_playable_account).unwrap_or(false) {
        return resolve_playable_spawn(slot, worker_tx, wd, default_world, addr).await;
    }

    // ── Normal account: use the per-account character record ──────────
    if let Some(acct) = account_name {
        let rec = {
            let map = wd.account_characters.read().await;
            map.get(acct).and_then(|chars| chars.get(slot)).cloned()
        };
        if let Some(rec) = rec {
            // Cancel any pending logout timer — the player has reconnected.
            // This is a no-op if the timer already fired (Cancel on a missing
            // entry is silently ignored by the reaper).
            let _ = wd.reaper_tx.send(crate::logout::ReaperCmd::Cancel {
                serial: rec.serial,
            }).await;

            // ── Case 1: entity is in its normal game world ────────────
            //
            // This covers both the "player reconnected before the timer fired"
            // path (timer was just cancelled above) and the "player never logged
            // out via the reaper" path.
            let world = rec.world;
            let engine = crate::game_util::engine_for(worker_tx, world);
            engine.mark_player(rec.serial).await;
            if let Some(m) = engine.get_entity(rec.serial).await.as_ref().and_then(|e| e.mobile()) {
                let display_name = if m.name.is_empty() { rec.name.clone() } else { m.name.clone() };

                // Clear META_LOGOUT_PENDING if present — the player reconnected
                // before the timer fired (or before the post-restart immediate
                // transfer ran).  Without this the flag would persist into the
                // next save and cause a spurious transfer on the following load.
                {
                    if let Some(mut props) = engine.get_item_props(rec.serial).await {
                        if props.meta.contains_key(crate::logout::META_LOGOUT_PENDING) {
                            props.meta.remove(crate::logout::META_LOGOUT_PENDING);
                            engine.set_item_props(rec.serial, Some(props)).await;
                        }
                    }
                }

                return (world, rec.serial, m.x, m.y, m.z, m.direction, m.graphic, m.color, display_name);
            }

            // ── Case 2: entity is in the offline storage zone ─────────
            //
            // The logout timer fired while the player was offline.  The entity
            // was transferred to LOGOUT_STORAGE_MAP.  Read the return address
            // from the entity's meta, transfer it back, and log in normally.
            {
                let storage_engine = crate::game_util::engine_for(
                    worker_tx,
                    crate::logout::LOGOUT_STORAGE_MAP,
                );
                if let Some(storage_entity) = storage_engine.get_entity(rec.serial).await {
                    // Read the return address stored before the transfer.
                    let return_meta = storage_engine
                        .get_item_props(rec.serial).await
                        .and_then(|p| {
                            p.get_meta_str(crate::logout::META_LOGOUT_RETURN)
                                .map(str::to_string)
                        });

                    let (ret_world, ret_x, ret_y, ret_z, ret_dir) =
                        parse_return_meta(return_meta.as_deref(), world, addr);

                    info!(
                        "[{addr}] character '{}' ({:#010X}) returning from storage zone → \
                         world={} ({},{},{})",
                        rec.name, rec.serial, ret_world, ret_x, ret_y, ret_z,
                    );

                    // Remove the return meta before transferring back so it
                    // doesn't linger on the live entity.
                    {
                        if let Some(mut props) = storage_engine.get_item_props(rec.serial).await {
                            props.meta.remove(crate::logout::META_LOGOUT_RETURN);
                            storage_engine.set_item_props(rec.serial, Some(props)).await;
                        }
                    }

                    // Transfer back to the game world.
                    let transfer_result = storage_engine.transfer_entity(
                        crate::logout::LOGOUT_STORAGE_MAP,
                        ret_world,
                        rec.serial,
                        ret_x, ret_y, ret_z,
                        Some(ret_dir),
                    ).await;

                    match transfer_result {
                        Ok(_) => {
                            // Now that the entity is back in its game world,
                            // mark it as a player and read its current position.
                            let live_engine = crate::game_util::engine_for(worker_tx, ret_world);
                            live_engine.mark_player(rec.serial).await;

                            let (graphic, color, name) = match storage_entity.mobile() {
                                Some(m) => {
                                    let n = if m.name.is_empty() { rec.name.clone() } else { m.name.clone() };
                                    (m.graphic, m.color, n)
                                }
                                None => (crate::constants::body::MALE_HUMAN, rec.hue, rec.name.clone()),
                            };
                            return (ret_world, rec.serial, ret_x, ret_y, ret_z, ret_dir, graphic, color, name);
                        }
                        Err(e) => {
                            warn!(
                                "[{addr}] failed to transfer '{}' ({:#010X}) from storage: {:?}; \
                                 falling back to Britain bank",
                                rec.name, rec.serial, e,
                            );
                        }
                    }
                }
            }

            // ── Case 3: entity missing entirely ───────────────────────
            //
            // Not in its game world and not in storage.  Fall back
            // deterministically to world 0 at Britain bank.
            warn!(
                "[{addr}] character '{}' ({:#010X}) not found in world {} or storage; \
                 falling back to world 0 bank",
                rec.name, rec.serial, world
            );
            let (fx, fy, fz) = deterministic_fallback_spawn(worker_tx).await;
            return (0, rec.serial, fx, fy, fz, 0, rec.body, rec.hue, rec.name);
        }
    }

    // No record (e.g. selected an empty slot) — deterministic fallback.
    warn!("[{addr}] no character record for slot {slot}; spawning at world 0 bank");
    let player_serial = wd.player_serial;
    let (fx, fy, fz) = deterministic_fallback_spawn(worker_tx).await;
    (0, player_serial, fx, fy, fz, 0, crate::constants::body::MALE_HUMAN, 0u16, "Player".to_string())
}

/// Parse the `META_LOGOUT_RETURN` value (`"world|x|y|z|dir"`) into its
/// components.  Falls back to `(fallback_world, 1438, 1696, 0, 0)` on any
/// parse error.
fn parse_return_meta(
    meta: Option<&str>,
    fallback_world: u8,
    addr: std::net::SocketAddr,
) -> (u8, u16, u16, i8, u8) {
    let Some(s) = meta else {
        warn!("[{addr}] logout_return meta missing; using Britain bank fallback");
        return (fallback_world, 1438, 1696, 0, 0);
    };
    let parts: Vec<&str> = s.split('|').collect();
    if parts.len() < 5 {
        warn!("[{addr}] malformed logout_return meta '{s}'; using Britain bank fallback");
        return (fallback_world, 1438, 1696, 0, 0);
    }
    let world = parts[0].parse::<u8>().unwrap_or(fallback_world);
    let x     = parts[1].parse::<u16>().unwrap_or(1438);
    let y     = parts[2].parse::<u16>().unwrap_or(1696);
    let z     = parts[3].parse::<i8>().unwrap_or(0);
    let dir   = parts[4].parse::<u8>().unwrap_or(0);
    (world, x, y, z, dir)
}

/// Resolve spawn data for the reserved playable-pool account.
///
/// The serial comes from `playable_serials`; world and position are read
/// from the live engine entity (it may have been moved across worlds during
/// this run).  Falls back deterministically to world 0 if not found.
async fn resolve_playable_spawn(
    slot: usize,
    worker_tx: &DemoWorkerTx,
    wd: &Arc<WorldData>,
    default_world: u8,
    addr: std::net::SocketAddr,
) -> (u8, u32, u16, u16, i8, u8, u16, u16, String) {
    let player_serial = wd
        .playable_serials
        .get(slot)
        .copied()
        .unwrap_or(wd.player_serial);

    // The live entity may be in any world; probe each candidate world,
    // starting with the default, to find where the entity currently lives.
    for world in std::iter::once(default_world).chain(0u8..=5) {
        let engine = crate::game_util::engine_for(worker_tx, world);
        if let Some(m) = engine.get_entity(player_serial).await.as_ref().and_then(|e| e.mobile()) {
            // Ensure the engine treats this character as a player so death
            // routes through `handle_kill_player` (ghost).
            engine.mark_player(player_serial).await;
            let display_name = if m.name.is_empty() {
                format!("Entity {:#010X}", player_serial)
            } else {
                m.name.clone()
            };
            return (world, player_serial, m.x, m.y, m.z, m.direction, m.graphic, m.color, display_name);
        }
    }

    warn!("[{addr}] playable entity {:#010X} not found in any world", player_serial);
    let (fx, fy, fz) = deterministic_fallback_spawn(worker_tx).await;
    (0, player_serial, fx, fy, fz, 0, crate::constants::body::MALE_HUMAN, 0u16, "Player".to_string())
}

/// Resolve a deterministic, valid spawn near the Britain bank in world 0.
///
/// Unlike [`pick_valid_spawn`] this never returns a random map location —
/// it is used as a safe fallback so a missing entity never strands the
/// player at a random spot.
async fn deterministic_fallback_spawn(worker_tx: &DemoWorkerTx) -> (u16, u16, i8) {
    const BANK_X: u16 = 1438;
    const BANK_Y: u16 = 1696;
    let engine = crate::game_util::engine_for(worker_tx, 0);
    let z = engine
        .resolve_z(BANK_X, BANK_Y, 0, Heading::South)
        .await
        .unwrap_or(0);
    (BANK_X, BANK_Y, z)
}

// ── Shared spawn helpers ────────────────────────────────────────────────────

/// Pick a random *valid* spawn position, honoring the `--cluster` box.
///
/// Retries up to 50 times to find a tile with a resolvable Z; falls back
/// to the Britain bank area (1438, 1696, 0) if none is found.
async fn pick_valid_spawn(
    engine: &common::uo_engine::rpc::EngineProxy<DemoCommand>,
    wd: &Arc<WorldData>,
    addr: std::net::SocketAddr,
) -> (u16, u16, i8) {
    const MAX_SPAWN_ATTEMPTS: u32 = 50;
    let mut attempts = 0u32;
    loop {
        let (rx, ry) = {
            let mut rng = rand::rng();
            if let Some(half) = wd.cluster {
                let h = half as u16;
                (
                    rng.random_range(1438u16.saturating_sub(h)..1438 + h),
                    rng.random_range(1696u16.saturating_sub(h)..1696 + h),
                )
            } else {
                (rng.random_range(0..6144), rng.random_range(0..4096))
            }
        };
        attempts += 1;
        match engine.resolve_z(rx, ry, 0, Heading::South).await {
            Some(rz) => return (rx, ry, rz),
            None if attempts >= MAX_SPAWN_ATTEMPTS => {
                warn!(
                    "[{addr}] failed to find valid spawn after {} attempts, using fallback",
                    attempts
                );
                return (1438, 1696, 0);
            }
            None => continue,
        }
    }
}

/// Create a fresh player entity (with mount + backpack) and spawn it in the
/// engine, registering an empty backpack container.
#[allow(clippy::too_many_arguments)]
async fn spawn_player_entity(
    serial: u32,
    x: u16,
    y: u16,
    z: i8,
    direction: u8,
    name: &str,
    body: u16,
    hue: u16,
    hits: u16,
    mana: u16,
    stamina: u16,
    str_: u16,
    dex: u16,
    int: u16,
    serial_alloc: &Arc<SerialAllocator>,
    worker_tx: &DemoWorkerTx,
    world: u8,
) {
    let bp_serial = serial_alloc.alloc_item().expect("item serial space exhausted");
    let mount_serial = serial_alloc.alloc_item().expect("item serial space exhausted");
    let entity = spawn_builders::new_player_entity(
        serial, x, y, z, direction, name, body, hue,
        hits, mana, stamina, str_, dex, int,
        bp_serial, mount_serial,
        crate::skills::default_player_skills(),
    );

    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            world,
            DemoCommand::Engine(EngineCommand::SpawnEntity { entity_id: serial, data: entity }),
        ))
        .await;

    // Register an empty backpack container for the new player.
    {
        use packets::interaction::{DrawContainer, DrawContainerLegacy};
        use packets::traits::encode_packet;
        let draw_pkt = DrawContainerLegacy {
            id: DrawContainer::ID,
            serial: bp_serial,
            gump_model: 0x003C, // standard backpack gump
        };
        let _ = worker_tx
            .send(WorkerCommand::MapCommand(
                world,
                DemoCommand::Engine(EngineCommand::IngestContainerPacket {
                    data: encode_packet(&draw_pkt).into(),
                }),
            ))
            .await;
    }
}

// ── 0x00 CreateCharacter ──────────────────────────────────────────────────

/// Handle the CreateCharacter (0x00) packet.
///
/// Parses the creation request, allocates a fresh mobile serial, builds the
/// new player entity, records it against the account, then enters the world
/// exactly like a normal character selection.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_create_character(
    packet: &RawPacket,
    player: &mut Option<PlayerState>,
    account_name: &Option<String>,
    access_level: common::uo_engine::auth::AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &Arc<WorldData>,
    serial_alloc: &Arc<SerialAllocator>,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<Arc<WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<Arc<WorldEvent>>,
    addr: std::net::SocketAddr,
    client_version: u_core::ProtocolVersion,
) -> error::Result<()> {
    use packets::login::{CreateCharacter, LoginRejected, LoginRejectedReason};

    let wd = world_data;
    let world = wd.player_world;

    // The reserved playable-pool account is read-only — it cannot create
    // characters (it only re-lists the shared log pool).
    if account_name.as_deref().map(is_playable_account).unwrap_or(false) {
        warn!("[{addr}] playable account tried to create a character — rejected");
        session.send(RawPacket::s2c(encode_packet(&LoginRejected::new(
            LoginRejectedReason::CharacterAlreadyExist,
        )))).await?;
        return Ok(());
    }

    let create = match CreateCharacter::from_bytes(&packet.data) {
        Ok(c) => c,
        Err(e) => {
            warn!("[{addr}] failed to parse CreateCharacter: {e:?}");
            return Ok(());
        }
    };

    let raw_name = create.name.to_string();
    let name = raw_name.trim();
    if name.is_empty() {
        // The standard client validates names client-side, so an empty
        // name should never reach here; ignore defensively.
        warn!("[{addr}] CreateCharacter with empty name — ignoring");
        return Ok(());
    }

    // Reject duplicate names within the same account.
    if let Some(acct) = account_name {
        let dup = {
            let map = wd.account_characters.read().await;
            map.get(acct)
                .map(|chars| chars.iter().any(|c| c.name.eq_ignore_ascii_case(name)))
                .unwrap_or(false)
        };
        if dup {
            warn!("[{addr}] account '{acct}' tried to create duplicate character '{name}'");
            session.send(RawPacket::s2c(encode_packet(&LoginRejected::new(
                LoginRejectedReason::CharacterAlreadyExist,
            )))).await?;
            return Ok(());
        }
    }

    // ── Allocate serial + resolve appearance ──────────────────────────
    let serial = serial_alloc.alloc_mobile().expect("mobile serial space exhausted");
    let body = if create.is_female() {
        crate::constants::body::FEMALE_HUMAN
    } else {
        crate::constants::body::MALE_HUMAN
    };
    let hue = create.skin_hue;
    let dir: u8 = 0;

    // ── Pick a valid spawn position ────────────────────────────────────
    let engine = crate::game_util::engine_for(worker_tx, world);
    let (x, y, z) = pick_valid_spawn(&engine, wd, addr).await;

    // ── Create the entity ──────────────────────────────────────────────
    //
    // All characters start with 100/100/100 stats regardless of what the
    // client creation packet requested, so hits/mana/stamina stay in sync
    // with str/dex/int (no desync between current resources and stat caps).
    let str_ = 100u16;
    let dex = 100u16;
    let int = 100u16;
    spawn_player_entity(
        serial, x, y, z, dir, name, body, hue,
        str_, int, dex, // hits≈str, mana≈int, stamina≈dex
        str_, dex, int,
        serial_alloc, worker_tx, world,
    ).await;

    // ── Persist the character against the account ──────────────────────
    if let Some(acct) = account_name {
        {
            let mut map = wd.account_characters.write().await;
            map.entry(acct.clone()).or_default().push(CharacterRecord {
                serial,
                name: name.to_string(),
                body,
                hue,
                world,
            });
        }
        // Persist accounts to disk so the new character survives a restart.
        crate::game_util::persist_accounts(wd).await;
    }

    info!(
        "[{addr}] created character '{}' ({:#010X}) for account {:?} at ({},{},{})",
        name, serial, account_name, x, y, z
    );

    enter_world(
        player, serial, x, y, z, dir, body, hue, name,
        true, world, access_level, session, worker_tx, world_data,
        serial_alloc, observer, event_rx, event_tx_for_observer, client_version,
    ).await
}
