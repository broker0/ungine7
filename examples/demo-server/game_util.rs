//! Shared game-logic primitives for the demo server.
//!
//! This module collects small helpers that are used across multiple game
//! subsystems (combat, magic, bandaging, items, session loop) to avoid
//! duplicating the same code in each file.
//!
//! ## Contents
//!
//! - **Geometry**: [`chebyshev`] distance.
//! - **RNG**: [`random_range`].
//! - **Packet builders**: [`system_speech`], [`system_message`],
//!   [`system_message_gray`], [`fizzle_packets`].
//! - **Broadcast helpers**: [`send_sound`], [`send_effect`],
//!   [`send_animation`] — thin wrappers around `DemoCommand::Broadcast*`
//!   that eliminate the verbose `WorkerCommand::MapCommand(world, …)`
//!   boilerplate at each call site.

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use framework::continuum::WorkerCommand;

use common::uo_engine::rpc::EngineProxy;

use crate::constants::{anim, body, effect, hue, sound};
use crate::{DemoCommand, DemoWorkerTx};

// ── Engine proxy helper ──────────────────────────────────────────────────

/// Create an [`EngineProxy`] for the given player's world.
///
/// Shorthand for `EngineProxy::<DemoCommand>::new(worker_tx.clone(), world)`.
pub fn engine_for(worker_tx: &DemoWorkerTx, world: u8) -> EngineProxy<DemoCommand> {
    EngineProxy::new(worker_tx.clone(), world)
}

// ── Account persistence ────────────────────────────────────────────────────

/// On-disk path for the per-account character map.
///
/// Persists `WorldData::account_characters` (names / appearance / world per
/// character) so the character-selection screen survives a server restart.
pub const ACCOUNTS_SAVE_PATH: &str = "accounts_save.json";

/// Persist the current `account_characters` map to [`ACCOUNTS_SAVE_PATH`].
///
/// Called whenever the map changes (character creation, cross-world
/// transfer) so the data survives even a non-graceful shutdown.  Failures
/// are logged but never propagated — persistence must not break gameplay.
pub async fn persist_accounts(world_data: &crate::WorldData) {
    let snapshot = {
        let map = world_data.account_characters.read().await;
        map.clone()
    };
    let path = std::path::Path::new(ACCOUNTS_SAVE_PATH);
    if let Err(e) = common::uo_engine::snapshot::save_accounts_to_file(&snapshot, path) {
        log::warn!("[accounts] failed to persist {ACCOUNTS_SAVE_PATH}: {e}");
    }
}

// ── Geometry ──────────────────────────────────────────────────────────────

/// Chebyshev (king-move) distance between two tile positions.
pub fn chebyshev(x1: u16, y1: u16, x2: u16, y2: u16) -> u16 {
    let dx = (x1 as i32 - x2 as i32).unsigned_abs() as u16;
    let dy = (y1 as i32 - y2 as i32).unsigned_abs() as u16;
    dx.max(dy)
}

// ── RNG ──────────────────────────────────────────────────────────────────

/// Random value in inclusive range `[min, max]`.
///
/// Returns `min` if `min >= max`.
pub fn random_range(min: u16, max: u16) -> u16 {
    if min >= max {
        return min;
    }
    use rand::Rng;
    rand::rng().random_range(min..=max)
}

// ── Packet builders ──────────────────────────────────────────────────────

/// Build a "System" speech packet (overhead text from the "System" entity).
///
/// Serial `0x01010101`, model `0x0101`, speech_type `Normal`,
/// color `0x03E8`, font 3.  Used for in-world feedback messages
/// (bandage prompts, spell fizzle text, etc.).
pub fn system_speech(msg: &str) -> RawPacket {
    use packets::speech::{SendSpeech, SpeechType};
    RawPacket::s2c(
        SendSpeech {
            serial: 0x0101_0101,
            model: 0x0101,
            speech_type: SpeechType::Normal,
            color: 0x03E8,
            font: 3,
            name: "System".to_string(),
            message: msg.to_string(),
        }
        .to_bytes(),
    )
}

/// Build an overhead speech packet anchored to a specific entity.
///
/// Unlike [`system_speech`] (which always speaks as the fixed "System"
/// entity), this renders `message` above the entity identified by `serial`
/// using the supplied `model` (graphic) and `color`. Lua scripts use this via
/// `engine:send_overhead_message(serial, message, color)`.
pub fn overhead_speech(serial: u32, model: u16, message: &str, color: u16) -> RawPacket {
    use packets::speech::{SendSpeech, SpeechType};
    RawPacket::s2c(
        SendSpeech {
            serial,
            model,
            speech_type: SpeechType::Normal,
            color,
            font: 3,
            name: String::new(),
            message: message.to_string(),
        }
        .to_bytes(),
    )
}

