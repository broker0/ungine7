//! Admin spawner object: a GM-visible, hidden world item that defines a
//! dynamic monster spawn point and is configured live via a GUMP.
//!
//! ## Design
//!
//! Unlike the static spawn points in [`crate::spawn_points`], a spawner is a
//! real world entity ([`DemoEntity::Item`](common::uo_engine::entity::DemoEntity::Item) with `hidden: true`, so only GM+
//! observers see it).  All of its parameters live in the entity's
//! `item_props.meta` so they survive `.save`/`.load`.
//!
//! The [`SpawnerController`] attached to the object is **only a UI layer**:
//! a controller cannot create entities (see [`crate::spawn_points`] module
//! docs).  The actual population logic runs inside
//! [`SpawnManager::tick`](crate::spawn_points::SpawnManager::tick), which
//! scans the zone for objects carrying [`META_SPAWN_TEMPLATE`] and keeps
//! `max_count` monsters alive around each one.
//!
//! ## Responsibilities of the controller
//!
//! * On [`UsedBy`](EntityEvent::UsedBy) (double-click) → open the config GUMP.
//!   Access gating (GameMaster+) happens at the session layer
//!   (`interaction.rs`) before the `UseObject` is forwarded, and the object
//!   is `hidden` so regular players never see it.
//! * On [`GumpResponse`](EntityEvent::GumpResponse) → mutate the object's
//!   meta (`+/-` buttons, template cycling, enable toggle) or flag it for
//!   deletion ([`META_SPAWN_DELETE`]).

use std::time::Duration;

use framework::anima::{ControlContext, EntityController};

use common::uo_engine::controller::{EntityEvent, DemoControllerDef};
use common::uo_engine::item_props::{ItemProps, MetaValue};

// ── Meta keys ───────────────────────────────────────────────────────────────

/// Template key (string) the spawner produces.  Presence of this key marks an
/// entity as a spawner for [`SpawnManager::tick`](crate::spawn_points::SpawnManager::tick).
pub const META_SPAWN_TEMPLATE: &str = "spawn_template";
/// Maximum number of live monsters to keep (int).
pub const META_SPAWN_MAX: &str = "spawn_max";
/// Scatter radius in tiles (int).
pub const META_SPAWN_RADIUS: &str = "spawn_radius";
/// Respawn delay in milliseconds (int).
pub const META_SPAWN_DELAY: &str = "spawn_delay";
/// Whether the spawner is active (bool).
pub const META_SPAWN_ENABLED: &str = "spawn_enabled";
/// When set to `true`, the next [`SpawnManager::tick`](crate::spawn_points::SpawnManager::tick) removes this object.
pub const META_SPAWN_DELETE: &str = "spawn_delete";

// ── Defaults & limits ────────────────────────────────────────────────────────

/// Graphic for the spawner object — a bloody pentagram (decorative, obvious).
pub const SPAWNER_GRAPHIC: u16 = 0x1183;

pub const DEFAULT_MAX: i64 = 2;
pub const DEFAULT_RADIUS: i64 = 6;
pub const DEFAULT_DELAY_MS: i64 = 15_000;

const MAX_MIN: i64 = 1;
const MAX_MAX: i64 = 20;
const RADIUS_MIN: i64 = 0;
const RADIUS_MAX: i64 = 24;
const DELAY_MIN: i64 = 1_000;
const DELAY_MAX: i64 = 300_000;
const DELAY_STEP: i64 = 1_000;

/// GUMP id used for the spawner config dialog.
pub const SPAWNER_GUMP_ID: u32 = 0x5350_4157; // "SPAW"

// ── GUMP button ids ───────────────────────────────────────────────────────

const BTN_CLOSE: u32 = 0;
const BTN_MAX_DEC: u32 = 1;
const BTN_MAX_INC: u32 = 2;
const BTN_RADIUS_DEC: u32 = 3;
const BTN_RADIUS_INC: u32 = 4;
const BTN_DELAY_DEC: u32 = 5;
const BTN_DELAY_INC: u32 = 6;
const BTN_ENABLED: u32 = 8;
const BTN_DELETE: u32 = 9;

// ── Default meta for a freshly placed spawner ───────────────────────────────

