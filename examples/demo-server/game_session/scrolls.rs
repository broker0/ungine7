//! Spell scroll casting: double-click a spell scroll in the backpack to
//! initiate a spell cast from that scroll.
//!
//! Flow:
//! 1. Player double-clicks a scroll item in backpack.
//! 2. The scroll graphic is matched to a [`SpellDef`](crate::magic::SpellDef) via [`get_spell_by_scroll`](crate::magic::get_spell_by_scroll).
//! 3. If the spell needs a target, a target cursor is shown and a
//!    [`PendingSpell`](crate::magic::PendingSpell) (with `scroll_item_serial`) is stored — the existing
//!    spell-target handling in `spells.rs` takes over from there.
//! 4. If the spell is self-targetable (e.g. Heal), a target cursor is still
//!    shown (matching original UO behaviour).
//! 5. On successful completion, one scroll is consumed from the item stack
//!    (handled by [`complete_cast`](crate::magic::complete_cast) via `scroll_item_serial`).
//! 6. Scroll casts do not consume mana.

use log::debug;

use protocol::RawPacket;
use packets::traits::BasicPacket;

use network::error;
use network::session::Session;

use packets::interaction::DoubleClick;

use crate::actions;
use crate::game_util;
use crate::magic;
use crate::DemoWorkerTx;

use super::pending_cursor::PendingCursor;
use super::session_state::SessionContext;

// ── Double-click intercept ───────────────────────────────────────────────

/// Check if a double-click packet targets a spell scroll.
///
/// If it does, verify the scroll is in the player's containers (backpack),
/// check action-slot blocking, then show a target cursor and populate
/// `ctx.pending_spell` (reusing the existing spell-target flow).
///
/// Returns `true` if the packet was consumed (regardless of success).
pub(super) async fn handle_scroll_double_click(
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

    // Paperdoll request (high bit) — not a scroll.
    if dc.serial & 0x8000_0000 != 0 {
        return Ok(false);
    }

    let clean_serial = dc.serial & 0x7FFF_FFFF;

    let p = match &ctx.infra.player {
        Some(p) => p,
        None => return Ok(false),
    };

    // Look up the item in container stores (backpack / nested containers).
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let item_info = engine.find_item_info(clean_serial).await;

    let graphic = match &item_info {
        Some((_serial, graphic, _color, _amount)) => *graphic,
        None => return Ok(false), // Not in a container — not our concern.
    };

    // Match graphic to a spell definition.
    let spell_def = match magic::get_spell_by_scroll(graphic) {
        Some(s) => s,
        None => return Ok(false), // Not a known spell scroll.
    };

    debug!(
        "[scroll] 0x{:08X} double-clicked scroll 0x{:04X} ({}) serial=0x{:08X}",
        p.serial, graphic, spell_def.name, clean_serial,
    );

    // Check action-slot blocking before showing cursor.
    let has_pending = ctx.has_pending_cursor();
    let has_blocking = ctx.has_blocking_gump();
    if let Err(msg) = actions::can_begin_cast(
        &ctx.active_cast, &ctx.active_skill, &ctx.active_bandage, has_pending, has_blocking,
    ) {
        session.send(game_util::system_message(msg)).await?;
        return Ok(true);
    }

    // Show target cursor (reuse spell-target flow).
    // Even self-targetable spells (Heal, Greater Heal) show a cursor,
    // matching original UO behaviour.
    let (ps, target_pkt) = magic::begin_spell_target(
        spell_def,
        p.serial,
        Some(clean_serial), // scroll item serial
    );
    ctx.infra.pending_cursor = Some(PendingCursor::from_spell(&ps));
    session.send(target_pkt).await?;

    Ok(true)
}