/// Build a system-channel message in **red** (error / spell failure).
///
/// Serial `0xFFFFFFFF`, model `0xFFFF`, speech_type `System`,
/// color `SYSTEM_RED` (`0x0025`), font 3.
pub fn system_message(msg: &str) -> RawPacket {
    use packets::speech::{SendSpeech, SpeechType};
    RawPacket::s2c(
        SendSpeech {
            serial: 0xFFFF_FFFF,
            model: 0xFFFF,
            speech_type: SpeechType::System,
            color: hue::SYSTEM_RED,
            font: 3,
            name: String::new(),
            message: msg.to_string(),
        }
        .to_bytes(),
    )
}

/// Build a system-channel message in **gray** (informational).
///
/// Serial `0xFFFFFFFF`, model `0xFFFF`, speech_type `Normal`,
/// color `SYSTEM_GRAY` (`0x03B2`), font 3.
pub fn system_message_gray(msg: &str) -> RawPacket {
    use packets::speech::{SendSpeech, SpeechType};
    RawPacket::s2c(
        SendSpeech {
            serial: 0xFFFF_FFFF,
            model: 0xFFFF,
            speech_type: SpeechType::Normal,
            color: hue::SYSTEM_GRAY,
            font: 3,
            name: "System".to_string(),
            message: msg.to_string(),
        }
        .to_bytes(),
    )
}

/// Build spell-fizzle packets for a synchronous context (no `await`).
///
/// Returns a `Vec` containing:
/// 1. A [`system_speech`] packet with `msg`.
/// 2. A `PlaySoundEffect` for the fizzle sound.
/// 3. A `GraphicalEffect` for the fizzle visual.
pub fn fizzle_packets(serial: u32, x: u16, y: u16, z: i8, msg: &str) -> Vec<RawPacket> {
    use packets::system::PlaySoundEffect;
    use packets::world::GraphicalEffect;

    vec![
        system_speech(msg),
        RawPacket::s2c(encode_packet(&PlaySoundEffect {
            id: PlaySoundEffect::ID,
            mode: 1,
            sound_model: sound::FIZZLE,
            unknown: 0,
            x,
            y,
            z: 0,
        })),
        RawPacket::s2c(encode_packet(&GraphicalEffect {
            id: GraphicalEffect::ID,
            direction_type: 3,
            source_serial: serial,
            target_serial: 0,
            model: effect::FIZZLE,
            x,
            y,
            z,
            target_x: 0,
            target_y: 0,
            target_z: 0,
            speed: 10,
            duration: 30,
            _pad0: (),
            fixed_direction: 0,
            explode: 0,
        })),
    ]
}

// ── Broadcast helpers ────────────────────────────────────────────────────

/// Broadcast a sound effect via `DemoCommand::BroadcastSound`.
pub async fn send_sound(
    worker_tx: &DemoWorkerTx,
    world: u8,
    sound_id: u16,
    x: u16, y: u16, z: i16,
) {
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        world,
        DemoCommand::BroadcastSound { sound_id, x, y, z },
    )).await;
}

/// Broadcast a graphical effect via `DemoCommand::BroadcastEffect`.
pub async fn send_effect(
    worker_tx: &DemoWorkerTx, world: u8,
    direction_type: u8,
    source_serial: u32, target_serial: u32,
    graphic: u16,
    x: u16, y: u16, z: i8,
    target_x: u16, target_y: u16, target_z: i8,
    speed: u8, duration: u8,
    fixed_direction: bool, explode: bool,
) {
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        world,
        DemoCommand::BroadcastEffect {
            direction_type, source_serial, target_serial, graphic,
            x, y, z, target_x, target_y, target_z,
            speed, duration, fixed_direction, explode,
        },
    )).await;
}

/// Broadcast a mobile animation via `DemoCommand::BroadcastAnimation`.
///
/// `reverse`, `repeat` and `frame_delay` control playback; pass
/// `reverse: false, repeat: false, frame_delay: 1` for the common case.
#[allow(clippy::too_many_arguments)]
pub async fn send_animation(
    worker_tx: &DemoWorkerTx, world: u8,
    serial: u32, action: u16, frame_count: u8, repeat_count: u16,
    reverse: bool, repeat: bool, frame_delay: u8,
    x: u16, y: u16,
) {
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        world,
        DemoCommand::BroadcastAnimation {
            serial, action, frame_count, repeat_count,
            reverse, repeat, frame_delay,
            x, y,
        },
    )).await;
}

// ── Resource harvesting ──────────────────────────────────────────────────