/// Build the default [`ItemProps`] for a new spawner producing `template`.
///
/// Writes the controller id so the controller is restored after `.load`,
/// plus all spawner parameters at their defaults.
pub fn default_props(template: &str) -> ItemProps {
    let mut props = ItemProps::with_name(&format!("[Spawner] {}", template));
    props.set_meta("controller", MetaValue::Str("spawner".to_string()));
    props.set_meta(META_SPAWN_TEMPLATE, MetaValue::Str(template.to_string()));
    props.set_meta(META_SPAWN_MAX, MetaValue::Int(DEFAULT_MAX));
    props.set_meta(META_SPAWN_RADIUS, MetaValue::Int(DEFAULT_RADIUS));
    props.set_meta(META_SPAWN_DELAY, MetaValue::Int(DEFAULT_DELAY_MS));
    props.set_meta(META_SPAWN_ENABLED, MetaValue::Bool(true));
    props
}

// ── Parameter view ──────────────────────────────────────────────────────────

/// Parsed spawner parameters read from an entity's [`ItemProps`].
#[derive(Debug, Clone)]
pub struct SpawnerParams {
    pub template: String,
    pub max_count: u8,
    pub radius: u16,
    pub respawn_delay_ms: u64,
    pub enabled: bool,
}

impl SpawnerParams {
    /// Read parameters from item props, applying defaults for missing keys.
    /// Returns `None` if the props are not a spawner (no template key).
    pub fn from_props(props: &ItemProps) -> Option<Self> {
        let template = props.get_meta_str(META_SPAWN_TEMPLATE)?.to_string();
        let max_count = props.get_meta_int(META_SPAWN_MAX).unwrap_or(DEFAULT_MAX)
            .clamp(MAX_MIN, MAX_MAX) as u8;
        let radius = props.get_meta_int(META_SPAWN_RADIUS).unwrap_or(DEFAULT_RADIUS)
            .clamp(RADIUS_MIN, RADIUS_MAX) as u16;
        let respawn_delay_ms = props.get_meta_int(META_SPAWN_DELAY).unwrap_or(DEFAULT_DELAY_MS)
            .clamp(DELAY_MIN, DELAY_MAX) as u64;
        let enabled = matches!(props.get_meta(META_SPAWN_ENABLED), Some(MetaValue::Bool(true)))
            || props.get_meta(META_SPAWN_ENABLED).is_none();
        Some(Self { template, max_count, radius, respawn_delay_ms, enabled })
    }

    /// Is this spawner flagged for deletion?
    pub fn is_delete_flagged(props: &ItemProps) -> bool {
        matches!(props.get_meta(META_SPAWN_DELETE), Some(MetaValue::Bool(true)))
    }
}

// ── SpawnerController ─────────────────────────────────────────────────────────

/// UI controller for an admin spawner object.  Holds no persistent state —
/// everything lives in the entity's `item_props.meta`.
pub struct SpawnerController;

impl SpawnerController {
    pub fn new() -> Self {
        Self
    }

    /// Read this entity's props as a clone, or `None` if absent / not a spawner.
    fn read_props(ctx: &ControlContext) -> Option<ItemProps> {
        let boxed = ctx.get_item_props_any(ctx.entity_serial)?;
        boxed.downcast::<ItemProps>().ok().map(|b| *b)
    }

    /// Write props back to this entity.
    fn write_props(ctx: &mut ControlContext, props: ItemProps) {
        ctx.set_item_props_any(ctx.entity_serial, Some(Box::new(props)));
    }

    /// Build the GUMP layout + text lines for the given parameters.
    fn build_gump(params: &SpawnerParams) -> (String, Vec<String>) {
        // Layout: a resizable background, a title, one labelled row per
        // parameter with `-`/`+` buttons, an enable toggle, Delete, and
        // Close.  The template is fixed at placement time (`.spawner`), so
        // it is shown read-only.
        //
        // GUMP layout numbers must be DECIMAL.  Button gump art (these are
        // decimal art ids that the project's other gumps use successfully):
        //   55 / 56     — small square minus / plus buttons (parameter steps)
        //   4014 / 4015 — generic button (enable toggle, Close)
        //   4017 / 4019 — X button (Delete)
        let layout = "\
            { page 0 }\
            { resizepic 0 0 2600 300 240 }\
            { text 20 12 1153 0 }\
            { text 20 45 996 1 }{ text 175 45 1153 2 }\
            { button 245 45 55 55 1 0 1 }{ button 270 45 56 56 1 0 2 }\
            { text 20 75 996 3 }{ text 175 75 1153 4 }\
            { button 245 75 55 55 1 0 3 }{ button 270 75 56 56 1 0 4 }\
            { text 20 105 996 5 }{ text 175 105 1153 6 }\
            { button 245 105 55 55 1 0 5 }{ button 270 105 56 56 1 0 6 }\
            { text 20 135 996 7 }{ text 175 135 1153 8 }\
            { text 20 165 996 9 }{ text 175 165 1153 10 }\
            { button 270 165 4014 4015 1 0 8 }\
            { button 20 205 4017 4019 1 0 9 }{ text 55 207 996 11 }\
            { button 200 205 4014 4015 1 0 0 }{ text 235 207 996 12 }"
            .to_string();

        let text_lines = vec![
            "Spawner Configuration".to_string(),               // 0 title
            "Max count:".to_string(),                          // 1
            params.max_count.to_string(),                      // 2
            "Radius:".to_string(),                             // 3
            params.radius.to_string(),                         // 4
            "Respawn (ms):".to_string(),                       // 5
            params.respawn_delay_ms.to_string(),               // 6
            "Template:".to_string(),                           // 7
            params.template.clone(),                           // 8
            "Enabled:".to_string(),                            // 9
            if params.enabled { "yes".to_string() } else { "no".to_string() }, // 10
            "Delete".to_string(),                              // 11
            "Close".to_string(),                               // 12
        ];

        (layout, text_lines)
    }

