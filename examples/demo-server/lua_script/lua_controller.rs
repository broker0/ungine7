//! Coroutine-based Lua anima that implements [`EntityController`].
//!
//! Each `LuaController` owns a Lua VM with the script loaded as a coroutine.
//! On every [`tick()`](EntityController::tick) the coroutine is resumed,
//! receiving synchronous access to the game world through [`ControlContext`].
//!
//! The script calls `sleep(ms)` or `wait_event(timeout_ms)` to yield
//! control back to the game loop.  All world operations (`step`,
//! `teleport`, `query_area`, etc.) execute synchronously within the
//! current tick — no RPC or channels involved.
//!
//! ## Example Lua script
//!
//! ```lua
//! local w = World()
//! while true do
//!     local me = w:get_entity(w:serial())
//!     if me then
//!         w:step(math.random(0, 7))
//!     end
//!     sleep(3000)
//! end
//! ```

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use log::{error, info, warn};
use mlua::prelude::*;

use framework::anima::context::ControlContext;
use framework::anima::traits::{AccessLevel, EntityController};
use framework::continuum::WorldEvent;
use framework::ecumene::tile_rect::TileRect;

use common::uo_engine::controller::{DemoControllerDef, GameCommand, EntityEvent};

use u_core::{Facing, Heading};

// ── Context pointer ──────────────────────────────────────────────────────
//
// During `tick()`, we store a raw pointer to `ControlContext` in Lua's
// app_data.  World methods read this pointer to call ControlContext
// methods synchronously.
//
// SAFETY: The pointer is valid for the entire duration of `tick()`.
// It is set before `resume()` and cleared immediately after.
// The Lua VM is single-threaded and never escapes `tick()`.