/// Ask the worker to harvest from a resource node, returning what it decided.
///
/// The worker owns both the static map data (for source validation) and the
/// node depletion/regeneration state, so this single round-trip validates the
/// source and applies the policy atomically.  Returns
/// [`HarvestReply::Invalid`](crate::commands::HarvestReply::Invalid) if the channel is closed (treated as a failed
/// attempt by the caller).
pub async fn try_harvest_resource(
    worker_tx: &DemoWorkerTx,
    world: u8,
    x: u16, y: u16, z: i8,
    graphic: u16,
    kind: crate::gathering::GatherKind,
    source: crate::commands::GatherSource,
    want: u16,
) -> crate::commands::HarvestReply {
    let (reply, rx) = tokio::sync::oneshot::channel();
    let sent = worker_tx.send(WorkerCommand::MapCommand(
        world,
        DemoCommand::TryHarvestResource { x, y, z, graphic, kind, source, want, reply },
    )).await;
    if sent.is_err() {
        return crate::commands::HarvestReply::Invalid;
    }
    rx.await.unwrap_or(crate::commands::HarvestReply::Invalid)
}

// ── Mount-aware animation resolution ─────────────────────────────────────

/// Resolve a humanoid animation action ID accounting for mount state.
///
/// Returns `Some(action)` with the correct mounted/on-foot variant, or
/// `None` if the animation should be skipped entirely (e.g. emotes or
/// crafting gestures have no mounted variant and look wrong on a horse).
///
/// This is the **single source of truth** for animation resolution —
/// all call sites (combat, magic, potions, crafting, gathering, emotes)
/// should use this instead of hand-rolling their own mapping.
pub fn resolve_animation(action: u16, is_mounted: bool) -> Option<u16> {
    if !is_mounted {
        return Some(action);
    }
    match action {
        // Melee attacks → generic mounted attack
        anim::SLASH_1H | anim::PIERCE_1H
        | anim::SWING_2H | anim::SLASH_2H | anim::PIERCE_2H => {
            Some(anim::MOUNTED_ATTACK)
        }
        // Ranged attacks → generic mounted attack
        anim::SHOOT_BOW | anim::SHOOT_XBOW => {
            Some(anim::MOUNTED_ATTACK)
        }
        // Get-hit → mounted get-hit
        anim::GET_HIT => Some(anim::MOUNTED_GET_HIT),
        // Spellcasting → mounted cast variants
        anim::CAST_DIRECTED => Some(anim::MOUNTED_CAST_DIRECTED),
        anim::CAST_AREA     => Some(anim::MOUNTED_CAST_AREA),
        // Emotes / eat / craft gestures — no mounted variant, skip
        anim::BOW | anim::SALUTE | anim::EAT => None,
        // Death animations — dismount happens first, pass through
        // Everything else (fidgets, walk, etc.) — pass through
        other => Some(other),
    }
}

/// Broadcast a mount-aware animation, skipping entirely if the action has
/// no suitable mounted variant.
///
/// Combines [`resolve_animation`] with [`send_animation`] into a single
/// call that handles the `None` (skip) case automatically.
pub async fn send_resolved_animation(
    worker_tx: &DemoWorkerTx, world: u8,
    serial: u32, action: u16, is_mounted: bool,
    frame_count: u8, repeat_count: u16,
    x: u16, y: u16,
) {
    if let Some(resolved) = resolve_animation(action, is_mounted) {
        send_animation(
            worker_tx, world, serial, resolved, frame_count, repeat_count,
            false, false, 1, x, y,
        ).await;
    }
}

// ── Body / gender helpers ────────────────────────────────────────────────

/// Check if a body graphic represents a female character.
pub fn is_female_body(graphic: u16) -> bool {
    graphic == body::FEMALE_HUMAN
}

/// Pick a random hurt (pain) sound appropriate for the character's body.
///
/// Classic UO uses five male and five female hurt sounds.
pub fn random_hurt_sound(graphic: u16) -> u16 {
    use rand::Rng;
    if is_female_body(graphic) {
        let pool = [
            sound::FEMALE_HURT_1, sound::FEMALE_HURT_2,
            sound::FEMALE_HURT_3, sound::FEMALE_HURT_4,
            sound::FEMALE_HURT_5,
        ];
        pool[rand::rng().random_range(0..pool.len())]
    } else {
        let pool = [
            sound::MALE_HURT_1, sound::MALE_HURT_2,
            sound::MALE_HURT_3, sound::MALE_HURT_4,
            sound::MALE_HURT_5,
        ];
        pool[rand::rng().random_range(0..pool.len())]
    }
}

/// Send fizzle broadcast effects (sound + visual) at a position.
///
/// Async variant used by spell/bandage code that has access to `worker_tx`.
pub async fn send_fizzle(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serial: u32,
    x: u16, y: u16, z: i8,
) {
    send_sound(worker_tx, world, sound::FIZZLE, x, y, z as i16).await;
    send_effect(
        worker_tx, world,
        3, serial, 0, effect::FIZZLE,
        x, y, z, 0, 0, 0,
        10, 30, false, false,
    ).await;
}

