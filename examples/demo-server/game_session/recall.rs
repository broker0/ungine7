//! Mark / Recall spell resolution.
//!
//! These two spells operate on a recall *rune* — an item in the caster's
//! backpack — rather than on a world entity, so their completion can't go
//! through the generic [`crate::magic::complete_cast`] path (which expects
//! a mobile or world item to compute line-of-sight and apply damage/heal).
//!
//! Instead, [`crate::game_session::rust_handler`] routes Mark/Recall casts
//! here when the cast-delay timer fires:
//!
//! * [`complete_mark`] — stores the caster's current coordinates on the
//!   targeted blank rune (via [`ItemProps`] metadata) and renames it.
//! * [`complete_recall`] — reads the coordinates from a marked rune and
//!   teleports the caster there.
//!
//! The teleport mirrors the `.tele` dot-command: it issues a
//! `TeleportEntity` engine command and sends `DrawGamePlayer` to the
//! client.  The world view is refreshed by the normal observer pipeline
//! once the player's position changes — no manual zone resync is needed.

use log::info;

use protocol::RawPacket;
use packets::character::DrawGamePlayer;
use packets::mobile_flags::MobileFlags;
use packets::traits::{encode_packet, BasicPacket};

use network::error;
use network::session::Session;

use common::uo_engine::item_props::{ItemProps, MetaValue};

use framework::ecumene::Entity as EngineEntity;

use crate::constants::{effect, item, sound};
use crate::game_util;
use crate::DemoWorkerTx;

use super::PlayerState;

// ── Rune metadata keys ─────────────────────────────────────────────────────

/// Marked-X coordinate stored on the rune's [`ItemProps`].
pub(crate) const META_RUNE_X: &str = "rune_x";
/// Marked-Y coordinate.
pub(crate) const META_RUNE_Y: &str = "rune_y";
/// Marked-Z coordinate.
pub(crate) const META_RUNE_Z: &str = "rune_z";
/// Marked world (map/facet id).  Absent on runes marked before multi-world
/// support; treated as the caster's current world for backward compatibility.
pub(crate) const META_RUNE_WORLD: &str = "rune_world";

// ── Mark ────────────────────────────────────────────────────────────────────

/// Complete a **Mark** cast: store the caster's current location on the
/// targeted blank rune.
///
/// The target must be a rune (graphic [`item::RUNE`]) located in the
/// caster's own backpack.  On success the rune's [`ItemProps`] gains the
/// marked coordinates and is renamed to "a marked rune".
pub(crate) async fn complete_mark(
    spell: &'static crate::magic::SpellDef,
    caster_serial: u32,
    rune_serial: u32,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = game_util::engine_for(worker_tx, world);

    // Caster position (the location being marked).
    let (cx, cy, cz) = match engine.get_entity(caster_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.x, m.y, m.z),
        None => return Ok(()),
    };

    // Validate the target rune: must be in the caster's backpack and have
    // the rune graphic.
    if !validate_rune(&engine, caster_serial, rune_serial).await {
        session.send(game_util::system_speech("That is not a recall rune.")).await?;
        return Ok(());
    }

    // Consume mana + reagents now that the cast resolves successfully.
    if !crate::magic::consume_spell_cost(spell, caster_serial, worker_tx, world).await {
        crate::game_util::send_fizzle(worker_tx, world, caster_serial, cx, cy, cz).await;
        session.send(game_util::system_speech("The spell fizzles.")).await?;
        return Ok(());
    }

    // Store the coordinates and rename the rune.
    let mut props = engine.get_item_props(rune_serial).await
        .unwrap_or_else(|| ItemProps::with_name("a marked rune"));
    props.set_name("a marked rune");
    props.set_meta(META_RUNE_X, MetaValue::Int(cx as i64));
    props.set_meta(META_RUNE_Y, MetaValue::Int(cy as i64));
    props.set_meta(META_RUNE_Z, MetaValue::Int(cz as i64));
    props.set_meta(META_RUNE_WORLD, MetaValue::Int(world as i64));
    engine.set_item_props(rune_serial, Some(props)).await;

    // Visual / audio feedback at the caster's location: a sparkle on the
    // caster plus the mark sound.
    game_util::send_effect(
        worker_tx, world,
        3, // stationary effect at source
        caster_serial, 0,
        effect::RECALL_SPARKLE,
        cx, cy, cz,
        0, 0, 0,
        10, 10,
        false, false,
    ).await;
    game_util::send_sound(worker_tx, world, sound::MARK, cx, cy, cz as i16).await;

    info!(
        "[recall] 0x{:08X} marked rune 0x{:08X} at ({},{},{})",
        caster_serial, rune_serial, cx, cy, cz,
    );

    Ok(())
}

// ── Recall ───────────────────────────────────────────────────────────────────

