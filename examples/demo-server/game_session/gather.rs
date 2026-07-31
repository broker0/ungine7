//! Resource gathering (mining): double-click tool → target source → timed
//! gather → resource into backpack.
//!
//! Flow:
//! 1. Player double-clicks a gathering tool (e.g. a pickaxe) in their
//!    backpack or on the ground nearby.
//! 2. A neutral **ground** target cursor is shown.
//! 3. Player targets a resource source — either a static tile (validated by
//!    the targeted tile graphic) or an item node carrying the
//!    [`gather_resource`](crate::gathering::META_GATHER_RESOURCE) meta.
//! 4. Distance is checked and a timed [`ActionPayload::Gather`] begins
//!    (occupies the `SkillUse` slot).
//! 5. On completion, distance is re-checked, a success roll is made, and on
//!    success a resource item is dropped into the player's backpack.
//!
//! There is no source depletion — each gather is an independent roll.

use std::pin::Pin;

use log::{info, warn};
use tokio::time::{Duration, Sleep};

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};
use packets::interaction::{DoubleClick, TargetCursor};

use network::error;
use network::session::Session;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::{DropResult, DropTarget, HeldItemInfo};
use common::uo_engine::rpc::EngineProxy;
use framework::ecumene::Entity as EngineEntity;

use crate::actions::{self, ActionKind, ActionPayload, ActiveAction};
use crate::gathering::{self, ToolDef};
use crate::game_util;
use crate::{DemoCommand, DemoWorkerTx};

use super::pending_cursor::{CursorKind, PendingCursor};
use super::session_state::SessionContext;

// ── Constants ────────────────────────────────────────────────────────────

/// Cursor ID base for gathering targets (distinct from spell/skill/bandage).
const GATHER_CURSOR_BASE: u32 = 0x6A7E_0000;

// ── Double-click intercept ───────────────────────────────────────────────

/// Check if a double-click packet targets a gathering tool.
///
/// If it does, verify the tool is accessible (in a container / backpack, or
/// on the ground within range), then send a **ground** target cursor and
/// populate `ctx.infra.pending_cursor`.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_tool_double_click(
    packet: &RawPacket,
    ctx: &mut SessionContext,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    if packet.id() != DoubleClick::ID {
        return Ok(false);
    }

    let dc = match DoubleClick::from_bytes(&packet.data) {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };

    // Paperdoll request (high bit) — not a tool.
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }

    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let p = match &ctx.infra.player {
        Some(p) => p,
        None => return Ok(false),
    };
    let player_serial = p.serial;
    let world = p.world;

    // Resolve the clicked item's graphic (backpack first, then ground).
    let engine = crate::game_util::engine_for(worker_tx, world);
    let graphic = match resolve_item_graphic(&engine, player_serial, clean_serial).await {
        Some(g) => g,
        None => return Ok(false),
    };

    // Is it a known gathering tool?
    let Some(tool) = gathering::lookup_tool(graphic) else {
        return Ok(false);
    };

    // Check skill slot blocking before showing the cursor.
    let has_pending = ctx.has_pending_cursor();
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, has_pending, ctx.has_blocking_gump()) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Send a *ground* target cursor (cursor_target = 1) so the client reports
    // the tile graphic and coordinates of whatever the player targets.
    let cursor_id = GATHER_CURSOR_BASE | (clean_serial & 0x0000_FFFF);

    let tc = TargetCursor {
        id: TargetCursor::ID,
        cursor_target: 1, // ground/tile target
        cursor_id,
        cursor_type: 0, // neutral
        target_serial: 0,
        x: 0,
        y: 0,
        _pad0: (),
        z: 0,
        graphic: 0,
    };

    ctx.infra.pending_cursor = Some(PendingCursor::gather(cursor_id, player_serial, tool.tool_graphic));

    session.send(game_util::system_speech("Where do you wish to dig?")).await?;
    session.send(RawPacket::s2c(encode_packet(&tc))).await?;
    Ok(true)
}

// ── Target cursor response ───────────────────────────────────────────────