/// Wrapper for the raw pointer stored in Lua app_data.
struct CtxPtr(*mut ControlContext<'static>);

// SAFETY: The Lua VM is owned by LuaController and only accessed from
// tick(), which runs on the worker thread.  The pointer is never sent
// across threads.
unsafe impl Send for CtxPtr {}
unsafe impl Sync for CtxPtr {}

/// Helper — run a closure with the current ControlContext.
///
/// Returns `Err(LuaError)` if no context is active (called outside tick).
fn with_ctx<R>(lua: &Lua, f: impl FnOnce(&mut ControlContext) -> R) -> LuaResult<R> {
    let ptr = lua
        .app_data_ref::<CtxPtr>()
        .ok_or_else(|| LuaError::external("World methods can only be called during tick"))?;
    if ptr.0.is_null() {
        return Err(LuaError::external("World context is not active"));
    }
    // SAFETY: pointer is valid during tick(), and we hold exclusive
    // access (single-threaded Lua, no reentrance).
    let ctx = unsafe { &mut *ptr.0 };
    Ok(f(ctx))
}

// ── Yield markers ────────────────────────────────────────────────────────

/// Parsed yield value from the Lua coroutine.
enum YieldReason {
    /// `sleep(ms)` — resume after `ms` milliseconds.
    Sleep(u64),
    /// `wait_event(timeout_ms)` — resume when an event arrives or timeout.
    WaitEvent(u64),
    /// `wait_command(timeout_ms)` — resume when a command arrives or timeout.
    WaitCommand(u64),
    /// `wait_world_event(timeout_ms)` — resume when a world event arrives or timeout.
    WaitWorldEvent(u64),
    /// Coroutine finished or yielded without a recognized marker.
    None,
}

fn parse_yield(value: &LuaValue) -> YieldReason {
    if let LuaValue::Table(t) = value {
        if let Ok(kind) = t.get::<String>("__yield") {
            return match kind.as_str() {
                "sleep" => {
                    let ms = t.get::<u64>("ms").unwrap_or(0);
                    YieldReason::Sleep(ms)
                }
                "wait_event" => {
                    let ms = t.get::<u64>("timeout_ms").unwrap_or(5000);
                    YieldReason::WaitEvent(ms)
                }
                "wait_command" => {
                    let ms = t.get::<u64>("timeout_ms").unwrap_or(5000);
                    YieldReason::WaitCommand(ms)
                }
                "wait_world_event" => {
                    let ms = t.get::<u64>("timeout_ms").unwrap_or(5000);
                    YieldReason::WaitWorldEvent(ms)
                }
                _ => YieldReason::None,
            };
        }
    }
    YieldReason::None
}

// ── World userdata ───────────────────────────────────────────────────────

/// Lua userdata — handle to the game world.
///
/// Unlike the async `LuaWorld` in `runtime.rs`, this one calls
/// `ControlContext` methods synchronously through the raw pointer.
#[derive(Clone)]
struct LuaCtrlWorld;

impl LuaUserData for LuaCtrlWorld {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // ── serial() → number ─────────────────────────────────────
        methods.add_method("serial", |lua, _this, ()| {
            with_ctx(lua, |ctx| ctx.entity_serial)
        });

        // ── map_id() → number ─────────────────────────────────────
        methods.add_method("map_id", |lua, _this, ()| {
            with_ctx(lua, |ctx| ctx.map_id())
        });

        // ── get_entity(serial) → table | nil ──────────────────────
        methods.add_method("get_entity", |lua, _this, serial: u32| {
            let info = with_ctx(lua, |ctx| ctx.get_entity(serial))?;
            match info {
                Some(e) => {
                    let t = lua.create_table()?;
                    t.set("serial", e.serial)?;
                    t.set("x", e.pos.x)?;
                    t.set("y", e.pos.y)?;
                    t.set("z", e.pos.z)?;
                    t.set("graphic", e.graphic)?;
                    t.set("is_mobile", e.is_mobile)?;
                    t.set("is_multi", e.is_multi)?;
                    // Full mobile stats (matching async API).
                    if e.is_mobile {
                        t.set("type", "mobile")?;
                        if let Some(v) = e.hits { t.set("hits", v)?; }
                        if let Some(v) = e.hits_max { t.set("hits_max", v)?; }
                        if let Some(v) = e.mana { t.set("mana", v)?; }
                        if let Some(v) = e.mana_max { t.set("mana_max", v)?; }
                        if let Some(v) = e.stamina { t.set("stamina", v)?; }
                        if let Some(v) = e.stamina_max { t.set("stamina_max", v)?; }
                        if let Some(v) = e.notoriety { t.set("notoriety", v)?; }
                        if let Some(ref v) = e.name { t.set("name", v.as_str())?; }
                        if let Some(v) = e.direction { t.set("direction", v)?; }
                        t.set("is_mounted", e.is_mounted)?;
                        t.set("is_player", e.is_player)?;
                    } else if e.is_multi {
                        t.set("type", "multi")?;
                    } else {
                        t.set("type", "item")?;
                    }
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // ── step(direction) → table | nil ─────────────────────────
        // direction: 0-7 for walking, or 128+ (0x80 | heading) for running.
        methods.add_method("step", |lua, _this, dir: u8| {
            let running = dir & 0x80 != 0;
            let heading = match Heading::from_raw(dir & 0x07) {
                Some(h) => h,
                None => return Ok(LuaValue::Nil),
            };
            let facing = Facing::from_heading(heading).with_running(running);
            let result = with_ctx(lua, |ctx| ctx.step(facing))?;
            match result {
                Ok(pos) => {
                    let t = lua.create_table()?;
                    t.set("x", pos.x)?;
                    t.set("y", pos.y)?;
                    t.set("z", pos.z)?;
                    Ok(LuaValue::Table(t))
                }
                Err(_) => Ok(LuaValue::Nil),
            }
        });

        // ── teleport(x, y, z) ─────────────────────────────────────
        methods.add_method("teleport", |lua, _this, (x, y, z): (u16, u16, i8)| {
            let result = with_ctx(lua, |ctx| ctx.teleport(x, y, z))?;
            match result {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        });

        // ── query_area(x1, y1, x2, y2) → table ───────────────────
        methods.add_method("query_area", |lua, _this, (x1, y1, x2, y2): (u16, u16, u16, u16)| {
            let area = TileRect { x_min: x1, y_min: y1, x_max: x2, y_max: y2 };
            let entities = with_ctx(lua, |ctx| ctx.query_area(&area))?;
            let result = lua.create_table()?;
            for (i, e) in entities.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("serial", e.serial)?;
                t.set("x", e.pos.x)?;
                t.set("y", e.pos.y)?;
                t.set("z", e.pos.z)?;
                t.set("graphic", e.graphic)?;
                t.set("is_mobile", e.is_mobile)?;
                t.set("is_multi", e.is_multi)?;
                if e.is_mobile {
                    t.set("type", "mobile")?;
                    if let Some(v) = e.hits { t.set("hits", v)?; }
                    if let Some(v) = e.hits_max { t.set("hits_max", v)?; }
                    if let Some(v) = e.notoriety { t.set("notoriety", v)?; }
                    if let Some(ref v) = e.name { t.set("name", v.as_str())?; }
                } else if e.is_multi {
                    t.set("type", "multi")?;
                } else {
                    t.set("type", "item")?;
                }
                result.set(i + 1, t)?;
            }
            Ok(LuaValue::Table(result))
        });

        // ── test_step(x, y, z, direction) → number | nil ─────────
        methods.add_method("test_step", |lua, _this, (x, y, z, dir): (u16, u16, i8, u8)| {
            let heading = match Heading::from_raw(dir) {
                Some(h) => h,
                None => return Ok(LuaValue::Nil),
            };
            let result = with_ctx(lua, |ctx| ctx.test_step(x, y, z, heading))?;
            match result {
                Some(new_z) => Ok(LuaValue::Integer(new_z as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── resolve_z(x, y, z_hint, direction) → number | nil ────
        methods.add_method("resolve_z", |lua, _this, (x, y, z_hint, dir): (u16, u16, i8, u8)| {
            let heading = match Heading::from_raw(dir) {
                Some(h) => h,
                None => return Ok(LuaValue::Nil),
            };
            let result = with_ctx(lua, |ctx| ctx.resolve_standing_z(x, y, z_hint, heading))?;
            match result {
                Some(z) => Ok(LuaValue::Integer(z as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── has_los(x1, y1, z1, x2, y2, z2) → boolean ───────────
        methods.add_method("has_los", |lua, _this, (x1, y1, z1, x2, y2, z2): (u16, u16, i16, u16, u16, i16)| {
            let result = with_ctx(lua, |ctx| ctx.has_los(x1, y1, z1, x2, y2, z2))?;
            Ok(LuaValue::Boolean(result))
        });

        // ── play_sound(sound_id, x, y, z) ─────────────────────────
        methods.add_method("play_sound", |lua, _this, (sound_id, x, y, z): (u16, u16, u16, i16)| {
            with_ctx(lua, |ctx| ctx.play_sound(sound_id, x, y, z))
        });

        // ── effect(params_table) ──────────────────────────────────
        methods.add_method("effect", |lua, _this, p: super::params::EffectParams| {
            with_ctx(lua, |ctx| {
                ctx.play_effect(
                    p.direction_type, p.source_serial, p.target_serial, p.graphic,
                    p.x, p.y, p.z, p.target_x, p.target_y, p.target_z,
                    p.speed, p.duration, p.fixed_direction, p.explode,
                );
            })
        });

        // ── animate(serial, action, frame_count, opts) ────────────
        methods.add_method("animate", |lua, _this,
            (serial, action, frame_count, a): (u32, u16, u8, super::params::AnimateOpts)|
        {
            with_ctx(lua, |ctx| {
                let (x, y) = ctx.get_entity(serial)
                    .map(|e| (e.pos.x, e.pos.y))
                    .unwrap_or((0, 0));
                ctx.animate(serial, action, frame_count, a.repeat_count, a.reverse, a.repeat, a.frame_delay, x, y);
            })
        });

        // ── say(message, opts) ────────────────────────────────────
        // Uses the anima's own entity serial and looks up its info.
        methods.add_method("say", |lua, _this, (message, s): (String, super::params::SayOpts)| {
            with_ctx(lua, |ctx| {
                let serial = ctx.entity_serial;
                let (graphic, x, y) = ctx.me()
                    .map(|e| (e.graphic, e.pos.x, e.pos.y))
                    .unwrap_or((0, 0, 0));
                let name = s.name.unwrap_or_default();
                ctx.say(serial, graphic, s.speech_type, s.color, s.font, name, message, x, y);
            })
        });

        // ── deal_damage(serial, amount) → table | nil ─────────────
        // Returns { new_hits, killed } or nil on error.
        // source_serial is automatically set to the controller's entity.
        methods.add_method("deal_damage", |lua, _this, (serial, amount): (u32, u16)| {
            let result = with_ctx(lua, |ctx| ctx.deal_damage(serial, amount))?;
            match result {
                Ok((new_hits, killed)) => {
                    let t = lua.create_table()?;
                    t.set("new_hits", new_hits)?;
                    t.set("killed", killed)?;
                    Ok(LuaValue::Table(t))
                }
                Err(_) => Ok(LuaValue::Nil),
            }
        });

        // ── heal_entity(serial, amount) → number | nil ────────────
        methods.add_method("heal_entity", |lua, _this, (serial, amount): (u32, u16)| {
            let result = with_ctx(lua, |ctx| ctx.heal_entity(serial, amount))?;
            match result {
                Ok(new_hits) => Ok(LuaValue::Integer(new_hits as i64)),
                Err(_) => Ok(LuaValue::Nil),
            }
        });

        // ── modify_mana(serial, delta) → number | nil ─────────────
        methods.add_method("modify_mana", |lua, _this, (serial, delta): (u32, i32)| {
            let result = with_ctx(lua, |ctx| ctx.modify_mana(serial, delta))?;
            match result {
                Ok(new_mana) => Ok(LuaValue::Integer(new_mana as i64)),
                Err(_) => Ok(LuaValue::Nil),
            }
        });

        // ── modify_stamina(serial, delta) → number | nil ──────────
        methods.add_method("modify_stamina", |lua, _this, (serial, delta): (u32, i32)| {
            let result = with_ctx(lua, |ctx| ctx.modify_stamina(serial, delta))?;
            match result {
                Ok(new_stamina) => Ok(LuaValue::Integer(new_stamina as i64)),
                Err(_) => Ok(LuaValue::Nil),
            }
        });

        // ── face(direction) → boolean ─────────────────────────────
        // Turn the controlled entity to face a direction (0-7) without
        // stepping.  Uses step() internally — if the entity is already
        // facing that direction nothing happens.
        methods.add_method("face", |lua, _this, dir: u8| {
            let heading = match Heading::from_raw(dir & 0x07) {
                Some(h) => h,
                None => return Ok(false),
            };
            let facing = Facing::from_heading(heading);
            // step() does turn-in-place when heading differs from current.
            // If already facing that way, it moves — we need to check first.
            with_ctx(lua, |ctx| {
                let me = ctx.me();
                if me.is_none() { return false; }
                // We always call step — if heading differs it turns in place,
                // if same it moves. We handle both cases.
                match ctx.step(facing) {
                    Ok(_) => true,
                    Err(_) => false,
                }
            })
        });

        // ── send_gump(target_player, gump_id, x, y, layout, text_lines [, blocking]) ──
        methods.add_method("send_gump", |lua, _this,
            (target_player, gump_id, gump_x, gump_y, layout, text_lines, blocking):
            (u32, u32, u32, u32, String, Vec<String>, Option<bool>)|
        {
            let blocking = blocking.unwrap_or(false);
            with_ctx(lua, |ctx| {
                ctx.send_gump(target_player, gump_id, gump_x, gump_y, layout, text_lines, blocking);
            })
        });

        // ── send_message(target_player, message, color) ───────────────
        methods.add_method("send_message", |lua, _this,
            (target_player, message, color): (u32, String, u16)|
        {
            with_ctx(lua, |ctx| {
                ctx.send_message(target_player, &message, color);
            })
        });

        // ── close_gump(target_player, gump_id) ───────────────────────
        methods.add_method("close_gump", |lua, _this,
            (target_player, gump_id): (u32, u32)|
        {
            with_ctx(lua, |ctx| {
                ctx.close_gump(target_player, gump_id);
            })
        });

        // ── teleport_other(serial, x, y, z) → boolean ────────────────
        // Requires Full access level; returns false if denied.
        methods.add_method("teleport_other", |lua, _this,
            (serial, x, y, z): (u32, u16, u16, i8)|
        {
            with_ctx(lua, |ctx| {
                match ctx.teleport_other(serial, x, y, z) {
                    Ok(()) => true,
                    Err(_) => false,
                }
            })
        });

        // ── teleport_other_world(serial, map, x, y, z) → boolean ──────
        // Teleport a player to another world (map facet).  Hands the move
        // off to the player's session (controllers cannot perform a
        // worker-level cross-map transfer).  Requires Full access level;
        // returns false if denied.  Only player mobiles can be moved this
        // way — for NPCs use intra-world `teleport_other`.
        methods.add_method("teleport_other_world", |lua, _this,
            (serial, map, x, y, z): (u32, u8, u16, u16, i8)|
        {
            with_ctx(lua, |ctx| {
                match ctx.send_cross_world_teleport(serial, map, x, y, z) {
                    Ok(()) => true,
                    Err(_) => false,
                }
            })
        });

        // ── Inventory / equipment access ──────────────────────────────

        // ── get_backpack_serial(serial) → number | nil ────────────────
        methods.add_method("get_backpack_serial", |lua, _this, serial: u32| {
            let result = with_ctx(lua, |ctx| ctx.get_backpack_serial(serial))?;
            match result {
                Some(bp) => Ok(LuaValue::Integer(bp as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── find_item_in_container(container_serial, graphic) → serial, amount | nil ──
        methods.add_method("find_item_in_container", |lua, _this,
            (container_serial, graphic): (u32, u16)|
        {
            let result = with_ctx(lua, |ctx| ctx.find_item_in_container(container_serial, graphic))?;
            match result {
                Some((serial, amount)) => Ok(LuaValue::Table({
                    let t = lua.create_table()?;
                    t.set("serial", serial)?;
                    t.set("amount", amount)?;
                    t
                })),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── consume_mana(serial, amount) → number | nil ───────────────
        methods.add_method("consume_mana", |lua, _this, (serial, amount): (u32, u16)| {
            let result = with_ctx(lua, |ctx| ctx.consume_mana(serial, amount))?;
            match result {
                Ok(Some(new_mana)) => Ok(LuaValue::Integer(new_mana as i64)),
                _ => Ok(LuaValue::Nil),
            }
        });

        // ── consume_item(serial, amount, expected_graphic?) → table | nil ──
        methods.add_method("consume_item", |lua, _this,
            (serial, amount, expected_graphic): (u32, u16, Option<u16>)|
        {
            let result = with_ctx(lua, |ctx| ctx.consume_item(serial, amount, expected_graphic))?;
            match result {
                Ok(Some((remaining, graphic))) => Ok(LuaValue::Table({
                    let t = lua.create_table()?;
                    t.set("remaining", remaining)?;
                    t.set("graphic", graphic)?;
                    t
                })),
                _ => Ok(LuaValue::Nil),
            }
        });

        // ── send_target_cursor(target_player, cursor_id, cursor_type) ──
        methods.add_method("send_target_cursor", |lua, _this,
            (target_player, cursor_id, cursor_type): (u32, u32, u8)|
        {
            with_ctx(lua, |ctx| {
                ctx.send_target_cursor(target_player, cursor_id, cursor_type);
            })
        });

        // ── get_item_props(serial) → table | nil ─────────────────────
        methods.add_method("get_item_props", |lua, _this, serial: u32| {
            use common::uo_engine::item_props::{ItemProps, MetaValue};            let boxed = with_ctx(lua, |ctx| ctx.get_item_props_any(serial))?;
            let Some(boxed) = boxed else { return Ok(LuaValue::Nil) };
            let props = boxed.downcast::<ItemProps>()
                .map_err(|_| LuaError::external("item_props type mismatch"))?;
            let t = lua.create_table()?;
            match props.name() {
                Some(name) => t.set("name", name)?,
                None => t.set("name", LuaValue::Nil)?,
            }
            match props.weight_override {
                Some(w) => t.set("weight_override", w)?,
                None => t.set("weight_override", LuaValue::Nil)?,
            }
            let meta = lua.create_table()?;
            for (key, value) in &props.meta {
                match value {
                    MetaValue::Int(v) => meta.set(key.as_str(), *v)?,
                    MetaValue::Float(v) => meta.set(key.as_str(), *v)?,
                    MetaValue::Str(v) => meta.set(key.as_str(), v.as_str())?,
                    MetaValue::Bool(v) => meta.set(key.as_str(), *v)?,
                }
            }
            t.set("meta", meta)?;
            Ok(LuaValue::Table(t))
        });

        // ── set_item_props(serial, props_table | nil) ────────────────
        methods.add_method("set_item_props", |lua, _this, (serial, props_value): (u32, LuaValue)| {
            use common::uo_engine::item_props::{ItemProps, MetaValue, ObjectText};
            let props: Option<Box<dyn std::any::Any>> = match props_value {
                LuaValue::Nil => None,
                LuaValue::Table(t) => {
                    let name: Option<String> = t.get("name").ok();
                    let weight_override: Option<u16> = t.get("weight_override").ok();
                    let mut meta = std::collections::HashMap::new();
                    if let Ok(meta_table) = t.get::<LuaTable>("meta") {
                        for pair in meta_table.pairs::<String, LuaValue>() {
                            let (key, value) = pair?;
                            let mv = match value {
                                LuaValue::Integer(v) => MetaValue::Int(v),
                                LuaValue::Number(v) => {
                                    if v.fract() == 0.0 && v.abs() < i64::MAX as f64 {
                                        MetaValue::Int(v as i64)
                                    } else {
                                        MetaValue::Float(v)
                                    }
                                }
                                LuaValue::String(s) => MetaValue::Str(s.to_str()?.to_string()),
                                LuaValue::Boolean(b) => MetaValue::Bool(b),
                                _ => continue,
                            };
                            meta.insert(key, mv);
                        }
                    }
                    Some(Box::new(ItemProps {
                        text: name.map(|n| ObjectText::with_title(n)).unwrap_or_default(),
                        weight_override,
                        meta,
                    }))
                }
                _ => return Err(LuaError::external("set_item_props: expected table or nil")),
            };
            with_ctx(lua, |ctx| ctx.set_item_props_any(serial, props))
        });

        // ── Spell definitions ─────────────────────────────────────────

        // ── get_spell(spell_id) → table | nil ─────────────────────────
        // Returns a single spell definition by ID.
        methods.add_method("get_spell", |lua, _this, spell_id: u16| {
            match crate::magic::get_spell(spell_id) {
                Some(spell) => {
                    let t = lua.create_table()?;
                    t.set("id", spell.id)?;
                    t.set("name", spell.name)?;
                    t.set("mana", spell.mana)?;
                    t.set("damage_min", spell.damage_min)?;
                    t.set("damage_max", spell.damage_max)?;
                    t.set("heal_min", spell.heal_min)?;
                    t.set("heal_max", spell.heal_max)?;
                    t.set("circle", spell.circle)?;
                    t.set("cast_delay_ms", spell.cast_delay_ms)?;
                    t.set("scroll_cast_delay_ms", spell.scroll_cast_delay_ms)?;
                    t.set("needs_target", spell.needs_target)?;
                    t.set("can_self", spell.can_self)?;
                    t.set("harmful", spell.harmful)?;
                    t.set("words", spell.words)?;
                    t.set("cast_sound", spell.cast_sound)?;
                    t.set("impact_sound", spell.impact_sound)?;
                    t.set("cast_action", spell.cast_action)?;
                    t.set("projectile_graphic", spell.projectile_graphic)?;
                    t.set("target_effect", spell.target_effect)?;
                    t.set("target_effect_speed", spell.target_effect_speed)?;
                    t.set("target_effect_duration", spell.target_effect_duration)?;
                    t.set("lightning_bolt", spell.lightning_bolt)?;
                    t.set("scroll_graphic", spell.scroll_graphic)?;
                    let reagents = lua.create_table()?;
                    for (i, &r) in spell.reagents.iter().enumerate() {
                        reagents.set(i + 1, r)?;
                    }
                    t.set("reagents", reagents)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // ── get_all_spells() → { [spell_id] = { ... }, ... } ──────────
        // Returns all spell definitions from magic.rs as a Lua table
        // keyed by spell ID.  Does not require ControlContext.
        methods.add_method("get_all_spells", |lua, _this, ()| {
            let all = crate::magic::all_spells();
            let result = lua.create_table()?;
            for spell in all {
                let t = lua.create_table()?;
                t.set("id", spell.id)?;
                t.set("name", spell.name)?;
                t.set("mana", spell.mana)?;
                t.set("damage_min", spell.damage_min)?;
                t.set("damage_max", spell.damage_max)?;
                t.set("heal_min", spell.heal_min)?;
                t.set("heal_max", spell.heal_max)?;
                t.set("circle", spell.circle)?;
                t.set("cast_delay_ms", spell.cast_delay_ms)?;
                t.set("scroll_cast_delay_ms", spell.scroll_cast_delay_ms)?;
                t.set("needs_target", spell.needs_target)?;
                t.set("can_self", spell.can_self)?;
                t.set("harmful", spell.harmful)?;
                t.set("words", spell.words)?;
                t.set("cast_sound", spell.cast_sound)?;
                t.set("impact_sound", spell.impact_sound)?;
                t.set("cast_action", spell.cast_action)?;
                t.set("projectile_graphic", spell.projectile_graphic)?;
                t.set("target_effect", spell.target_effect)?;
                t.set("target_effect_speed", spell.target_effect_speed)?;
                t.set("target_effect_duration", spell.target_effect_duration)?;
                t.set("lightning_bolt", spell.lightning_bolt)?;
                t.set("scroll_graphic", spell.scroll_graphic)?;
                let reagents = lua.create_table()?;
                for (i, &r) in spell.reagents.iter().enumerate() {
                    reagents.set(i + 1, r)?;
                }
                t.set("reagents", reagents)?;
                result.set(spell.id, t)?;
            }
            Ok(LuaValue::Table(result))
        });

        // ── World event subscription ──────────────────────────────────

        // subscribe_world_events(radius) — subscribe to world events
        // within a Chebyshev radius around this entity.
        methods.add_method("subscribe_world_events", |lua, _this, radius: u16| {
            with_ctx(lua, |ctx| ctx.subscribe_world_events(radius))
        });

        // unsubscribe_world_events() — remove world event subscription.
        methods.add_method("unsubscribe_world_events", |lua, _this, ()| {
            with_ctx(lua, |ctx| ctx.unsubscribe_world_events())
        });

        // remove_entity(serial) → boolean — remove an entity from the zone.
        // Requires Full access level.
        methods.add_method("remove_entity", |lua, _this, serial: u32| {
            let result = with_ctx(lua, |ctx| ctx.remove_entity(serial))?;
            Ok(result.is_ok())
        });
    }
}

// ── Event buffer ─────────────────────────────────────────────────────────

/// Events queued for the Lua script to poll.
struct EventBuffer {
    events: std::collections::VecDeque<EntityEvent>,
}

impl EventBuffer {
    fn new() -> Self {
        Self {
            events: std::collections::VecDeque::new(),
        }
    }

    fn push(&mut self, event: EntityEvent) {
        // Cap buffer to prevent unbounded growth.
        if self.events.len() >= 256 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn pop(&mut self) -> Option<EntityEvent> {
        self.events.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

// ── Command buffer ──────────────────────────────────────────────────────

/// Commands queued via `on_command()` for the Lua script to poll.
struct CommandBuffer {
    commands: std::collections::VecDeque<GameCommand>,
}

impl CommandBuffer {
    fn new() -> Self {
        Self {
            commands: std::collections::VecDeque::new(),
        }
    }

    fn push(&mut self, cmd: GameCommand) {
        if self.commands.len() >= 256 {
            self.commands.pop_front();
        }
        self.commands.push_back(cmd);
    }

    fn pop(&mut self) -> Option<GameCommand> {
        self.commands.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

// ── World event buffer ─────────────────────────────────────────────────

/// World events buffered via `on_world_event()` for the Lua script to poll.
struct WorldEventBuffer {
    events: std::collections::VecDeque<Arc<WorldEvent>>,
}

impl WorldEventBuffer {
    fn new() -> Self {
        Self {
            events: std::collections::VecDeque::new(),
        }
    }

    fn push(&mut self, event: Arc<WorldEvent>) {
        if self.events.len() >= 256 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn pop(&mut self) -> Option<Arc<WorldEvent>> {
        self.events.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

// ── Access level annotation parsing ──────────────────────────────────────

/// Parse the `---@access <level>` annotation from the first lines of a
/// Lua script source.  Returns [`AccessLevel::Safe`] if no annotation is
/// found.
///
/// Recognized annotations (case-insensitive value):
///   `---@access full`  → `AccessLevel::Full`
///   `---@access safe`  → `AccessLevel::Safe`
fn parse_access_annotation(source: &str) -> AccessLevel {
    for line in source.lines().take(10) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("---@access") {
            let value = rest.trim();
            return match value.to_ascii_lowercase().as_str() {
                "full" => AccessLevel::Full,
                _ => AccessLevel::Safe,
            };
        }
        // Stop scanning after the first non-comment, non-empty line.
        if !trimmed.is_empty() && !trimmed.starts_with("--") {
            break;
        }
    }
    AccessLevel::Safe
}

// ── LuaController ────────────────────────────────────────────────────────

/// Coroutine-based Lua script running as an [`EntityController`].
///
/// Created via [`LuaController::from_file`].  The Lua VM is owned by the
/// anima and all world access goes through [`ControlContext`].
pub struct LuaController {
    lua: Lua,
    coroutine_key: LuaRegistryKey,
    script_name: String,

    /// Access level parsed from the script's `---@access` annotation.
    access_level: AccessLevel,

    /// Time at which to wake from `sleep()`.
    wake_at: Option<Instant>,

    /// Whether the script is waiting for an event via `wait_event()`.
    waiting_for_event: bool,
    /// Timeout for `wait_event()`.
    event_timeout: Option<Instant>,

    /// Events queued for `poll_event()` / `wait_event()`.
    event_buffer: RefCell<EventBuffer>,

    /// Commands queued for `poll_command()` / `wait_command()`.
    command_buffer: RefCell<CommandBuffer>,

    /// World events queued for `poll_world_event()` / `wait_world_event()`.
    world_event_buffer: RefCell<WorldEventBuffer>,

    /// Whether the script is waiting for a command via `wait_command()`.
    waiting_for_command: bool,
    /// Timeout for `wait_command()`.
    command_timeout: Option<Instant>,

    /// Whether the script is waiting for a world event via `wait_world_event()`.
    waiting_for_world_event: bool,
    /// Timeout for `wait_world_event()`.
    world_event_timeout: Option<Instant>,

    /// Whether the coroutine has finished (dead).
    finished: bool,
}

impl LuaController {
    /// Load a Lua script from a file and create a anima.
    ///
    /// If `scripts_dir` is provided, `package.path` is configured so that
    /// `require("lib.foo")` resolves to `{scripts_dir}/lib/foo.lua`, and
    /// a `SCRIPTS_DIR` global is set for use with `dofile()`.
    pub fn from_file(path: &Path, scripts_dir: Option<&Path>) -> Result<Self, LuaError> {
        let script_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());

        info!("[lua-ctrl:{}] loading script", script_name);

        let source = std::fs::read_to_string(path)
            .map_err(|e| LuaError::external(format!("failed to read {}: {e}", path.display())))?;

        // Parse access level from a `---@access full` annotation in the
        // first few lines of the script.
        let access_level = parse_access_annotation(&source);
        if access_level != AccessLevel::Safe {
            info!("[lua-ctrl:{}] access level: {:?}", script_name, access_level);
        }

        let lua = Lua::new();

        // Set instruction limit to prevent infinite loops from blocking
        // the game loop.  1 million instructions per resume should be
        // more than enough for any NPC tick.
        lua.set_hook(
            LuaHookTriggers::new().every_nth_instruction(1_000_000),
            |_lua, _debug| {
                Err(LuaError::external(
                    "script exceeded instruction limit (possible infinite loop)",
                ))
            },
        );

        // Initialize the context pointer as null.
        lua.set_app_data(CtxPtr(std::ptr::null_mut()));

        // Set up package.path and SCRIPTS_DIR for require() / dofile().
        if let Some(sd) = scripts_dir {
            let sd_str = sd.to_string_lossy();
            // Use forward slashes for Lua compatibility on all platforms.
            let sd_lua = sd_str.replace('\\', "/");
            lua.load(format!(
                r#"
                package.path = "{0}/?.lua;{0}/?/init.lua;" .. package.path
                SCRIPTS_DIR = "{0}"
                "#,
                sd_lua,
            )).exec().map_err(|e| {
                LuaError::external(format!("failed to set package.path: {e}"))
            })?;
        }

        // Register globals.
        Self::register_globals(&lua, &script_name)?;

        // Create the coroutine from the script source.
        let func = lua.load(&source).set_name(&script_name).into_function()?;
        let thread = lua.create_thread(func)?;
        let coroutine_key = lua.create_registry_value(thread)?;

        Ok(Self {
            lua,
            coroutine_key,
            script_name,
            access_level,
            wake_at: None,
            waiting_for_event: false,
            event_timeout: None,
            event_buffer: RefCell::new(EventBuffer::new()),
            command_buffer: RefCell::new(CommandBuffer::new()),
            world_event_buffer: RefCell::new(WorldEventBuffer::new()),
            waiting_for_command: false,
            command_timeout: None,
            waiting_for_world_event: false,
            world_event_timeout: None,
            finished: false,
        })
    }

    /// Register global functions available to the script.
    fn register_globals(lua: &Lua, script_name: &str) -> LuaResult<()> {
        let globals = lua.globals();

        // World() constructor — returns a LuaCtrlWorld handle.
        let world_ctor = lua.create_function(|_lua, ()| {
            Ok(LuaCtrlWorld)
        })?;
        globals.set("World", world_ctor)?;

        // log(msg) — logging.
        let name = script_name.to_string();
        let log_fn = lua.create_function(move |_lua, msg: String| {
            info!("[lua-ctrl:{}] {}", name, msg);
            Ok(())
        })?;
        globals.set("log", log_fn)?;

        // clock() → number — monotonic time in seconds.
        //
        // Returns elapsed seconds since the controller was created.
        // Uses tokio::time::Instant which respects tokio::time::pause(),
        // allowing deterministic fast-forward in headless tests.
        let epoch = Instant::now();
        let clock_fn = lua.create_function(move |_, ()| {
            let elapsed = epoch.elapsed();
            Ok(elapsed.as_secs_f64())
        })?;
        globals.set("clock", clock_fn)?;

        // sleep(ms) and wait_event(timeout_ms) must be pure Lua functions
        // (not Rust callbacks) because they call coroutine.yield, and
        // Lua 5.4 forbids yielding across a C-call boundary.
        //
        // poll_event() remains a Rust callback — it never yields.

        // poll_event() → table | nil — non-blocking event poll.
        let poll_fn = lua.create_function(|lua, ()| {
            // Access the event buffer through Lua app_data.
            // The buffer is stored as a raw pointer to our RefCell<EventBuffer>.
            let ptr = lua
                .app_data_ref::<EventBufPtr>()
                .ok_or_else(|| LuaError::external("event buffer not available"))?;
            if ptr.0.is_null() {
                return Ok(LuaValue::Nil);
            }
            // SAFETY: valid during tick(), single-threaded.
            let buf = unsafe { &*ptr.0 };
            let mut buf = buf.borrow_mut();
            match buf.pop() {
                Some(event) => event.into_lua(lua),
                None => Ok(LuaValue::Nil),
            }
        })?;
        globals.set("poll_event", poll_fn)?;

        // sleep(ms) — pure Lua: yields with a marker table.
        lua.load(r#"
            function sleep(ms)
                coroutine.yield({ __yield = "sleep", ms = ms })
            end
        "#).exec()?;

        // wait_event(timeout_ms) — pure Lua: checks buffer first via
        // poll_event() (Rust), then yields if nothing available.
        lua.load(r#"
            function wait_event(timeout_ms)
                local ev = poll_event()
                if ev then return ev end
                return coroutine.yield({ __yield = "wait_event", timeout_ms = timeout_ms })
            end
        "#).exec()?;

        // poll_command() → table | nil — non-blocking command poll.
        let poll_cmd_fn = lua.create_function(|lua, ()| {
            let ptr = lua
                .app_data_ref::<CommandBufPtr>()
                .ok_or_else(|| LuaError::external("command buffer not available"))?;
            if ptr.0.is_null() {
                return Ok(LuaValue::Nil);
            }
            // SAFETY: valid during tick(), single-threaded.
            let buf = unsafe { &*ptr.0 };
            let mut buf = buf.borrow_mut();
            match buf.pop() {
                Some(cmd) => cmd.into_lua(lua),
                None => Ok(LuaValue::Nil),
            }
        })?;
        globals.set("poll_command", poll_cmd_fn)?;

        // wait_command(timeout_ms) — pure Lua: checks buffer first via
        // poll_command() (Rust), then yields if nothing available.
        lua.load(r#"
            function wait_command(timeout_ms)
                local cmd = poll_command()
                if cmd then return cmd end
                return coroutine.yield({ __yield = "wait_command", timeout_ms = timeout_ms })
            end
        "#).exec()?;

        // poll_world_event() → table | nil — non-blocking world event poll.
        let poll_we_fn = lua.create_function(|lua, ()| {
            let ptr = lua
                .app_data_ref::<WorldEventBufPtr>()
                .ok_or_else(|| LuaError::external("world event buffer not available"))?;
            if ptr.0.is_null() {
                return Ok(LuaValue::Nil);
            }
            // SAFETY: valid during tick(), single-threaded.
            let buf = unsafe { &*ptr.0 };
            let mut buf = buf.borrow_mut();
            match buf.pop() {
                Some(event) => {
                    let tbl = super::runtime::world_event_to_lua(lua, &event)?;
                    Ok(LuaValue::Table(tbl))
                }
                None => Ok(LuaValue::Nil),
            }
        })?;
        globals.set("poll_world_event", poll_we_fn)?;

        // wait_world_event(timeout_ms) — pure Lua: checks buffer first,
        // then yields if nothing available.
        lua.load(r#"
            function wait_world_event(timeout_ms)
                local ev = poll_world_event()
                if ev then return ev end
                return coroutine.yield({ __yield = "wait_world_event", timeout_ms = timeout_ms })
            end
        "#).exec()?;

        Ok(())
    }

    /// Resume the Lua coroutine with the given argument.
    fn resume_coroutine(&mut self, arg: LuaValue) {
        let thread: LuaThread = match self.lua.registry_value(&self.coroutine_key) {
            Ok(t) => t,
            Err(e) => {
                error!("[lua-ctrl:{}] failed to get coroutine: {}", self.script_name, e);
                self.finished = true;
                return;
            }
        };

        if thread.status() != LuaThreadStatus::Resumable {
            self.finished = true;
            return;
        }

        match thread.resume::<LuaValue>(arg) {
            Ok(value) => {
                // Check if coroutine finished (returned, not yielded).
                if thread.status() != LuaThreadStatus::Resumable {
                    info!("[lua-ctrl:{}] script finished", self.script_name);
                    self.finished = true;
                    return;
                }

                // Parse yield reason.
                match parse_yield(&value) {
                    YieldReason::Sleep(ms) => {
                        self.wake_at = Some(Instant::now() + Duration::from_millis(ms));
                    }
                    YieldReason::WaitEvent(timeout_ms) => {
                        self.waiting_for_event = true;
                        self.event_timeout =
                            Some(Instant::now() + Duration::from_millis(timeout_ms));
                    }
                    YieldReason::WaitCommand(timeout_ms) => {
                        self.waiting_for_command = true;
                        self.command_timeout =
                            Some(Instant::now() + Duration::from_millis(timeout_ms));
                    }
                    YieldReason::WaitWorldEvent(timeout_ms) => {
                        self.waiting_for_world_event = true;
                        self.world_event_timeout =
                            Some(Instant::now() + Duration::from_millis(timeout_ms));
                    }
                    YieldReason::None => {
                        // Unknown yield — treat as immediate resume next tick.
                    }
                }
            }
            Err(e) => {
                error!("[lua-ctrl:{}] script error: {}", self.script_name, e);
                self.finished = true;
            }
        }
    }
}

// ── Event buffer pointer ─────────────────────────────────────────────────

/// Raw pointer to the event buffer, stored in Lua app_data during tick().
struct EventBufPtr(*const RefCell<EventBuffer>);

unsafe impl Send for EventBufPtr {}
unsafe impl Sync for EventBufPtr {}

/// Raw pointer to the command buffer, stored in Lua app_data during tick().
struct CommandBufPtr(*const RefCell<CommandBuffer>);

unsafe impl Send for CommandBufPtr {}
unsafe impl Sync for CommandBufPtr {}

/// Raw pointer to the world event buffer, stored in Lua app_data during tick().
struct WorldEventBufPtr(*const RefCell<WorldEventBuffer>);

unsafe impl Send for WorldEventBufPtr {}
unsafe impl Sync for WorldEventBufPtr {}

// ── EntityController impl ────────────────────────────────────────────────

impl EntityController<DemoControllerDef> for LuaController {
    fn tick(&mut self, ctx: &mut ControlContext, _dt: Duration) {
        if self.finished {
            return;
        }

        let now = Instant::now();

        // Check sleep condition.
        if let Some(wake_at) = self.wake_at {
            if now < wake_at {
                return; // still sleeping
            }
            self.wake_at = None;
        }

        // Check wait_event condition.
        let resume_arg = if self.waiting_for_event {
            if !self.event_buffer.borrow().is_empty() {
                // Event available — resume with it.
                self.waiting_for_event = false;
                self.event_timeout = None;
                let event = self.event_buffer.borrow_mut().pop().unwrap();
                match event.into_lua(&self.lua) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("[lua-ctrl:{}] failed to convert event: {}", self.script_name, e);
                        LuaValue::Nil
                    }
                }
            } else if let Some(timeout) = self.event_timeout {
                if now >= timeout {
                    // Timed out — resume with nil.
                    self.waiting_for_event = false;
                    self.event_timeout = None;
                    LuaValue::Nil
                } else {
                    return; // still waiting
                }
            } else {
                return; // waiting indefinitely
            }
        } else if self.waiting_for_command {
            // Check wait_command condition.
            if !self.command_buffer.borrow().is_empty() {
                self.waiting_for_command = false;
                self.command_timeout = None;
                let cmd = self.command_buffer.borrow_mut().pop().unwrap();
                match cmd.into_lua(&self.lua) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("[lua-ctrl:{}] failed to convert command: {}", self.script_name, e);
                        LuaValue::Nil
                    }
                }
            } else if let Some(timeout) = self.command_timeout {
                if now >= timeout {
                    self.waiting_for_command = false;
                    self.command_timeout = None;
                    LuaValue::Nil
                } else {
                    return; // still waiting
                }
            } else {
                return; // waiting indefinitely
            }
        } else if self.waiting_for_world_event {
            // Check wait_world_event condition.
            if !self.world_event_buffer.borrow().is_empty() {
                self.waiting_for_world_event = false;
                self.world_event_timeout = None;
                let we = self.world_event_buffer.borrow_mut().pop().unwrap();
                match super::runtime::world_event_to_lua(&self.lua, &we) {
                    Ok(tbl) => LuaValue::Table(tbl),
                    Err(e) => {
                        warn!("[lua-ctrl:{}] failed to convert world event: {}", self.script_name, e);
                        LuaValue::Nil
                    }
                }
            } else if let Some(timeout) = self.world_event_timeout {
                if now >= timeout {
                    self.waiting_for_world_event = false;
                    self.world_event_timeout = None;
                    LuaValue::Nil
                } else {
                    return; // still waiting
                }
            } else {
                return; // waiting indefinitely
            }
        } else {
            LuaValue::Nil
        };

        // Set the ControlContext pointer for World methods.
        // SAFETY: We cast ctx to 'static lifetime.  This is safe because:
        // 1. The pointer is only used during this tick() call
        // 2. We clear it before returning
        // 3. The Lua VM is single-threaded and never escapes this call
        let ctx_ptr = ctx as *mut ControlContext as *mut ControlContext<'static>;
        self.lua.set_app_data(CtxPtr(ctx_ptr));

        // Set the event and command buffer pointers.
        let buf_ptr = &self.event_buffer as *const RefCell<EventBuffer>;
        self.lua.set_app_data(EventBufPtr(buf_ptr));
        let cmd_ptr = &self.command_buffer as *const RefCell<CommandBuffer>;
        self.lua.set_app_data(CommandBufPtr(cmd_ptr));
        let we_ptr = &self.world_event_buffer as *const RefCell<WorldEventBuffer>;
        self.lua.set_app_data(WorldEventBufPtr(we_ptr));

        // Resume the coroutine.
        self.resume_coroutine(resume_arg);

        // Clear pointers — no longer valid after tick().
        self.lua.set_app_data(CtxPtr(std::ptr::null_mut()));
        self.lua.set_app_data(EventBufPtr(std::ptr::null()));
        self.lua.set_app_data(CommandBufPtr(std::ptr::null()));
        self.lua.set_app_data(WorldEventBufPtr(std::ptr::null()));
    }

    fn on_event(&mut self, _ctx: &mut ControlContext, event: EntityEvent) {
        self.event_buffer.borrow_mut().push(event);
    }

    fn on_command(&mut self, _ctx: &mut ControlContext, cmd: GameCommand) {
        self.command_buffer.borrow_mut().push(cmd);
    }

    fn on_world_event(&mut self, _ctx: &mut ControlContext, event: &Arc<WorldEvent>) {
        self.world_event_buffer.borrow_mut().push(Arc::clone(event));
    }

    fn access_level(&self) -> AccessLevel {
        self.access_level
    }

    fn name(&self) -> &str {
        &self.script_name
    }

    fn next_tick_at(&self) -> Option<Instant> {
        // Buffered event/command/world event already waiting — need immediate
        // tick so the coroutine is resumed without waiting for the full timeout.
        if self.waiting_for_event && !self.event_buffer.borrow().is_empty() {
            return Some(Instant::now());
        }
        if self.waiting_for_command && !self.command_buffer.borrow().is_empty() {
            return Some(Instant::now());
        }
        if self.waiting_for_world_event && !self.world_event_buffer.borrow().is_empty() {
            return Some(Instant::now());
        }

        let deadline = [
            self.wake_at,
            self.event_timeout,
            self.command_timeout,
            self.world_event_timeout,
        ]
            .into_iter()
            .flatten()
            .min();

        // If no deadline is set but the coroutine is alive and not blocked
        // on an event/command/world_event wait — it needs an immediate tick.
        // This covers two cases:
        //   1. First tick after attach (coroutine never resumed yet).
        //   2. Coroutine yielded without calling sleep() (YieldReason::None).
        if deadline.is_none()
            && !self.finished
            && !self.waiting_for_event
            && !self.waiting_for_command
            && !self.waiting_for_world_event
        {
            return Some(Instant::now());
        }

        deadline
    }
}
