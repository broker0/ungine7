//! Replay session — fake game server that plays back a `.uolog` file.
//!
//! # World phase (playback)
//!
//! S→C packets are sent to the client with their original timing.
//!
//! `MoveAck (0x22)` is handled specially: it is **not** forwarded raw because
//! its `sequence` belongs to the original session.  Instead, we look up the
//! matching `MoveRequest` from the log (by sequence), apply the coordinate
//! delta, and send a `DrawGamePlayer (0x20)` with the updated position so the
//! client renders the new location.
//!
//! If the current client tries to move, we send `MoveReject (0x21)` + a fresh
//! `DrawGamePlayer` to snap them back to the replay position.
//!
//! `ResyncRequest (0x22)` from the client is answered with a `DrawGamePlayer`
//! carrying the current tracked position so the client can recover.
//!
//! `TargetCursor (0x6C)` and all other C→S packets are silently dropped.

mod client_handler;
mod engine_rpc;
pub mod playback;
pub mod playback_headless;
mod preprocess;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use log::{debug, info, trace, warn};

use u_core::Facing;
use network::error as fw_error;
use network::session::{Session, SessionEvent};
use packets::character::UpdateMobile;
use packets::login::{CharacterList, CharacterSlot, LoginCharacter, StartingLocation};
use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;
use packets::speech::{SendSpeech, SpeechType};
use packets::system::ClientViewRange;
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::DrawMobile;
use protocol::RawPacket;

use crate::continuum::{Zone, Worker, WorkerCommand};
use crate::continuum::traits::EntityStore;
use framework::ecumene::StaticWorldData;
use crate::log_player::{LogPlayer, LogPlayerSnapshot};
use crate::packet_log::{LogEntry, read_log};
use crate::replay_handler::{EngineHandler, ReplayCommand};
use crate::uo_engine::entity::{DemoEntity, MobileData};

// Re-export public items used by other modules (e.g. dot_commands).
pub use engine_rpc::ShadowTx;
pub use common::uo_engine::rpc::EngineProxy;
pub use common::uo_engine::handler::EngineCommand;

// Re-export headless playback types for external consumers.
pub use playback_headless::{
    PlaybackCommand, PlaybackStatus, HeadlessPlaybackResult,
    PacketLogEntry, PacketSource, HeadlessChannels,
    run_playback_headless,
};

use client_handler::FreeMoveResult;
pub use playback::run_playback;
pub use preprocess::preprocess;
use framework::diorama::{ObserverPipeline, VisibleWorld};
// ── Configuration ─────────────────────────────────────────────────────────

/// When `true`, the replay session sends a synthetic `0xA9 CharacterList`
/// after receiving `0x91 GameLogin` and waits for `0x5D LoginCharacter`
/// before starting playback.  This prevents the "create character" window
/// from flashing on screen.
///
/// Set to `false` to skip char-selection entirely (old behaviour) — useful
/// if you hit edge-case compatibility issues with specific client versions.
const CHAR_SELECT: bool = true;

/// Fast-forward playback speed multiplier.
///
/// When the user presses FF+Ns the replay does **not** do a snapshot seek;
/// instead it temporarily switches to accelerated playback that actually
/// plays back all intermediate packets at this speed multiplier (e.g. 4.0 =
/// four times faster than normal).
pub const FAST_FORWARD_SPEED: f64 = 5.0;

/// Serial assigned to the observer entity in Observer view mode.
///
/// Chosen to be outside the range of real entity serials (mobiles are
/// usually `0x00000001`–`0x3FFFFFFF`, items `0x40000000`+).
const OBSERVER_SERIAL: u32 = 0x00DE_AD01;

/// Body graphic for the observer character (male human, naked).
const OBSERVER_BODY: u16 = 0x0190;

/// Hue for the observer character (neutral, no coloring).
const OBSERVER_HUE: u16 = 0;

// ── View mode ─────────────────────────────────────────────────────────────

/// How the replay client experiences the session.
#[derive(Clone, Debug)]
pub enum ViewMode {
    /// Classic replay: the client **is** the recorded character.
    ///
    /// Packets flow through as-is, and `DrawGamePlayer (0x20)` carries the
    /// original player's serial.  Movement during playback is rejected.
    FirstPerson,

    /// Observer mode: the client controls a separate entity while the
    /// recorded character plays back as an NPC.
    ///
    /// The observer can walk freely during playback.  The recorded
    /// character's movements are synthesised as `UpdateMobile (0x77)`.
    Observer {
        /// Serial of the observer entity (what the UO client "is").
        observer_serial: u32,
        /// Serial of the recorded player character (visible as an NPC).
        player_serial: u32,
        /// Body graphic of the recorded character.
        player_body: u16,
        /// Hue of the recorded character.
        player_hue: u16,
        /// Notoriety of the recorded character.
        player_notoriety: Notoriety,
        /// Mobile status flags of the recorded character.
        player_flags: MobileFlags,
    },
}

impl ViewMode {
    /// Returns `true` when this is [`ViewMode::Observer`].
    pub fn is_observer(&self) -> bool {
        matches!(self, Self::Observer { .. })
    }

    /// Build an `UpdateMobile (0x77)` packet for the recorded character
    /// using the provided position tracker.
    ///
    /// Position, direction, body graphic, hue, and status flags are taken
    /// from the tracker (which is updated by every movement step and
    /// position-carrying packet).  Only the serial and notoriety come from
    /// the static `ViewMode` configuration.
    ///
    /// Only meaningful in `Observer` mode; returns `None` in `FirstPerson`.
    pub fn build_replay_char_update(
        &self,
        pos: &framework::rythmos::PositionTracker,
    ) -> Option<Bytes> {
        match self {
            Self::Observer {
                player_serial,
                player_notoriety,
                ..
            } => Some(
                UpdateMobile {
                    id: UpdateMobile::ID,
                    serial: *player_serial,
                    model: pos.body_type,
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    direction: pos.facing.raw(),
                    hue: pos.hue,
                    status_flags: MobileFlags(pos.flags),
                    notoriety: *player_notoriety,
                }
                .to_bytes(),
            ),
            Self::FirstPerson => None,
        }
    }
}