/// Handle a gathering target-cursor response (0x6C).
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_gather_target(
    packet: &RawPacket,
    pending: PendingCursor,
    ctx: &mut SessionContext,
    skill_timer: &mut Pin<Box<Sleep>>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let CursorKind::GatherTarget { user_serial, tool_graphic } = pending.kind else {
        unreachable!("handle_gather_target called with non-GatherTarget cursor kind");
    };

    let tc = match TargetCursor::from_bytes(&packet.data) {
        Ok(tc) => tc,
        Err(_) => return Ok(true),
    };

    // Cancelled by the player.
    if common::dot_commands::is_target_cancelled(&tc) {
        return Ok(true);
    }

    let p = match ctx.infra.player.as_ref() {
        Some(p) => p,
        None => return Ok(true),
    };
    let world = p.world;
    let (px, py) = (p.x, p.y);

    let Some(tool) = gathering::lookup_tool(tool_graphic) else {
        return Ok(true);
    };

    let engine = crate::game_util::engine_for(worker_tx, world);

    // Resolve the target location and validate the source.
    let resolved = match resolve_target_source(&engine, tool, &tc).await {
        Some(loc) => loc,
        None => {
            session.send(game_util::system_message("You can't mine that.")).await?;
            return Ok(true);
        }
    };
    let (tx, ty, tz) = (resolved.x, resolved.y, resolved.z);

    // Distance check (Chebyshev).
    if game_util::chebyshev(px, py, tx, ty) > gathering::GATHER_RANGE {
        session.send(game_util::system_message("That is too far away.")).await?;
        return Ok(true);
    }

    // Re-check skill slot (has_pending=false: this IS the resolving cursor).
    if let Err(msg) = actions::can_begin_skill(&ctx.active_skill, false, ctx.has_blocking_gump()) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Start the timed gather action.
    let delay = Duration::from_millis(tool.delay_ms);
    let payload = ActionPayload::Gather {
        user_serial,
        tool_graphic,
        target_x: tx,
        target_y: ty,
        target_z: tz,
        source_graphic: resolved.graphic,
        source_serial: resolved.serial,
        world,
    };
    let new_action = ActiveAction::new(ActionKind::SkillUse, delay, payload);
    skill_timer.as_mut().reset(new_action.completes_at);
    ctx.active_skill = Some(new_action);

    session.send(game_util::system_speech("You begin mining...")).await?;
    Ok(true)
}

// ── Action completion ────────────────────────────────────────────────────

/// Complete a gather action: re-check distance, ask the worker to harvest the
/// node (which validates the source and applies depletion / regeneration), and
/// on a yield drop the produced resource into the player's backpack.
pub(super) async fn complete_gather(
    user_serial: u32,
    tool_graphic: u16,
    target_x: u16,
    target_y: u16,
    target_z: i8,
    source_graphic: u16,
    source_serial: u32,
    world: u8,
    serial_alloc: &std::sync::Arc<common::uo_engine::serial_alloc::SerialAllocator>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let Some(tool) = gathering::lookup_tool(tool_graphic) else {
        return Ok(());
    };

    let engine = crate::game_util::engine_for(worker_tx, world);

    // Re-fetch the player to re-check distance (they may have walked off).
    let (px, py, mounted) = match engine.get_entity(user_serial).await.as_ref().and_then(|e| e.mobile()) {
        Some(m) => {
            let mt = m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount);
            (m.x, m.y, mt)
        }
        None => return Ok(()),
    };
    if game_util::chebyshev(px, py, target_x, target_y) > gathering::GATHER_RANGE {
        session.send(game_util::system_message("You move too far away to continue mining.")).await?;
        return Ok(());
    }

    // Working feedback: swing animation + sound at the source.
    game_util::send_resolved_animation(worker_tx, world, user_serial, tool.anim, mounted, 7, 1, px, py).await;
    game_util::send_sound(worker_tx, world, tool.sound, target_x, target_y, target_z as i16).await;

    // Ask the worker to harvest the node.  It validates the source against the
    // authoritative map/entity data and applies the resource-node policy
    // (capacity, depletion, time-based regeneration / maturation).  We let the
    // tool's max amount be the upper bound the node may grant this swing.
    let source = if source_serial != 0 {
        crate::commands::GatherSource::ItemNode { serial: source_serial }
    } else {
        crate::commands::GatherSource::StaticTile
    };
    let want = tool.resource.amount_max.max(1);
    let reply = game_util::try_harvest_resource(
        worker_tx, world, target_x, target_y, target_z, source_graphic, tool.kind, source, want,
    ).await;

    let (graphic, color, amount, name) = match reply {
        crate::commands::HarvestReply::Yield { graphic, color, amount, name } => {
            (graphic, color, amount, name)
        }
        crate::commands::HarvestReply::Depleted => {
            session.send(game_util::system_speech(
                "This vein has been exhausted.  Give it time to recover.",
            )).await?;
            return Ok(());
        }
        crate::commands::HarvestReply::Nothing => {
            session.send(game_util::system_speech(
                "You loosen some rocks but fail to find any useful ore.",
            )).await?;
            return Ok(());
        }
        crate::commands::HarvestReply::Invalid => {
            session.send(game_util::system_message("You can't mine that.")).await?;
            return Ok(());
        }
    };

    // Resolve the player's backpack.
    let bp_serial = match engine.get_entity(user_serial).await.and_then(|e| e.backpack_serial()) {
        Some(s) => s,
        None => {
            warn!("[gather] player {:#010X} has no backpack — discarding resource", user_serial);
            return Ok(());
        }
    };

    // Allocate a serial and drop the resource into the backpack (auto-stacks).
    let Some(serial) = serial_alloc.alloc_item() else {
        warn!("[gather] serial space exhausted — cannot create resource");
        return Ok(());
    };

    let item = HeldItemInfo { serial, graphic, color, amount };
    let target = DropTarget::OnEntity { target_serial: bp_serial, x: 0xFFFF, y: 0xFFFF };
    let result = engine.drop_item(user_serial, item, target, None).await;

    match result {
        DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. } => {
            session.send(game_util::system_speech(
                &format!("You dig some {} and put it in your backpack.", name),
            )).await?;
            info!(
                "[gather] 0x{:08X} mined {}x {} (graphic={:#06X})",
                user_serial, amount, name, graphic,
            );
        }
        other => {
            warn!("[gather] unexpected drop result for resource: {:?}", other);
            session.send(game_util::system_message("Your backpack cannot hold any more.")).await?;
        }
    }

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// A validated gather source: its location plus enough identity for the
/// worker to re-validate it and key its resource-node state.
struct ResolvedSource {
    x: u16,
    y: u16,
    z: i8,
    /// The source tile/item graphic.
    graphic: u16,
    /// Resource-node item serial, or `0` for a static map tile.
    serial: u32,
}

