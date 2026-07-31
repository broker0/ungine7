//! Bank box interaction: find a nearby banker and open the player's bank box.
//!
//! A banker is an ordinary mobile tagged with `ItemProps.meta["npc_type"] = "banker"`.
//! Players open their bank box by saying `bank` near a banker NPC
//! (see the speech interception in `session_loop.rs`).  This module:
//!
//! * resolves the nearest banker to the player ([`find_nearest_banker`]),
//! * creates the bank box container on first use (equipped on [`Layer::Bank`]),
//! * opens the existing bank box on subsequent uses,
//! * registers the container in [`OpenContainers`] as [`ContainerKind::Bank`]
//!   so it auto-closes when the player moves.
//!
//! Bank box contents are stored in the zone's [`HashContainerStore`](framework::continuum::HashContainerStore) and
//! are persisted through `.save` / `.load` commands.

use bytes::Bytes;
use log::{info, warn};

use protocol::RawPacket;
use packets::interaction::{DrawContainerLegacy, DrawContainer};
use packets::layer::Layer;
use packets::system::PlaySoundEffect;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};
use packets::world::EquippedItem;

use network::error;
use network::session::Session;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::rpc::EngineProxy;

use crate::bank;
use crate::game_util::{chebyshev, engine_for};
use crate::{DemoCommand, DemoWorkerTx};

use super::containers::{ContainerKind, OpenContainers};
use super::PlayerState;

/// Maximum distance (Chebyshev tiles) a player may be from a banker.
const BANKER_RANGE: u16 = 6;

/// Eye-height offset for LOS checks (matches interaction / combat).
const EYE_HEIGHT: i16 = 14;

/// Sound effect played when the bank box opens.
const BANK_OPEN_SOUND: u16 = 0x01FC;

// ── Banker resolution ──────────────────────────────────────────────────────

/// Find the nearest banker NPC to the player within [`BANKER_RANGE`].
///
/// Filters mobiles tagged with `meta["npc_type"] == "banker"`, requires
/// line of sight, and returns the closest one (Chebyshev distance).
pub(super) async fn find_nearest_banker(
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
) -> Option<u32> {
    let engine = engine_for(worker_tx, player.world);

    let area = framework::ecumene::TileRect::from_view(
        player.x, player.y, BANKER_RANGE,
    );
    let entities = engine.query_area(area).await;

    let mut best: Option<(u16, u32)> = None;
    for ent in &entities {
        let DemoEntity::Mobile(m) = ent else { continue };
        if m.serial == player.serial {
            continue;
        }
        let dist = chebyshev(player.x, player.y, m.x, m.y);
        if dist > BANKER_RANGE {
            continue;
        }
        // Check if this mobile is tagged as a banker.
        let Some(props) = engine.get_item_props(m.serial).await else {
            continue;
        };
        if !bank::is_banker(&props) {
            continue;
        }
        // Line of sight check.
        if !engine.check_los(
            player.x, player.y, player.z as i16 + EYE_HEIGHT,
            m.x, m.y, m.z as i16,
        ).await {
            continue;
        }
        match best {
            Some((bd, _)) if dist >= bd => {}
            _ => best = Some((dist, m.serial)),
        }
    }

    best.map(|(_, serial)| serial)
}

// ── Open bank box ──────────────────────────────────────────────────────────