// ── Entry kinds ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum EntryKind {
    /// Forward raw S→C packet to client.
    Forward(Bytes),
    /// World-initialisation S→C packet (0x1B, 0x20, 0x55).
    ///
    /// During normal playback: forwarded as-is.
    /// After a seek: suppressed or replaced with a lightweight equivalent.
    WorldInit(Bytes),
    /// Synthesise DrawGamePlayer after applying movement delta.
    MoveAck { direction: Facing },
    /// C→S packet — not forwarded to the client during playback,
    /// stored so that the full log is available for packet-level stepping.
    ClientPacket(Bytes),
}

impl EntryKind {
    /// Returns `true` for C→S packets stored for packet-stepping.
    fn is_client(&self) -> bool {
        matches!(self, Self::ClientPacket(_))
    }
}

pub struct ReplayEntry {
    pub us_offset: u64,
    /// Index of the originating `LogEntry` in the raw log slice.
    /// Used by `compute_state_at` to replay state up to this point.
    pub log_idx: usize,
    pub kind: EntryKind,
}

// ── Observer entity helper ────────────────────────────────────────────────

/// Spawn (or re-spawn) the synthetic observer entity in the shadow
/// continuum zone.  Called on first run, after `populate_full_world`
/// (which resets zones), and on replay restart.
async fn spawn_observer_in_zone(
    shadow_tx: &ShadowTx,
    world: u8,
    observer_serial: u32,
    x: u16,
    y: u16,
    z: i8,
    facing: u8,
) {
    use packets::layer::Layer;
    use packets::world::EquippedItem;

    let obs_entity = DemoEntity::Mobile(MobileData {
        serial: observer_serial,
        graphic: OBSERVER_BODY,
        x,
        y,
        z,
        direction: facing,
        color: OBSERVER_HUE,
        status: MobileFlags(0),
        notoriety: Notoriety::Innocent,
        items: vec![
            EquippedItem {
                serial: observer_serial | 0x4000_0000,
                graphic: 0x3EA0,
                layer: Layer::Mount,
                color: None,
            },
            EquippedItem {
                serial: observer_serial | 0x5000_0000,
                graphic: 0x0E75,
                layer: Layer::Backpack,
                color: None,
            },
        ],
        name: "Observer".to_string(),
        hits: 100,
        hits_max: 100,
        mana: 100,
        mana_max: 100,
        stamina: 100,
        stamina_max: 100,
        str_: 100,
        dex: 100,
        int: 100,
        is_player: false,
        dead: false,
        living_graphic: 0,
        ..Default::default()
    });
    let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
    engine.spawn_entity(observer_serial, obs_entity).await;
}

/// Reset a zone and re-spawn the observer entity if in Observer mode.
///
/// **All zone resets in replay-proxy MUST go through this function**
/// to maintain the observer entity invariant.  The observer is a
/// synthetic entity that does not exist in any log-derived snapshot,
/// so every `engine.reset_zone` call destroys it.
///
/// When `observer_pos` is `Some`, the observer is re-spawned at the
/// given position.  Pass `None` in FirstPerson mode or when the
/// observer position is not yet known.
pub(super) async fn reset_zone(
    shadow_tx: &ShadowTx,
    world: u8,
    entities: Vec<DemoEntity>,
    containers: framework::continuum::HashContainerStore,
    view_mode: &ViewMode,
    observer_pos: Option<&framework::rythmos::PositionTracker>,
) {
    let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
    engine.reset_zone(entities, containers).await;

    if let (ViewMode::Observer { observer_serial, .. }, Some(pos)) = (view_mode, observer_pos) {
        spawn_observer_in_zone(
            shadow_tx, world, *observer_serial,
            pos.x, pos.y, pos.z, pos.facing.raw(),
        ).await;
    }
}

/// Re-ingest cached `0xD8 SendCustomHouse` packets into the shadow zone.
///
/// After a `reset_zone` / `populate_full_world` the zone's `EntityRegistry`
/// loses its `custom_defs` because `clear_all()` wipes them.  The entity
/// store is repopulated from `LogPlayer` snapshots, but those only carry
/// `DemoEntity::Multi` (foundation serial + graphic), not the custom house
/// tile data from `0xD8`.
///
/// This function re-sends every cached `0xD8` packet as an `IngestPacket`
/// to the shadow zone.  By this point the multi entities have already been
/// spawned via `zone.spawn()`, so `handle_ingest_packet`'s `0xD8` handler
/// will find them in the registry and call `add_custom()`.
async fn reingest_house_cache(
    shadow_tx: &ShadowTx,
    world: u8,
    house_cache: &HashMap<u32, Bytes>,
) {
    if house_cache.is_empty() {
        return;
    }
    let mut count = 0usize;
    for (_serial, data) in house_cache {
        let _ = shadow_tx.send(WorkerCommand::MapCommand(
            world,
            ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
        )).await;
        count += 1;
    }
    info!(
        "[replay] re-ingested {} cached 0xD8 SendCustomHouse packets into shadow zone (world={})",
        count, world,
    );
}

/// Re-ingest cached `0xD8` packets into the diorama `SessionView` registry.
///
/// This must be called after any operation that clears the session's
/// `EntityRegistry` (e.g. `rebuild_registry()`, `SessionView::new()`),
/// because those operations wipe `custom_defs`.  The multi entities
/// must already be present in the registry (via `rebuild_registry` or
/// `ingest_packet` of the foundation `0x1A`/`0xF3` packets).
fn reingest_house_cache_into_session(
    session: &mut framework::diorama::SessionView,
    house_cache: &HashMap<u32, Bytes>,
) {
    if house_cache.is_empty() {
        return;
    }
    let mut count = 0usize;
    for (_serial, data) in house_cache {
        session.ingest_packet(data);
        count += 1;
    }
    info!(
        "[replay] re-ingested {} cached 0xD8 packets into session registry",
        count,
    );
}