/// Resolve the targeted gather source.
///
/// Returns [`ResolvedSource`] if it is valid for the tool: either a matching
/// static-tile graphic, or an item node carrying the `gather_resource` meta
/// matching the tool's kind.  Returns `None` otherwise.
///
/// Note: this is the *session-side* first pass.  The authoritative validation
/// (against the loaded map data) happens in the worker when the harvest
/// completes — see `complete_gather`.
async fn resolve_target_source(
    engine: &EngineProxy<DemoCommand>,
    tool: &ToolDef,
    tc: &TargetCursor,
) -> Option<ResolvedSource> {
    let target_serial = tc.target_serial & 0x7FFF_FFFF;

    // 1. Item node target — check the `gather_resource` meta.
    if target_serial != 0 {
        if let Some(props) = engine.get_item_props(target_serial).await {
            if props.get_meta_str(gathering::META_GATHER_RESOURCE) == Some(tool.kind.as_str()) {
                // Use the node's own coordinates/graphic if available, else the
                // cursor-reported position.
                if let Some(DemoEntity::Item { x, y, z, graphic, .. }) =
                    engine.get_entity(target_serial).await
                {
                    return Some(ResolvedSource { x, y, z, graphic, serial: target_serial });
                }
                return Some(ResolvedSource {
                    x: tc.x, y: tc.y, z: tc.z, graphic: tc.graphic, serial: target_serial,
                });
            }
        }
        // A non-resource entity was targeted — not valid.
        return None;
    }

    // 2. Static-tile target — validate the reported tile graphic.
    if tool.tile_is_valid(tc.graphic) {
        return Some(ResolvedSource {
            x: tc.x, y: tc.y, z: tc.z, graphic: tc.graphic, serial: 0,
        });
    }

    None
}

/// Resolve a clicked item's graphic (equipped → backpack → ground entity).
async fn resolve_item_graphic(
    engine: &EngineProxy<DemoCommand>,
    player_serial: u32,
    item_serial: u32,
) -> Option<u16> {
    // 1. In a container (backpack)?
    if let Some((_serial, graphic, _color, _amount)) = engine.find_item_info(item_serial).await {
        return Some(graphic);
    }

    // 2. Equipped on the player?
    if let Some(m) = engine.get_entity(player_serial).await.as_ref().and_then(|e| e.mobile()) {
        if let Some(eq) = m.items.iter().find(|eq| eq.serial == item_serial) {
            return Some(eq.graphic);
        }
    }

    // 3. As a standalone ground item entity?
    if let Some(DemoEntity::Item { graphic, .. }) = engine.get_entity(item_serial).await {
        return Some(graphic);
    }

    None
}
