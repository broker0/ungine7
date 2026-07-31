//! Concrete [`ControllerDef`] for the UO engine.
//!
//! Defines the type family (`DemoControllerDef`) that binds entity events,
//! global events, and commands used by all UO anima implementations.

use u_core::Heading;
use framework::anima::{ControllerDef, ControlContext, ControllerError};

// ── ControllerDef ──────────────────────────────────────────────────────────

/// The UO-specific anima definition.
///
/// Binds [`EntityEvent`], [`DemoGameEvent`], and [`GameCommand`] into a single
/// type family consumed by [`framework::anima::ControllerHost`].
pub struct DemoControllerDef;

impl ControllerDef for DemoControllerDef {
    type Event = EntityEvent;
    type GlobalEvent = DemoGameEvent;
    type Command = GameCommand;

    fn timer_event(_entity_serial: u32, timer_id: u64) -> EntityEvent {
        EntityEvent::TimerFired { timer_id }
    }
}

// ── Events ─────────────────────────────────────────────────────────────────

/// A text entry from a gump response.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
pub struct GumpTextEntry {
    /// Entry ID (matches the `textentry` command's entry_id in the gump layout).
    pub entry_id: u16,
    /// Text entered by the player.
    pub text: String,
}

/// Event directed at a specific entity's anima.
#[derive(Debug)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
#[cfg_attr(feature = "lua", lua(tag = "type", rename_all = "snake_case"))]
pub enum EntityEvent {
    /// The entity moved (step completed successfully).
    Moved {
        #[cfg_attr(feature = "lua", lua(into_via = "u8::from"))]
        direction: Heading,
        x: u16,
        y: u16,
        z: i8,
    },
    /// A scheduler timer fired.
    TimerFired {
        timer_id: u64,
    },
    /// The entity took damage from a source.
    DamageReceived {
        source_serial: u32,
        amount: u16,
    },
    /// A spell hit this entity.
    SpellHit {
        source_serial: u32,
        spell_id: u16,
    },

    // ── Interaction events (for per-object scripted controllers) ──────

    /// A player used (double-clicked) this entity.
    UsedBy {
        player_serial: u32,
    },
    /// A player responded to a gump opened by this entity's controller.
    GumpResponse {
        player_serial: u32,
        gump_id: u32,
        button_id: u32,
        switches: Vec<u32>,
        text_entries: Vec<GumpTextEntry>,
    },
    /// A player dropped an item onto this entity.
    ItemDroppedOn {
        player_serial: u32,
        item_serial: u32,
        item_graphic: u16,
    },
    /// A mobile stepped onto the tile occupied by this entity.
    ///
    /// Delivered to item controllers when a mobile (player or NPC) moves
    /// onto the same tile.  Use cases: teleporters, traps, pressure
    /// plates, trigger zones.
    SteppedOnBy {
        mobile_serial: u32,
    },
}

/// Global zone-wide event, broadcast to all controllers.
#[derive(Debug, Clone)]
pub enum DemoGameEvent {
    /// The zone was reset / reloaded.
    ZoneReset,
}

// ── Commands ───────────────────────────────────────────────────────────────

/// External command sent to an entity's anima.
#[derive(Debug)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
#[cfg_attr(feature = "lua", lua(tag = "type", rename_all = "snake_case"))]
pub enum GameCommand {
    /// Step in a direction (from client MoveRequest or pathfinder).
    Move {
        #[cfg_attr(feature = "lua", lua(into_via = "u8::from"))]
        direction: Heading,
        running: bool,
    },
    /// Cast a spell (from client 0x12 CastSpell or 0xBF CastTargetedSpell).
    CastSpell {
        spell_id: u16,
        /// Target serial (0 = needs targeting cursor, >0 = pre-targeted).
        target_serial: u32,
    },
    /// Use a skill (from client 0x12 UseSkill).
    UseSkill {
        skill_id: u16,
    },
    /// Attack a target (from client 0x05 AttackRequest).
    Attack {
        target_serial: u32,
    },
    /// Cancel current attack target.
    CancelAttack,
    /// Toggle war/peace mode (from client 0x72 WarMode).
    ToggleWarMode {
        fighting: bool,
    },
    /// Response to a target cursor (from client 0x6C TargetCursor).
    TargetResponse {
        cursor_id: u32,
        target_serial: u32,
        x: u16,
        y: u16,
        z: i16,
    },
}

// ── Helper: execute Move command ───────────────────────────────────────────

/// Process a [`GameCommand::Move`] by stepping the entity via the context.
///
/// Returns `Ok(pos)` on success, `Err` if blocked or access denied.
/// This is a free function so that any anima can reuse the logic.
pub fn execute_move(
    ctx: &mut ControlContext,
    direction: Heading,
) -> Result<u_core::Pos3D, ControllerError> {
    ctx.step(u_core::Facing::from_heading(direction))
}