// ── Public entry point ────────────────────────────────────────────────────

pub async fn run(
    mut client: Session,
    log_path: &Path,
    static_data: Option<Arc<StaticWorldData>>,
) -> fw_error::Result<()> {
    info!("[replay] starting replay from {}", log_path.display());
    info!(
        "[replay] world data: {}",
        if static_data.is_some() {
            "loaded (terrain validation enabled)"
        } else {
            "not loaded (movement unrestricted)"
        }
    );

    // ── 1. Wait for 0x91 GameLogin from client ───────────────────────────

    if let Err(e) = wait_for_game_login(&mut client).await {
        warn!("[replay] did not receive 0x91 GameLogin: {e}");
        client.close().await;
        return Ok(());
    }

    // ── 2. Load and pre-process log ──────────────────────────────────────
    // Done before char-selection so we can extract the original character
    // name from the 0xA9 packet in the log.

    let log_entries = match read_log(log_path) {
        Ok(e) => e,
        Err(err) => {
            warn!("[replay] failed to read log {}: {err}", log_path.display());
            client.close().await;
            return Ok(());
        }
    };

    // SessionView tracks current_world and visible during preprocessing.

    let (initial_entries, init_packets, init_player, snapshots, house_cache, container_store) =
        preprocess(&log_entries, static_data.clone());

    // `init_player` is at end-of-log, so its position/world may differ from
    // the replay start.  Seek a fresh player to the origin 0x78 to get the
    // correct START position and world.
    let (initial_pos, initial_world) = if !initial_entries.is_empty() {
        let mut origin_player = LogPlayer::new(static_data.clone());
        let origin_log_idx = initial_entries[0].log_idx;
        origin_player.seek_to(&log_entries, origin_log_idx, &snapshots);
        (origin_player.observer.pos, origin_player.observer.session.current_world)
    } else {
        (init_player.observer.pos, init_player.observer.session.current_world)
    };
    let char_name = init_player.char_name.clone();
    let mut observer = init_player.observer.clone();
    // Override observer position and world to the START state (origin 0x78).
    // `init_player` is at end-of-log, so its coordinates/world may differ.
    observer.pos.x = initial_pos.x;
    observer.pos.y = initial_pos.y;
    observer.pos.z = initial_pos.z;
    observer.pos.facing = initial_pos.facing;
    observer.session.current_world = initial_world;

    // ── 2.5. Start Engine Shadow Worker ────────────────────────────────
    // Always started. Without data files physics works in "everything
    // allowed, z unchanged" mode — dynamic object snapshots are maintained
    // regardless.
    let (shadow_tx, shadow_rx) = tokio::sync::mpsc::channel(10000);
    {
        // Zone factory — allows the worker to auto-create zones for any
        // map_id on first access.  Essential for replays with world
        // transitions (e.g. Felucca → Trammel).
        let sd_for_factory = static_data.clone();
        let zone_factory: crate::continuum::worker::ZoneFactory<DemoEntity, framework::continuum::HashContainerStore> =
            Box::new(move |map_id: u8| {
                let sd: Option<Arc<dyn framework::ecumene::StaticDataProvider>> =
                    sd_for_factory.clone().map(|arc| arc as Arc<dyn framework::ecumene::StaticDataProvider>);
                Zone::<DemoEntity, framework::continuum::HashContainerStore>::new(
                    map_id,
                    sd,
                    Box::new(crate::uo_engine::store::DemoStore::new()),
                    896,
                    512,
                )
            });

        // Pre-create the initial zone **empty** — entities will stream in
        // via IngestPacket as playback dispatches S→C packets.  The zone
        // factory auto-creates zones for other worlds on first access.
        let mut worker = Worker::with_factory(shadow_rx, EngineHandler { serial_alloc: std::sync::Arc::new(common::uo_engine::serial_alloc::SerialAllocator::new()), equipment_index: std::collections::HashMap::new() }, zone_factory);
        let initial_world = observer.session.current_world;

        {
            let sd: Option<Arc<dyn framework::ecumene::StaticDataProvider>> =
                static_data.clone().map(|arc| arc as Arc<dyn framework::ecumene::StaticDataProvider>);
            let zone = Zone::<DemoEntity, framework::continuum::HashContainerStore>::new(
                initial_world,
                sd,
                Box::new(crate::uo_engine::store::DemoStore::new()),
                896,
                512,
            );
            worker.zones.insert(initial_world, zone);
        }

        tokio::spawn(worker.run());
        info!(
            "[shadow] started Engine shadow worker for map {} (empty — entities stream during playback, terrain: {})",
            initial_world,
            if static_data.is_some() {
                "enabled"
            } else {
                "disabled — passthrough mode"
            },
        );
    }

    {
        let move_ack_count = initial_entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::MoveAck { .. }))
            .count();
        let fwd_count = initial_entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Forward(_)))
            .count();
        info!(
            "[replay] preprocessed {} log entries → {} replay entries ({} forward, {} steps)",
            log_entries.len(),
            initial_entries.len(),
            fwd_count,
            move_ack_count,
        );
    }
    info!(
        "[replay] initial pos ({},{},{}) facing={}, char {:?}",
        initial_pos.x,
        initial_pos.y,
        initial_pos.z,
        initial_pos.facing,
        char_name.as_deref().unwrap_or("(unknown)"),
    );

    // ── 3. Character selection ───────────────────────────────────────────
    let view_mode = if CHAR_SELECT {
        let name = char_name.as_deref().unwrap_or("Replay");
        match do_char_select(&mut client, name, &init_player).await {
            Ok(vm) => vm,
            Err(e) => {
                warn!("[replay] char selection failed: {e}");
                client.close().await;
                return Ok(());
            }
        }
    } else {
        ViewMode::FirstPerson
    };

    // ── 3.5. Spawn observer entity in shadow continuum ───────────────────
    if let ViewMode::Observer { observer_serial, .. } = &view_mode {
        let world = observer.session.current_world;
        spawn_observer_in_zone(
            &shadow_tx, world, *observer_serial,
            initial_pos.x, initial_pos.y, initial_pos.z,
            initial_pos.facing.raw(),
        ).await;
        info!(
            "[shadow] spawned observer entity {:#010X} at ({},{},{}) in world {}",
            observer_serial, initial_pos.x, initial_pos.y, initial_pos.z, world,
        );
    }

    // ── 4. Playback → free-move loop ─────────────────────────────────────

    run_replay_loop(
        &mut client,
        &log_entries,
        initial_entries,
        init_packets,
        observer,
        &shadow_tx,
        &snapshots,
        &house_cache,
        &container_store,
        &static_data,
        &view_mode,
    )
    .await?;

    client.close().await;
    info!("[replay] finished");
    Ok(())
}