// ── Corpse decay ─────────────────────────────────────────────────────────

/// Default corpse decay time in seconds.
const CORPSE_DECAY_SECS: u64 = 120;

/// Schedule a corpse to be removed from the world after a delay.
///
/// Spawns a background task that sleeps for [`CORPSE_DECAY_SECS`] and then
/// sends a `RemoveEntity` command.  Uses the same fire-and-forget pattern
/// as [`schedule_miss_sound`](crate::combat::schedule_miss_sound).
pub fn schedule_corpse_decay(
    worker_tx: &DemoWorkerTx,
    world: u8,
    corpse_serial: u32,
) {
    let tx = worker_tx.clone();
    let delay = std::time::Duration::from_secs(CORPSE_DECAY_SECS);

    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let engine = EngineProxy::<DemoCommand>::new(tx, world);
        engine.remove_entity(corpse_serial).await;
    });
}

/// Schedule a set of entities (e.g. a treasure chest and its guardians) to be
/// removed from the world after `secs` seconds.
///
/// Spawns a single background task that sleeps and then removes every serial.
/// Guardians that were already killed are simply no-ops on removal.
pub fn schedule_treasure_decay(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serials: Vec<u32>,
    secs: u64,
) {
    let tx = worker_tx.clone();
    let delay = std::time::Duration::from_secs(secs);

    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let engine = EngineProxy::<DemoCommand>::new(tx, world);
        for serial in serials {
            engine.remove_entity(serial).await;
        }
    });
}

/// Schedule a set of entities to be removed from the world one-by-one, in a
/// randomized order, with a staggered delay between each removal.
///
/// Spawns a single background task that sleeps for `first_delay_secs`, then
/// removes each serial in turn waiting `interval_secs` between removals.  Used
/// by the Wall of Stone spell so its blocks fade out individually rather than
/// all at once.  Each removal emits a `DeleteObject` (0x1D) to nearby clients.
pub fn schedule_staggered_decay(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serials: Vec<u32>,
    first_delay_secs: u64,
    interval_secs: u64,
) {
    if serials.is_empty() {
        return;
    }
    let tx = worker_tx.clone();

    tokio::spawn(async move {
        use rand::seq::SliceRandom;
        let mut order = serials;
        order.shuffle(&mut rand::rng());

        let engine = EngineProxy::<DemoCommand>::new(tx, world);
        tokio::time::sleep(std::time::Duration::from_secs(first_delay_secs)).await;
        let mut first = true;
        for serial in order {
            if !first {
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            }
            first = false;
            engine.remove_entity(serial).await;
        }
    });
}

// ── Mount NPC respawn on death ─────────────────────────────────────────────

/// Spawn a default mount NPC at a death location from a dropped mount item.
///
/// Called from the combat/magic kill path when a player dies while mounted and
/// the engine could NOT restore the original NPC from its saved data (i.e.
/// [`KillResult::dropped_mount`](common::uo_engine::handler::KillResult) is
/// `Some`).  The NPC is placed one tile east of the death location.
///
/// Returns `Some(npc_serial)` on success, `None` if the mount graphic is not
/// recognised by [`crate::constants::mount`].
pub async fn spawn_mount_npc_on_death(
    worker_tx: &DemoWorkerTx,
    world: u8,
    mount_item: &packets::world::EquippedItem,
    x: u16,
    y: u16,
    z: i8,
) -> Option<u32> {
    use common::uo_engine::entity::{DemoEntity, MobileData};
    use packets::mobile_flags::MobileFlags;
    use packets::movement::Notoriety;

    let def = crate::constants::mount::mount_graphic_to_mount(mount_item.graphic)?;

    let engine = engine_for(worker_tx, world);
    let npc_serial = engine.allocate_mobile_serial().await;
    if npc_serial == 0 {
        return None;
    }

    let npc = DemoEntity::Mobile(MobileData {
        serial: npc_serial,
        graphic: def.body,
        x: x.wrapping_add(1),
        y,
        z,
        direction: 0,
        color: mount_item.color.unwrap_or(0),
        status: MobileFlags(0),
        notoriety: Notoriety::Attackable,
        items: Vec::new(),
        name: def.name.to_string(),
        hits: 50,
        hits_max: 50,
        mana: 0,
        mana_max: 0,
        stamina: 50,
        stamina_max: 50,
        str_: 50,
        dex: 50,
        int: 10,
        is_player: false,
        dead: false,
        living_graphic: 0,
        noto_class: common::uo_engine::notoriety::NotorietyClass::Neutral,
        ..Default::default()
    });

    engine.spawn_entity(npc_serial, npc).await;
    Some(npc_serial)
}
