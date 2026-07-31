//! Spellbook opening: double-click a spellbook in the backpack to display
//! the list of spells it contains.
//!
//! A spellbook is a *virtual* container — the spells it holds are not real
//! engine items.  When the player double-clicks the book we synthesize a
//! container view:
//!
//! 1. `AddItemToContainerLegacy` (0x25) — re-affirms the book's slot in the
//!    backpack (matches the reference client flow).
//! 2. `DrawContainer` (0x24) with `gump_model = 0xFFFF` — the special model
//!    the client maps to the spellbook gump.
//! 3. `ContainerContent` (0x3C) — one virtual "spell scroll" item per spell
//!    the book contains, with graphic `0x1F2C + spell_id` and `amount`
//!    equal to the spell number (the client reads the circle/page layout
//!    from these).
//!
//! The spell-item serials are synthetic (high range) so they never collide
//! with real item serials, and are deterministic per `(book, spell)` so the
//! same item always refers to the same spell.

use log::debug;

use protocol::RawPacket;
use packets::traits::BasicPacket;
use packets::interaction::DoubleClick;

use network::error;
use network::session::Session;

use crate::constants::item;
use crate::DemoWorkerTx;

use super::session_state::SessionContext;

// ── Constants ────────────────────────────────────────────────────────────

/// Special `gump_model` value that tells the client to draw the spellbook
/// gump rather than a normal container.
const SPELLBOOK_GUMP: u16 = 0xFFFF;

/// Base graphic for spell scroll items in a spellbook page.
///
/// `graphic = SPELL_GRAPHIC_BASE + spell_id` (spell IDs are 1-indexed).
const SPELL_GRAPHIC_BASE: u16 = 0x1F2C;

/// Number of spells a full Magery book holds (8 circles × 8 spells).
const SPELL_COUNT: u16 = 64;

/// High base for synthesized spell-item serials, ensuring no overlap with
/// real engine item serials.
const SPELL_SERIAL_BASE: u32 = 0xF000_0000;

// ── Double-click intercept ─────────────────────────────────────────────────

/// Check if a double-click packet targets a spellbook in the player's
/// backpack and, if so, send the spellbook container view.
///
/// Returns `true` if the packet was consumed.
pub(super) async fn handle_spellbook_double_click(
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

    // Paperdoll request (high bit) — not a spellbook.
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

    let (graphic, color, amount) = match item_info {
        Some((_serial, graphic, color, amount)) => (graphic, color, amount),
        None => return Ok(false), // not in a container — not our concern
    };

    if graphic != item::SPELLBOOK {
        return Ok(false);
    }

    // Only plain (color 0) books are Magery spellbooks.  Other hues are
    // reserved for future book types (e.g. runebooks).
    if color != 0 {
        return Ok(false);
    }

    // Find the container holding the book (its backpack) to re-add it.
    let container_serial = match engine.find_container_of_item(clean_serial).await {
        Some(cs) => cs,
        None => return Ok(false),
    };

    debug!(
        "[spellbook] 0x{:08X} opened spellbook 0x{:08X}",
        p.serial, clean_serial,
    );

    let version = ctx.infra.client_version;

    // 1. Re-affirm the book's slot in the backpack (0x25).
    session.send(common::spawn::build_add_item_to_container(
        clean_serial, graphic, amount.max(1), 44, 65,
        container_serial, color, 0, version,
    )).await?;

    // 2. Draw the spellbook gump (0x24, gump 0xFFFF).
    session.send(common::spawn::build_draw_container(clean_serial, SPELLBOOK_GUMP, version)).await?;

    // 3. Send the spell list as container content (0x3C).
    let spell_items: Vec<(u32, u16, u16, u16, u16, u32, u16)> = (1..=SPELL_COUNT)
        .map(|spell_id| (
            SPELL_SERIAL_BASE.wrapping_add(spell_id as u32),
            SPELL_GRAPHIC_BASE + spell_id,
            spell_id,
            1u16,
            1u16,
            clean_serial,
            0u16,
        ))
        .collect();
    session.send(common::spawn::build_container_content(&spell_items, version)).await?;

    Ok(true)
}