// ── Replay loop ───────────────────────────────────────────────────────────

// ── Observer init-packet rewriting ────────────────────────────────────────

/// Rewrite the bootstrap `init_packets` for Observer mode.
///
/// - `0x1B CharacterLocaleAndBody` — serial and body are replaced with the
///   observer's; position and map dimensions are preserved.
/// - `0x20 DrawGamePlayer` — serial and body are replaced.
/// - `0x78 DrawMobile` whose serial matches `player_serial` — replaced with
///   a minimal observer DrawMobile (naked human + backpack).
///
/// All other packets are passed through unchanged.
fn rewrite_init_packets_for_observer(
    init_packets: &[Bytes],
    observer_serial: u32,
    player_serial: u32,
) -> Vec<Bytes> {
    use packets::character::{CharacterLocaleAndBody, DrawGamePlayer};
    use packets::layer::Layer;
    use packets::world::EquippedItem;

    let mut out = Vec::with_capacity(init_packets.len() + 1);

    for pkt in init_packets {
        if pkt.is_empty() {
            out.push(pkt.clone());
            continue;
        }

        match pkt[0] {
            // 0x1B — replace serial/body with observer identity
            id if id == CharacterLocaleAndBody::ID => {
                if let Ok(orig) = CharacterLocaleAndBody::from_bytes(pkt) {
                    let rewritten = common::spawn::build_character_locale_and_body(
                        observer_serial,
                        OBSERVER_BODY,
                        orig.x,
                        orig.y,
                        orig.z,
                        orig.facing,
                        orig.map_width_minus8,
                        orig.map_height,
                    );
                    out.push(rewritten.data);
                } else {
                    out.push(pkt.clone());
                }
            }

            // 0x20 — replace serial/body with observer identity
            id if id == DrawGamePlayer::ID => {
                if let Ok(orig) = DrawGamePlayer::from_bytes(pkt) {
                    let rewritten = common::spawn::build_draw_game_player(
                        observer_serial,
                        OBSERVER_BODY,
                        OBSERVER_HUE,
                        orig.x,
                        orig.y,
                        orig.z,
                        orig.direction,
                    );
                    out.push(rewritten.data);
                } else {
                    out.push(pkt.clone());
                }
            }

            // 0x78 — if it's the recorded character, replace with a naked
            // observer DrawMobile and also emit the original as-is so the
            // recorded character appears as an NPC.
            id if id == DrawMobile::ID => {
                if let Ok(orig) = DrawMobile::parse(pkt, false) {
                    if orig.serial == player_serial {
                        // Observer DrawMobile — naked human on horse + backpack
                        let mount = EquippedItem {
                            serial: observer_serial | 0x4000_0000,
                            graphic: 0x3EA0, // horse mount graphic
                            layer: Layer::Mount,
                            color: None,
                        };
                        let backpack = EquippedItem {
                            serial: observer_serial | 0x5000_0000,
                            graphic: 0x0E75,
                            layer: Layer::Backpack,
                            color: None,
                        };
                        let obs_mob = DrawMobile {
                            serial: observer_serial,
                            graphic: OBSERVER_BODY,
                            x: orig.x,
                            y: orig.y,
                            z: orig.z,
                            direction: orig.direction,
                            color: OBSERVER_HUE,
                            status: MobileFlags(0),
                            notoriety: Notoriety::Innocent,
                            items: vec![mount, backpack],
                        };
                        out.push(obs_mob.to_bytes());
                        // Also emit the original DrawMobile so the recorded
                        // character appears as an NPC with full equipment.
                        out.push(pkt.clone());
                    } else {
                        out.push(pkt.clone());
                    }
                } else {
                    out.push(pkt.clone());
                }
            }

            _ => {
                out.push(pkt.clone());
            }
        }
    }

    out
}