    /// Open the configuration GUMP for `player_serial`.
    fn open_gump(&self, ctx: &mut ControlContext, player_serial: u32) {
        let Some(props) = Self::read_props(ctx) else { return };
        let Some(params) = SpawnerParams::from_props(&props) else { return };
        let (layout, text_lines) = Self::build_gump(&params);
        ctx.send_gump(player_serial, SPAWNER_GUMP_ID, 100, 100, layout, text_lines, false);
    }

    /// Apply a button press, mutate meta, and re-open the GUMP (unless closed
    /// or deleted).
    fn handle_button(&self, ctx: &mut ControlContext, player_serial: u32, button_id: u32) {
        if button_id == BTN_CLOSE {
            return;
        }

        let Some(mut props) = Self::read_props(ctx) else { return };
        let Some(params) = SpawnerParams::from_props(&props) else { return };

        match button_id {
            BTN_MAX_DEC => set_int(&mut props, META_SPAWN_MAX, params.max_count as i64 - 1, MAX_MIN, MAX_MAX),
            BTN_MAX_INC => set_int(&mut props, META_SPAWN_MAX, params.max_count as i64 + 1, MAX_MIN, MAX_MAX),
            BTN_RADIUS_DEC => set_int(&mut props, META_SPAWN_RADIUS, params.radius as i64 - 1, RADIUS_MIN, RADIUS_MAX),
            BTN_RADIUS_INC => set_int(&mut props, META_SPAWN_RADIUS, params.radius as i64 + 1, RADIUS_MIN, RADIUS_MAX),
            BTN_DELAY_DEC => set_int(&mut props, META_SPAWN_DELAY, params.respawn_delay_ms as i64 - DELAY_STEP, DELAY_MIN, DELAY_MAX),
            BTN_DELAY_INC => set_int(&mut props, META_SPAWN_DELAY, params.respawn_delay_ms as i64 + DELAY_STEP, DELAY_MIN, DELAY_MAX),
            BTN_ENABLED => {
                props.set_meta(META_SPAWN_ENABLED, MetaValue::Bool(!params.enabled));
            }
            BTN_DELETE => {
                props.set_meta(META_SPAWN_DELETE, MetaValue::Bool(true));
                Self::write_props(ctx, props);
                ctx.close_gump(player_serial, SPAWNER_GUMP_ID);
                return;
            }
            _ => return,
        }

        Self::write_props(ctx, props);
        // Re-open the gump so the player sees the updated values.
        self.open_gump(ctx, player_serial);
    }
}

impl Default for SpawnerController {
    fn default() -> Self {
        Self::new()
    }
}

/// Clamp `value` into `[lo, hi]` and store it as an `Int` meta entry.
fn set_int(props: &mut ItemProps, key: &str, value: i64, lo: i64, hi: i64) {
    props.set_meta(key, MetaValue::Int(value.clamp(lo, hi)));
}

impl EntityController<DemoControllerDef> for SpawnerController {
    fn tick(&mut self, _ctx: &mut ControlContext, _dt: Duration) {
        // No periodic work — spawning is driven by SpawnManager::tick.
    }

    fn on_event(&mut self, ctx: &mut ControlContext, event: EntityEvent) {
        match event {
            EntityEvent::UsedBy { player_serial } => {
                self.open_gump(ctx, player_serial);
            }
            EntityEvent::GumpResponse { player_serial, gump_id, button_id, .. } => {
                if gump_id != SPAWNER_GUMP_ID {
                    return;
                }
                self.handle_button(ctx, player_serial, button_id);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "spawner"
    }

    fn next_tick_at(&self) -> Option<tokio::time::Instant> {
        None
    }
}