/// Open (or create) the player's bank box container.
///
/// 1. Turns the banker to face the player.
/// 2. Looks for an existing equipped item on [`Layer::Bank`] on the player.
/// 3. If none exists, allocates a serial, equips it on the player, and
///    registers an empty container in the zone's `HashContainerStore`.
/// 4. Sends `DrawContainer` (0x24) + `ContainerContent` (0x3C) + open
///    sound to the client.
/// 5. Registers the container in `open_containers` as [`ContainerKind::Bank`].
///
/// Returns `true` if the bank box was opened successfully.
pub(super) async fn open_bank_box(
    banker_serial: u32,
    player: &PlayerState,
    open_containers: &mut OpenContainers,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let engine = engine_for(worker_tx, player.world);

    // ── Turn the banker to face the player ────────────────────────────
    turn_banker_to_player(banker_serial, player, &engine).await;

    // ── Step 1: resolve or create the bank box serial ─────────────────
    let bank_serial = resolve_or_create_bank_box(player.serial, &engine).await;
    let Some(bank_serial) = bank_serial else {
        warn!("[bank] failed to resolve or create bank box for player 0x{:08X}", player.serial);
        session.send(crate::game_util::system_message_gray(
            "Unable to open your bank box.",
        )).await?;
        return Ok(false);
    };

    // ── Step 2: ensure the container exists in the zone store ─────────
    // Send a DrawContainer (0x24) packet to the engine so it creates
    // the ContainerInfo entry if it doesn't exist yet.
    // Use the legacy (7-byte) form for engine ingest — the ingest path
    // reads only the first 7 bytes regardless of client version.
    let draw_ingest_bytes = encode_packet(&DrawContainerLegacy {
        id: DrawContainer::ID,
        serial: bank_serial,
        gump_model: bank::BANK_GUMP_MODEL,
    });
    engine.ingest_container(Bytes::from(draw_ingest_bytes)).await;

    // ── Step 3: retrieve contents and send to client ──────────────────
    let container = engine.get_container(bank_serial).await;
    let items: Vec<(u32, u16, u16, u16, u16, u32, u16)> = container
        .as_ref()
        .map(|c| {
            c.items.iter().map(|i| (
                i.serial, i.graphic, i.amount, i.x, i.y, bank_serial, i.color,
            )).collect()
        })
        .unwrap_or_default();

    let version = player.client_version;

    // Send DrawContainer (0x24) — version-aware.
    session.send(common::spawn::build_draw_container(bank_serial, bank::BANK_GUMP_MODEL, version)).await?;

    // Send ContainerContent (0x3C) — version-aware.
    session.send(common::spawn::build_container_content(&items, version)).await?;

    // Send open sound effect (single client only).
    session.send(RawPacket::s2c(encode_packet(&PlaySoundEffect {
        id: PlaySoundEffect::ID,
        mode: 1,
        sound_model: BANK_OPEN_SOUND,
        unknown: 0,
        x: player.x,
        y: player.y,
        z: player.z as i16,
    }))).await?;

    // ── Step 4: register in open containers ───────────────────────────
    // Store the player's current position so the bank box is only closed
    // when the player actually moves (not on every packet cycle).
    open_containers.open(bank_serial, ContainerKind::Bank {
        x: player.x,
        y: player.y,
    });

    info!(
        "[bank] opened bank box: player=0x{:08X} bank=0x{:08X} ({} items)",
        player.serial, bank_serial,
        container.as_ref().map(|c| c.items.len()).unwrap_or(0),
    );
    Ok(true)
}

/// Turn the banker NPC to face the player.
///
/// Uses `teleport()` to the banker's own position with a new direction
/// computed from the banker→player vector.
async fn turn_banker_to_player(
    banker_serial: u32,
    player: &PlayerState,
    engine: &EngineProxy<DemoCommand>,
) {
    let Some(entity) = engine.get_entity(banker_serial).await else { return };
    let Some(m) = entity.mobile() else { return };

    let dx = player.x as i32 - m.x as i32;
    let dy = player.y as i32 - m.y as i32;
    let Some(heading) = u_core::Heading::from_delta(dx, dy) else { return };

    // Teleport to the same position with a new facing direction.
    engine.teleport(banker_serial, m.x, m.y, m.z, Some(heading as u8)).await;
}

/// Resolve an existing bank box serial from the player's equipment, or
/// create a new one if none exists.
async fn resolve_or_create_bank_box(
    player_serial: u32,
    engine: &EngineProxy<DemoCommand>,
) -> Option<u32> {
    // Check if the player already has a bank box equipped.
    let entity = engine.get_entity(player_serial).await?;
    let mobile = entity.mobile()?;

    if let Some(eq) = mobile.items.iter().find(|eq| eq.layer == Layer::Bank) {
        // Already has a bank box — return its serial.
        info!(
            "[bank] player 0x{:08X} already has bank box 0x{:08X}",
            player_serial, eq.serial,
        );
        return Some(eq.serial);
    }

    // No bank box yet — allocate a serial and equip it.
    let bank_serial = engine.allocate_serial().await;
    if bank_serial == 0 {
        warn!("[bank] serial space exhausted — cannot create bank box");
        return None;
    }

    // Equip the bank box on the player (Layer::Bank = 0x1D).
    // The graphic is a standard chest (0x09AB = metal chest).
    let bank_item = EquippedItem {
        serial: bank_serial,
        graphic: 0x09AB,
        layer: Layer::Bank,
        color: None,
    };

    let equipped = engine.equip_on_mobile(player_serial, bank_item).await;
    if !equipped {
        warn!(
            "[bank] failed to equip bank box 0x{:08X} on player 0x{:08X}",
            bank_serial, player_serial,
        );
        return None;
    }

    info!(
        "[bank] created bank box 0x{:08X} for player 0x{:08X}",
        bank_serial, player_serial,
    );
    Some(bank_serial)
}