/// Top-level playback → free-move cycle.
///
/// Runs until the client disconnects.  Restarts from the beginning of the
/// log whenever the user requests it via the action-menu gump.
///
/// On the **first** iteration the full `init_packets` are sent to bootstrap
/// the client into the world (0x1B, pre-origin items, etc.).  On subsequent
/// iterations (restart) we perform a seek to entry 0 instead — the client
/// is already in the world and doesn't need re-initialisation.
async fn run_replay_loop(
    client: &mut Session,
    log_entries: &[LogEntry],
    entries: Vec<ReplayEntry>,
    init_packets: Vec<Bytes>,
    mut observer: ObserverPipeline,
    shadow_tx: &ShadowTx,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
    container_store: &framework::continuum::ContainerStore,
    static_data: &Option<Arc<StaticWorldData>>,
    view_mode: &ViewMode,
) -> fw_error::Result<()> {
    let mut first_run = true;

    // Upcast static_data for the session view's multi registry.
    let sd_provider: Option<Arc<dyn framework::ecumene::StaticDataProvider>> =
        static_data.clone().map(|arc| arc as Arc<dyn framework::ecumene::StaticDataProvider>);

    // Ensure the session's local entity registry has access to static data
    // so that rebuild_registry actually works.
    // The session was created by LogPlayer without static_data.
    if let Some(ref sd) = sd_provider {
        observer.session.set_static_data(Some(sd.clone()));
    }

    loop {
        if first_run {
            // ── Bootstrap: send init_packets to bring the client into the
            // world for the first time.
            first_run = false;

            // In Observer mode, transform init_packets so that the client
            // identity (0x1B, 0x20, 0x78) uses the observer serial/body
            // instead of the recorded character's.  The recorded character
            // will be injected as an NPC after init.
            let effective_init: Vec<Bytes>;
            let init_ref: &[Bytes] = if let ViewMode::Observer { observer_serial, player_serial, .. } = view_mode {
                effective_init = rewrite_init_packets_for_observer(
                    &init_packets, *observer_serial, *player_serial,
                );
                &effective_init
            } else {
                &init_packets
            };

            observer.session.visible = VisibleWorld::new(
                observer.pos.x, observer.pos.y, ClientViewRange::DEFAULT as u16,
            );

            // In Observer mode, override the observer's serial to the
            // observer entity so that PositionTracker tracks the observer.
            if let ViewMode::Observer { observer_serial, .. } = view_mode {
                observer.pos.serial = *observer_serial;
                observer.pos.body_type = OBSERVER_BODY;
                observer.pos.hue = OBSERVER_HUE;
            }

            // Ingest init_packets (pre-origin S→C: items, mobiles, etc.)
            // into the shadow continuum so entities from the bootstrap
            // phase are present in the zone when the observer walks around.
            {
                let world = observer.session.current_world;
                let mut ingested = 0usize;
                for pkt in &init_packets {
                    if pkt.is_empty() { continue; }
                    let _ = shadow_tx.send(WorkerCommand::MapCommand(
                        world,
                        ReplayCommand::IngestPacket { data: pkt.clone(), emit_events: false },
                    )).await;
                    ingested += 1;
                }
                info!(
                    "[replay] ingested {} init packets into shadow continuum (world={})",
                    ingested, world,
                );
            }

            run_playback(
                client, log_entries, &entries, Some(init_ref),
                &mut observer, shadow_tx, &snapshots, &house_cache,
                static_data.clone(), view_mode,
            ).await?;

            info!(
                "[replay] playback done — pos ({},{},{}) facing={}, world={}, view_range={}",
                observer.pos.x, observer.pos.y, observer.pos.z,
                observer.pos.facing, observer.session.current_world, observer.view_range()
            );

            // Populate shadow continuum with the full end-of-log world so
            // that free-move shows all entities, not just those visible
            // at the moment playback was stopped.
            populate_full_world(log_entries, &mut observer, shadow_tx, &snapshots, static_data, container_store, view_mode).await;
            reingest_house_cache(shadow_tx, observer.session.current_world, house_cache).await;

            // Sync player entity position in the zone with the actual
            // playback position — populate_full_world replays the entire
            // log, so the player entity ends up at the end-of-log coords
            // which may differ from where playback was stopped.
            if observer.pos.is_ready() {
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.teleport(
                    observer.pos.serial, observer.pos.x, observer.pos.y, observer.pos.z,
                    None,
                ).await;
            }

            send_replay_end(client, &mut observer, shadow_tx, house_cache).await?;

            info!(
                "[replay] entering free-move phase (view_range={}, world={}, visible={})",
                observer.view_range(), observer.session.current_world, observer.session.visible.len(),
            );
            observer.session.rebuild_registry();
            reingest_house_cache_into_session(&mut observer.session, house_cache);
            match client_handler::run_free_move(
                client, &mut observer, shadow_tx, &house_cache,
                sd_provider.as_ref(),
            ).await? {
                FreeMoveResult::Disconnected => break,
                FreeMoveResult::RestartReplay => {
                    info!("[replay] restarting replay from beginning");
                    observer.reset();
                    if let Some(ref sd) = sd_provider {
                        observer.session.set_static_data(Some(sd.clone()));
                    }
                    continue;
                }
            }
        } else {
            // ── Restart: seek to entry 0 (no init_packets, client is
            // already in the world).
            // Advance LogPlayer to entry 0 so perform_seek has state.
            let mut player = LogPlayer::new(static_data.clone());
            let origin_log_idx = entries[0].log_idx;
            player.seek_to(log_entries, origin_log_idx, &snapshots);

            let replay_pos = player.observer.pos;
            // Reset the initial world zone to empty — entities will
            // stream in via IngestPacket during playback, just like
            // the first run.  Then ingest init_packets so pre-origin
            // entities are present.
            let world = player.observer.session.current_world;
            reset_zone(shadow_tx, world, vec![], framework::continuum::HashContainerStore::new(), view_mode, Some(&replay_pos)).await;
            {
                let mut ingested = 0usize;
                for pkt in &init_packets {
                    if pkt.is_empty() { continue; }
                    let _ = shadow_tx.send(WorkerCommand::MapCommand(
                        world,
                        ReplayCommand::IngestPacket { data: pkt.clone(), emit_events: false },
                    )).await;
                    ingested += 1;
                }
                info!(
                    "[replay] restart — reset zone {} to empty, ingested {} init packets",
                    world, ingested,
                );
            }
            reingest_house_cache(shadow_tx, world, house_cache).await;

            observer.reset();
            if let Some(ref sd) = sd_provider {
                observer.session.set_static_data(Some(sd.clone()));
            }
            observer.session.current_world = player.observer.session.current_world;
            observer.session.visible = VisibleWorld::new(
                replay_pos.x, replay_pos.y, ClientViewRange::DEFAULT as u16,
            );
            observer.pos = replay_pos;

            // In Observer mode, restore the observer identity.
            if let ViewMode::Observer { observer_serial, .. } = view_mode {
                observer.pos.serial = *observer_serial;
                observer.pos.body_type = OBSERVER_BODY;
                observer.pos.hue = OBSERVER_HUE;
            }

            // Send DrawGamePlayer so the client sees the starting position.
            if observer.pos.is_ready() {
                client
                    .send(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                    .await?;
            }

            // In Observer mode, also send the recorded character as an NPC
            // at the starting position.
            if let ViewMode::Observer { player_serial, player_notoriety, .. } = view_mode {
                let npc_update = UpdateMobile {
                    id: UpdateMobile::ID,
                    serial: *player_serial,
                    model: replay_pos.body_type,
                    x: replay_pos.x,
                    y: replay_pos.y,
                    z: replay_pos.z,
                    direction: replay_pos.facing.raw(),
                    hue: replay_pos.hue,
                    status_flags: MobileFlags(replay_pos.flags),
                    notoriety: *player_notoriety,
                };
                client.send(RawPacket::s2c(npc_update.to_bytes())).await?;
            }

            // Send all visible items from the continuum.
            let world = observer.session.current_world;
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
            let items = engine.items_in_area(*observer.view_rect()).await;
            for raw in &items {
                observer.session.ingest_packet(raw);
                client.send(RawPacket::s2c(raw.clone())).await?;
            }
            debug!(
                "[replay] restart seek — pos ({},{}) sent {} items",
                observer.pos.x, observer.pos.y, items.len()
            );

            run_playback(
                client, log_entries, &entries, None,
                &mut observer, shadow_tx, &snapshots, &house_cache,
                static_data.clone(), view_mode,
            ).await?;

            info!(
                "[replay] playback done — pos ({},{},{}) facing={}, world={}, view_range={}",
                observer.pos.x, observer.pos.y, observer.pos.z,
                observer.pos.facing, observer.session.current_world, observer.view_range()
            );

            populate_full_world(log_entries, &mut observer, shadow_tx, &snapshots, static_data, container_store, view_mode).await;
            reingest_house_cache(shadow_tx, observer.session.current_world, house_cache).await;

            // Sync player entity position (same reason as first-run branch).
            if observer.pos.is_ready() {
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.teleport(
                    observer.pos.serial, observer.pos.x, observer.pos.y, observer.pos.z,
                    None,
                ).await;
            }

            send_replay_end(client, &mut observer, shadow_tx, house_cache).await?;

            info!(
                "[replay] entering free-move phase (view_range={}, world={}, visible={})",
                observer.view_range(), observer.session.current_world, observer.session.visible.len(),
            );
            observer.session.rebuild_registry();
            reingest_house_cache_into_session(&mut observer.session, house_cache);
            match client_handler::run_free_move(
                client, &mut observer, shadow_tx, &house_cache,
                sd_provider.as_ref(),
            ).await? {
                FreeMoveResult::Disconnected => break,
                FreeMoveResult::RestartReplay => {
                    info!("[replay] restarting replay from beginning");
                    observer.reset();
                    if let Some(ref sd) = sd_provider {
                        observer.session.set_static_data(Some(sd.clone()));
                    }
                    continue;
                }
            }
        }
    }

    Ok(())
}

/// Advance `LogPlayer` to the end of the log and reset the shadow continuum
/// with the full world state for **every** world encountered in the log.
/// The player's position is **not** changed — the caller keeps using
/// `final_pos` from the playback loop.
///
/// This ensures that when transitioning to free-move after an early stop
/// the player can see all entities that existed at any point during the
/// session, not just those present at the moment of stopping.
///
/// **Important:** `session.current_world` is intentionally **not** changed.
/// The playback may have stopped in a different world than the one at the
/// end of the log (e.g. playback stopped in Felucca while the log ends in
/// Malas).  Overwriting `current_world` with the end-of-log world would
/// cause free-move to resolve coordinates against the wrong map, producing
/// garbage Z values.
async fn populate_full_world(
    log_entries: &[LogEntry],
    observer: &mut ObserverPipeline,
    shadow_tx: &ShadowTx,
    snapshots: &[LogPlayerSnapshot],
    static_data: &Option<Arc<StaticWorldData>>,
    container_store: &framework::continuum::ContainerStore,
    view_mode: &ViewMode,
) {
    if log_entries.is_empty() {
        return;
    }
    let mut player = LogPlayer::new(static_data.clone());
    player.advance_to(log_entries, log_entries.len() - 1, snapshots);

    let playback_world = observer.session.current_world;
    let all_worlds = player.take_all_world_entities();

    let mut total_entities = 0usize;
    for (&world_id, entities) in &all_worlds {
        total_entities += entities.len();
        // Pass the preprocess-collected container store so containers
        // survive the zone reset.  All containers are loaded into each
        // world zone — in practice only the player's current world
        // matters for DoubleClick.
        //
        // reset_zone handles observer re-spawn automatically — the
        // observer entity is only placed in the current playback world.
        let obs_pos = if world_id == playback_world {
            Some(&observer.pos)
        } else {
            None
        };
        reset_zone(
            shadow_tx, world_id, entities.clone(),
            framework::continuum::HashContainerStore::from(container_store.clone()),
            view_mode, obs_pos,
        ).await;
    }
    info!(
        "[replay] populating full world — {} entities across {} worlds, {} containers (playback world={})",
        total_entities, all_worlds.len(), container_store.len(), playback_world,
    );
    // NOTE: session.current_world is deliberately NOT overwritten.
}

/// Notify the client that playback has ended, then resync position and
/// visible items so the player is correctly placed for free-move.
async fn send_replay_end(
    client: &mut Session,
    observer: &mut ObserverPipeline,
    shadow_tx: &ShadowTx,
    _house_cache: &HashMap<u32, Bytes>,
) -> fw_error::Result<()> {
    let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
    let item_count = engine.count_entities().await;
    let end_text = format!(
        "*** Replay ended. {} items in world. You may now move freely. ***",
        item_count,
    );

    // System message (lower-left corner).
    let sys_msg = SendSpeech {
        serial: 0xFFFF_FFFF,
        model: 0xFFFF,
        speech_type: SpeechType::System,
        color: 0x03B2,
        font: 3,
        name: String::new(),
        message: end_text.clone(),
    };
    client.send(RawPacket::s2c(sys_msg.to_bytes())).await?;

    // Overhead message on the player character.
    if observer.pos.is_ready() {
        let overhead = SendSpeech {
            serial: observer.pos.serial,
            model: observer.pos.body_type,
            speech_type: SpeechType::Normal,
            color: 0x03B2,
            font: 3,
            name: String::new(),
            message: end_text,
        };
        client.send(RawPacket::s2c(overhead.to_bytes())).await?;
    }

    // Resync: send DrawGamePlayer + all cached items currently in view.
    if observer.pos.is_ready() {
        client
            .send(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
            .await?;
    }
    observer.session.visible.update_view(observer.pos.x, observer.pos.y);

    let world = observer.session.current_world;
    let vr = observer.view_rect();
    info!(
        "[replay] send_replay_end: view_range={} view_rect=({},{})..({},{}) world={} pos=({},{},{})",
        observer.view_range(), vr.x_min, vr.y_min, vr.x_max, vr.y_max,
        world, observer.pos.x, observer.pos.y, observer.pos.z,
    );
    let items = engine.items_in_area(*observer.view_rect()).await;
    if !items.is_empty() {
        debug!(
            "[replay] resync — sending {} cached items in view",
            items.len()
        );
        for raw in &items {
            observer.session.ingest_packet(raw);
            client.send(RawPacket::s2c(raw.clone())).await?;
        }
    }

    Ok(())
}

// ── Login sub-phase ───────────────────────────────────────────────────────

pub async fn wait_for_game_login(client: &mut Session) -> Result<(), String> {
    for _ in 0..10 {
        match client.recv().await.event {
            SessionEvent::Seed(_) => {}
            SessionEvent::Packet(p) if p.id() == 0x91 => {
                trace!("[replay] received 0x91 GameLogin");
                return Ok(());
            }
            SessionEvent::Packet(p) => {
                trace!("[replay] ignoring pre-login packet 0x{:02X}", p.id());
            }
            SessionEvent::Disconnected | SessionEvent::Stopped => {
                return Err("client disconnected before GameLogin".into());
            }
            SessionEvent::Error(e) => return Err(format!("transport error: {e}")),
        }
    }
    Err("did not receive 0x91 GameLogin within expected packets".into())
}

/// Perform character selection and determine the [`ViewMode`].
///
/// Sends a synthetic `0xA9 CharacterList` with two slots:
///
/// - **Slot 0** — the original recorded character (first-person replay)
/// - **Slot 1** — "Observer" (third-person: free movement, recorded char
///   appears as NPC)
///
/// Returns `ViewMode::FirstPerson` or `ViewMode::Observer` depending on
/// which slot the client picks.  The `player_*` fields for Observer mode
/// are filled from the provided `init_player` state.
pub async fn do_char_select(
    client: &mut Session,
    name: &str,
    init_player: &LogPlayer,
) -> Result<ViewMode, String> {
    let slots = vec![
        CharacterSlot::new(name),
        CharacterSlot::new("Observer"),
        CharacterSlot::new(""),
        CharacterSlot::new(""),
        CharacterSlot::new(""),
    ];

    let char_list = CharacterList::new(
        slots,
        vec![StartingLocation {
            index: 0,
            city_name: packets::u_io::FixedString::new("Britain"),
            area_name: packets::u_io::FixedString::new("Sweet Dreams Inn"),
        }],
        0,
    );
    client
        .send(RawPacket::s2c(char_list.to_bytes()))
        .await
        .map_err(|e| format!("failed to send CharacterList: {e}"))?;
    trace!("[replay] sent synthetic 0xA9 CharacterList (name={name:?}, +Observer)");

    for _ in 0..20 {
        match client.recv().await.event {
            SessionEvent::Packet(p) if p.id() == LoginCharacter::ID => {
                let slot = LoginCharacter::from_bytes(&p.data)
                    .map(|lc| lc.slot)
                    .unwrap_or(0);

                let view_mode = if slot == 1 {
                    info!("[replay] slot 1 selected — Observer mode");
                    ViewMode::Observer {
                        observer_serial: OBSERVER_SERIAL,
                        player_serial: init_player.player_serial,
                        player_body: init_player.observer.pos.body_type,
                        player_hue: init_player.observer.pos.hue,
                        player_notoriety: Notoriety::Innocent,
                        player_flags: MobileFlags(init_player.observer.pos.flags),
                    }
                } else {
                    info!("[replay] slot 0 selected — FirstPerson mode");
                    ViewMode::FirstPerson
                };

                return Ok(view_mode);
            }
            SessionEvent::Packet(p) => {
                trace!(
                    "[replay] ignoring packet 0x{:02X} during char select",
                    p.id()
                );
            }
            SessionEvent::Seed(_) => {}
            SessionEvent::Disconnected | SessionEvent::Stopped => {
                return Err("client disconnected during char selection".into());
            }
            SessionEvent::Error(e) => return Err(format!("transport error: {e}")),
        }
    }
    Err("did not receive 0x5D LoginCharacter within expected packets".into())
}

// ── Headless replay (no shadow continuum, no gumps) ─────────────────────────

/// Run a headless replay session — fake game server that plays back a
/// `.uolog` file with external control via channels.
///
/// No shadow continuum is started.  No in-game gumps.  Playback is controlled
/// entirely through the channels in [`HeadlessChannels`], and every packet
/// dispatched is also broadcast for the web packet inspector.
///
/// The function loops internally, restarting from the beginning if the
/// external anima sends [`PlaybackCommand::Restart`].  It returns
/// when the client disconnects or [`PlaybackCommand::Stop`] is received.
pub async fn run_headless(
    mut client: Session,
    log_path: &std::path::Path,
    static_data: Option<Arc<StaticWorldData>>,
    channels: &mut HeadlessChannels,
) -> fw_error::Result<()> {
    info!("[replay:headless] starting headless replay from {}", log_path.display());

    // 1. Wait for 0x91 GameLogin from client.
    if let Err(e) = wait_for_game_login(&mut client).await {
        warn!("[replay:headless] did not receive 0x91 GameLogin: {e}");
        client.close().await;
        return Ok(());
    }

    // 2. Load and pre-process log.
    let log_entries = match read_log(log_path) {
        Ok(e) => e,
        Err(err) => {
            warn!("[replay:headless] failed to read log {}: {err}", log_path.display());
            client.close().await;
            return Ok(());
        }
    };

    let (initial_entries, init_packets, init_player, snapshots, house_cache, _container_store) =
        preprocess(&log_entries, static_data.clone());

    let char_name = init_player.char_name.clone();
    let mut observer = init_player.observer.clone();

    {
        let move_ack_count = initial_entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::MoveAck { .. }))
            .count();
        let fwd_count = initial_entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Forward(_)))
            .count();
        info!(
            "[replay:headless] preprocessed {} log entries → {} replay entries ({} forward, {} steps)",
            log_entries.len(),
            initial_entries.len(),
            fwd_count,
            move_ack_count,
        );
    }
    info!(
        "[replay:headless] initial pos ({},{},{}) facing={}, char {:?}",
        observer.pos.x, observer.pos.y, observer.pos.z,
        observer.pos.facing,
        char_name.as_deref().unwrap_or("(unknown)"),
    );

    // 3. Character selection (headless always uses FirstPerson).
    if CHAR_SELECT {
        let name = char_name.as_deref().unwrap_or("Replay");
        if let Err(e) = do_char_select(&mut client, name, &init_player).await {
            warn!("[replay:headless] char selection failed: {e}");
            client.close().await;
            return Ok(());
        }
    }

    // Upcast static_data for the session view's multi registry.
    let sd_provider: Option<Arc<dyn framework::ecumene::StaticDataProvider>> =
        static_data.clone().map(|arc| arc as Arc<dyn framework::ecumene::StaticDataProvider>);

    if let Some(ref sd) = sd_provider {
        observer.session.set_static_data(Some(sd.clone()));
    }

    // 4. Playback loop (restart-capable).
    let mut first_run = true;
    loop {
        if first_run {
            first_run = false;

            observer.session.visible = framework::diorama::VisibleWorld::new(
                observer.pos.x, observer.pos.y, ClientViewRange::DEFAULT as u16,
            );

            let result = run_playback_headless(
                &mut client, &log_entries, &initial_entries, Some(&init_packets),
                &mut observer, &snapshots, &house_cache,
                static_data.clone(), channels,
            ).await?;

            match result {
                HeadlessPlaybackResult::Restart => {
                    info!("[replay:headless] restarting from beginning");
                    observer.reset();
                    if let Some(ref sd) = sd_provider {
                        observer.session.set_static_data(Some(sd.clone()));
                    }
                    continue;
                }
                HeadlessPlaybackResult::Disconnected => break,
                HeadlessPlaybackResult::Stopped => break,
                HeadlessPlaybackResult::Finished => {
                    // Finished is handled inside run_playback_headless as
                    // paused-at-end.  If it returns Finished the client
                    // likely disconnected.
                    break;
                }
            }
        } else {
            // Restart: seek to entry 0.
            let mut player = LogPlayer::new(static_data.clone());
            let origin_log_idx = initial_entries[0].log_idx;
            player.seek_to(&log_entries, origin_log_idx, &snapshots);

            let replay_pos = player.observer.pos;

            observer.reset();
            if let Some(ref sd) = sd_provider {
                observer.session.set_static_data(Some(sd.clone()));
            }
            observer.session.current_world = player.observer.session.current_world;
            observer.session.visible = framework::diorama::VisibleWorld::new(
                replay_pos.x, replay_pos.y, ClientViewRange::DEFAULT as u16,
            );
            observer.pos = replay_pos;

            // Send DrawGamePlayer so the client sees the starting position.
            if observer.pos.is_ready() {
                client
                    .send(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                    .await?;
            }

            // Stream visible entities from LogPlayer entity map.
            {
                use framework::ecumene::Entity;
                let world = observer.session.current_world;
                let entities = player.take_entities_for_world(world);
                let view_rect = *observer.view_rect();
                let mut count = 0usize;
                for entity in &entities {
                    let pos = Entity::pos(entity);
                    if pos.x >= view_rect.x_min && pos.x <= view_rect.x_max
                        && pos.y >= view_rect.y_min && pos.y <= view_rect.y_max
                    {
                        let raw = entity.to_raw_bytes();
                        observer.session.ingest_packet(&raw);
                        client.send(RawPacket::s2c(raw)).await?;
                        count += 1;
                    }
                }
                debug!(
                    "[replay:headless] restart seek — pos ({},{}) sent {} items",
                    observer.pos.x, observer.pos.y, count
                );
            }

            let result = run_playback_headless(
                &mut client, &log_entries, &initial_entries, None,
                &mut observer, &snapshots, &house_cache,
                static_data.clone(), channels,
            ).await?;

            match result {
                HeadlessPlaybackResult::Restart => {
                    info!("[replay:headless] restarting from beginning (again)");
                    observer.reset();
                    if let Some(ref sd) = sd_provider {
                        observer.session.set_static_data(Some(sd.clone()));
                    }
                    continue;
                }
                HeadlessPlaybackResult::Disconnected | HeadlessPlaybackResult::Stopped | HeadlessPlaybackResult::Finished => break,
            }
        }
    }

    client.close().await;
    info!("[replay:headless] finished");
    Ok(())
}