/// Complete a **Recall** cast: read the marked location from the targeted
/// rune and teleport the caster there.
///
/// `player` is the caster's session-level [`PlayerState`] — it is updated
/// with the new position so subsequent movement/view logic stays in sync.
///
/// When the rune's marked world differs from the caster's current world, the
/// teleport requires the atomic cross-zone transfer (which needs the session
/// loop's observer / event plumbing).  In that case this function consumes the
/// spell cost, plays the departure effect, and returns
/// `Ok(Some(PendingTeleport))` for the session loop to execute.  Same-world
/// recalls are handled inline and return `Ok(None)`.
pub(super) async fn complete_recall(
    spell: &'static crate::magic::SpellDef,
    rune_serial: u32,
    player: &mut PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<Option<super::game_logic::PendingTeleport>> {
    let world = player.world;
    let engine = game_util::engine_for(worker_tx, world);

    // Validate the rune belongs to the caster's backpack.
    if !validate_rune(&engine, player.serial, rune_serial).await {
        session.send(game_util::system_speech("That is not a recall rune.")).await?;
        return Ok(None);
    }

    // Read the marked coordinates.
    let props = engine.get_item_props(rune_serial).await;
    let (Some(tx), Some(ty), Some(tz)) = (
        props.as_ref().and_then(|p| p.get_meta_int(META_RUNE_X)),
        props.as_ref().and_then(|p| p.get_meta_int(META_RUNE_Y)),
        props.as_ref().and_then(|p| p.get_meta_int(META_RUNE_Z)),
    ) else {
        session.send(game_util::system_speech("That rune is not yet marked.")).await?;
        return Ok(None);
    };
    let (tx, ty, tz) = (tx as u16, ty as u16, tz as i8);
    // Marked world: default to the caster's current world for runes marked
    // before multi-world support existed.
    let target_world = props
        .as_ref()
        .and_then(|p| p.get_meta_int(META_RUNE_WORLD))
        .map(|w| w as u8)
        .unwrap_or(world);

    // Consume mana + reagents now that the cast resolves successfully.
    if !crate::magic::consume_spell_cost(spell, player.serial, worker_tx, world).await {
        let (ox, oy, oz) = (player.x, player.y, player.z);
        crate::game_util::send_fizzle(worker_tx, world, player.serial, ox, oy, oz).await;
        session.send(game_util::system_speech("The spell fizzles.")).await?;
        return Ok(None);
    }

    // Cross-world recall: defer to the session loop's atomic transfer.  The
    // departure effect/sound are played here at the origin; arrival feedback
    // is handled by the transfer path.
    if target_world != world {
        game_util::send_effect(
            worker_tx, world,
            3, player.serial, 0,
            effect::RECALL_SPARKLE,
            player.x, player.y, player.z,
            0, 0, 0,
            10, 10,
            false, false,
        ).await;
        game_util::send_sound(worker_tx, world, sound::RECALL, player.x, player.y, player.z as i16).await;
        info!(
            "[recall] 0x{:08X} recalling cross-world {} → {} at ({},{},{}) via rune 0x{:08X}",
            player.serial, world, target_world, tx, ty, tz, rune_serial,
        );
        return Ok(Some(super::game_logic::PendingTeleport {
            world: target_world,
            x: tx,
            y: ty,
            z: tz,
        }));
    }

    // Teleport the entity in the engine (same world).
    engine.teleport(player.serial, tx, ty, tz, Some(player.direction)).await;

    // Update session-level player state.
    player.x = tx;
    player.y = ty;
    player.z = tz;

    // Tell the client about the new position (mirrors the `.tele` command).
    let (graphic, color) = match engine.get_entity(player.serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.graphic, m.color),
        None => (crate::constants::body::MALE_HUMAN, 0),
    };
    let dgp = DrawGamePlayer {
        id: DrawGamePlayer::ID,
        serial: player.serial,
        body_type: graphic,
        _pad0: (),
        hue: color,
        flags: MobileFlags(0),
        x: tx,
        y: ty,
        _pad1: (),
        direction: player.direction,
        z: tz,
    };
    session.send(RawPacket::s2c(encode_packet(&dgp))).await?;

    // Arrival sound — send it directly to the recalling player's session.
    //
    // A broadcast (`send_sound`) would be routed by the *old* observer zone
    // (the player is still indexed near the departure point when the engine
    // processes the broadcast), so a long-distance recall would drop the
    // sound entirely.  Sending it on the session guarantees the caster hears
    // it at the arrival location.
    {
        use packets::system::PlaySoundEffect;
        session.send(RawPacket::s2c(encode_packet(&PlaySoundEffect {
            id: PlaySoundEffect::ID,
            mode: 1,
            sound_model: sound::RECALL,
            unknown: 0,
            x: tx,
            y: ty,
            z: tz as i16,
        }))).await?;
    }

    info!(
        "[recall] 0x{:08X} recalled to ({},{},{}) via rune 0x{:08X}",
        player.serial, tx, ty, tz, rune_serial,
    );

    Ok(None)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Verify that `rune_serial` is a recall rune sitting in `owner_serial`'s
/// backpack.
async fn validate_rune(
    engine: &common::uo_engine::rpc::EngineProxy<crate::DemoCommand>,
    owner_serial: u32,
    rune_serial: u32,
) -> bool {
    // The rune must be a known item with the rune graphic.
    let info = engine.find_item_info(rune_serial & 0x7FFF_FFFF).await;
    let Some((_serial, graphic, _color, _amount)) = info else {
        return false;
    };
    if graphic != item::RUNE {
        return false;
    }

    // The rune must live in the caster's backpack.
    let bp_serial = match engine.get_entity(owner_serial).await {
        Some(entity) => entity.backpack_serial(),
        None => None,
    };
    let Some(bp_serial) = bp_serial else { return false };

    matches!(
        engine.find_container_of_item(rune_serial & 0x7FFF_FFFF).await,
        Some(cs) if cs == bp_serial
    )
}
