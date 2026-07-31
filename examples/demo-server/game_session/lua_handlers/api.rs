//! Lua API for per-session scripts: engine, session, broadcast.
//!
//! Reuses patterns from `lua_script/runtime.rs` but adapted for
//! per-session use: events come from an mpsc channel (not broadcast),
//! and actions are sent back to the session via another mpsc channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use log::info;
use mlua::prelude::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use framework::continuum::WorldEvent;
use u_core::{Facing, Heading};

use common::uo_engine::entity::{DemoEntity, MobileData};
use common::uo_engine::item_props::{ItemProps, MetaValue, ObjectText};
use common::uo_engine::rpc::EngineProxy;

use crate::{DemoCommand, DemoWorkerTx};
use super::{SessionLuaEvent, LuaSessionAction};

// ── Engine userdata ───────────────────────────────────────────────────────

/// Lua `engine` object — async RPC to the game engine.
/// Reuses the same patterns as `LuaWorld` in `runtime.rs`.
#[derive(Clone)]
struct LuaEngine {
    engine: EngineProxy<DemoCommand>,
}

impl LuaUserData for LuaEngine {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // get_entity(serial) → table | nil
        methods.add_async_method("get_entity", |lua, this, serial: u32| async move {
            let entity = this.engine.get_entity(serial).await;
            match entity {
                Some(e) => entity_to_lua(&lua, &e).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });

        // step(serial, direction) → table | nil
        methods.add_async_method("step", |lua, this, (serial, dir): (u32, u8)| async move {
            let running = dir & 0x80 != 0;
            let heading = match Heading::from_raw(dir & 0x07) {
                Some(h) => h,
                None => return Ok(LuaValue::Nil),
            };
            let facing = Facing::from_heading(heading).with_running(running);
            let result = this.engine.mobile_step(serial, facing).await;
            match result {
                Some(r) => {
                    let t = lua.create_table()?;
                    t.set("x", r.x)?;
                    t.set("y", r.y)?;
                    t.set("z", r.z)?;
                    t.set("direction", r.direction)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // teleport(serial, x, y, z)
        methods.add_async_method("teleport", |_lua, this, (serial, x, y, z): (u32, u16, u16, i8)| async move {
            this.engine.teleport(serial, x, y, z, None).await;
            Ok(())
        });

        // query_area(x1, y1, x2, y2) → table
        methods.add_async_method("query_area", |lua, this, (x1, y1, x2, y2): (u16, u16, u16, u16)| async move {
            use framework::ecumene::TileRect;
            let area = TileRect { x_min: x1, y_min: y1, x_max: x2, y_max: y2 };
            let entities = this.engine.query_area(area).await;
            let result = lua.create_table()?;
            for (i, e) in entities.iter().enumerate() {
                let t = entity_to_lua(&lua, e)?;
                result.set(i + 1, t)?;
            }
            Ok(LuaValue::Table(result))
        });

        // test_step(x, y, z, direction) → number | nil
        methods.add_async_method("test_step", |_lua, this, (x, y, z, dir): (u16, u16, i8, u8)| async move {
            let heading = match Heading::from_raw(dir) {
                Some(h) => h,
                None => return Ok(LuaValue::Nil),
            };
            let result = this.engine.validate_step(x, y, z, heading).await;
            match result {
                Some(new_z) => Ok(LuaValue::Integer(new_z as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // resolve_z(x, y, z_hint, direction) → number | nil
        methods.add_async_method("resolve_z", |_lua, this, (x, y, z_hint, dir): (u16, u16, i8, u8)| async move {
            let heading = match Heading::from_raw(dir) {
                Some(h) => h,
                None => return Ok(LuaValue::Nil),
            };
            let result = this.engine.resolve_z(x, y, z_hint, heading).await;
            match result {
                Some(z) => Ok(LuaValue::Integer(z as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // has_los(x1, y1, z1, x2, y2, z2) → boolean
        methods.add_async_method("has_los", |_lua, this, (x1, y1, z1, x2, y2, z2): (u16, u16, i16, u16, u16, i16)| async move {
            let result = this.engine.check_los(x1, y1, z1, x2, y2, z2).await;
            Ok(LuaValue::Boolean(result))
        });

        // deal_damage(serial, amount, source_serial?) → table | nil
        methods.add_async_method("deal_damage", |lua, this, (serial, amount, source): (u32, u16, Option<u32>)| async move {
            let source_serial = source.unwrap_or(0);
            let result = this.engine.deal_damage(serial, amount, source_serial).await;
            match result {
                Some(dr) => {
                    let t = lua.create_table()?;
                    t.set("new_hits", dr.new_hp)?;
                    t.set("killed", dr.killed)?;
                    if let Some(ref kill) = dr.kill {
                        t.set("corpse_serial", kill.corpse_serial)?;
                    }
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // heal_entity(serial, amount) → number | nil
        methods.add_async_method("heal_entity", |_lua, this, (serial, amount): (u32, u16)| async move {
            let result = this.engine.heal(serial, amount).await;
            match result {
                Some(new_hits) => Ok(LuaValue::Integer(new_hits as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // modify_mana(serial, delta) → number | nil
        methods.add_async_method("modify_mana", |_lua, this, (serial, delta): (u32, i32)| async move {
            let result = this.engine.modify_mana(serial, delta).await;
            match result {
                Some(new_mana) => Ok(LuaValue::Integer(new_mana as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // modify_stamina(serial, delta) → number | nil
        methods.add_async_method("modify_stamina", |_lua, this, (serial, delta): (u32, i32)| async move {
            let result = this.engine.modify_stamina(serial, delta).await;
            match result {
                Some(new_stamina) => Ok(LuaValue::Integer(new_stamina as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // consume_mana(serial, amount) → number | nil
        methods.add_async_method("consume_mana", |_lua, this, (serial, amount): (u32, u16)| async move {
            let result = this.engine.consume_mana(serial, amount).await;
            match result {
                Some(new_mana) => Ok(LuaValue::Integer(new_mana as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // get_weight(serial) → table | nil
        methods.add_async_method("get_weight", |lua, this, serial: u32| async move {
            let result = this.engine.compute_weight(serial, None).await;
            match result {
                Some((current, max)) => {
                    let t = lua.create_table()?;
                    t.set("current", current)?;
                    t.set("max", max)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // is_mounted(serial) → boolean
        methods.add_async_method("is_mounted", |_lua, this, serial: u32| async move {
            let entity = this.engine.get_entity(serial).await;
            let mounted = entity.as_ref()
                .and_then(|e| e.mobile())
                .map(|m| m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount))
                .unwrap_or(false);
            Ok(LuaValue::Boolean(mounted))
        });

        // get_spell(spell_id) → table | nil
        methods.add_method("get_spell", |lua, _this, spell_id: u16| {
            match crate::magic::get_spell(spell_id) {
                Some(spell) => spell_def_to_lua_table(lua, spell).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });

        // get_spell_by_scroll(graphic) → table | nil
        // Look up a spell definition by its scroll item graphic.
        methods.add_method("get_spell_by_scroll", |lua, _this, graphic: u16| {
            log::debug!("[lua-api] get_spell_by_scroll(graphic={:#06X})", graphic);
            match crate::magic::get_spell_by_scroll(graphic) {
                Some(spell) => {
                    log::debug!("[lua-api] get_spell_by_scroll -> {} (id={})", spell.name, spell.id);
                    spell_def_to_lua_table(lua, spell).map(LuaValue::Table)
                }
                None => {
                    log::debug!("[lua-api] get_spell_by_scroll -> nil (no match for {:#06X})", graphic);
                    Ok(LuaValue::Nil)
                },
            }
        });

        // get_all_spells() → { [spell_id] = { ... }, ... }
        // Returns all spell definitions as a Lua table keyed by spell ID.
        methods.add_method("get_all_spells", |lua, _this, ()| {
            let all = crate::magic::all_spells();
            let result = lua.create_table()?;
            for spell in all {
                let t = spell_def_to_lua_table(lua, spell)?;
                result.set(spell.id, t)?;
            }
            Ok(LuaValue::Table(result))
        });

        // spawn_npc(params) → serial
        methods.add_async_method("spawn_npc", |_lua, this, params: LuaTable| async move {
            use packets::mobile_flags::MobileFlags;
            use packets::movement::Notoriety;

            let serial = this.engine.allocate_mobile_serial().await;
            if serial == 0 {
                return Err(LuaError::external("mobile serial space exhausted"));
            }

            let graphic: u16 = params.get("graphic").unwrap_or(crate::constants::body::MALE_HUMAN);
            let x: u16 = params.get("x").unwrap_or(0);
            let y: u16 = params.get("y").unwrap_or(0);
            let z: i8 = params.get("z").unwrap_or(0);
            let name: String = params.get("name").unwrap_or_else(|_| "NPC".to_string());
            let color: u16 = params.get("color").unwrap_or(0);
            let direction: u8 = params.get("direction").unwrap_or(0);
            let notoriety_val: u8 = params.get("notoriety").unwrap_or(1);
            let hits: u16 = params.get("hits").unwrap_or(100);
            let hits_max: u16 = params.get("hits_max").unwrap_or(100);

            let notoriety = crate::lua_script::runtime::notoriety_from_u8(notoriety_val);
            let noto_class = match notoriety {
                Notoriety::Innocent => common::uo_engine::notoriety::NotorietyClass::Innocent,
                Notoriety::Criminal => common::uo_engine::notoriety::NotorietyClass::Criminal,
                Notoriety::Murderer => common::uo_engine::notoriety::NotorietyClass::Murderer,
                Notoriety::Enemy => common::uo_engine::notoriety::NotorietyClass::Enemy,
                _ => common::uo_engine::notoriety::NotorietyClass::Neutral,
            };

            let entity = DemoEntity::Mobile(MobileData {
                serial,
                graphic,
                x,
                y,
                z,
                direction,
                color,
                status: MobileFlags(0),
                notoriety,
                items: Vec::new(),
                name,
                hits,
                hits_max,
                mana: hits_max,
                mana_max: hits_max,
                stamina: hits_max,
                stamina_max: hits_max,
                str_: 50,
                dex: 50,
                int: 50,
                is_player: false,
                dead: false,
                living_graphic: 0,
                noto_class,
                ..Default::default()
            });

            this.engine.spawn_entity(serial, entity).await;
            Ok(serial)
        });

        // remove_entity(serial)
        methods.add_async_method("remove_entity", |_lua, this, serial: u32| async move {
            this.engine.remove_entity(serial).await;
            Ok(())
        });

        // get_container(serial) → table | nil
        // Returns { serial, gump_id, items: [{ serial, graphic, amount, color, x, y }] }
        methods.add_async_method("get_container", |lua, this, serial: u32| async move {
            let info = this.engine.get_container(serial).await;
            match info {
                Some(ci) => {
                    let t = lua.create_table()?;
                    t.set("serial", ci.serial)?;
                    t.set("gump_id", ci.gump_model)?;

                    let items_table = lua.create_table()?;
                    for (idx, item) in ci.items.iter().enumerate() {
                        let it = lua.create_table()?;
                        it.set("serial", item.serial)?;
                        it.set("graphic", item.graphic)?;
                        it.set("amount", item.amount)?;
                        it.set("color", item.color)?;
                        it.set("x", item.x)?;
                        it.set("y", item.y)?;
                        it.set("container_serial", ci.serial)?;
                        items_table.set(idx + 1, it)?;
                    }
                    t.set("items", items_table)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // consume_item(serial, amount?, expected_graphic?) → table | nil
        // Returns { remaining, graphic, was_ground } or nil if item not found / wrong graphic.
        methods.add_async_method("consume_item", |lua, this, (serial, amount, expected_graphic): (u32, Option<u16>, Option<u16>)| async move {
            let amount = amount.unwrap_or(1);
            let result = this.engine.consume_item(
                serial, amount, expected_graphic,
            ).await;
            match result {
                Some(cr) => {
                    let t = lua.create_table()?;
                    t.set("remaining", cr.remaining)?;
                    t.set("graphic", cr.graphic)?;
                    t.set("was_ground", cr.was_ground_item)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // find_item_info(serial) → table | nil
        // Searches entities, equipment, and containers for the given item serial.
        // Returns { container_serial, graphic, color, amount } or nil if not found.
        methods.add_async_method("find_item_info", |lua, this, serial: u32| async move {
            let result = this.engine.find_item_info(serial).await;
            match result {
                Some((container_serial, graphic, color, amount)) => {
                    let t = lua.create_table()?;
                    t.set("container_serial", container_serial)?;
                    t.set("graphic", graphic)?;
                    t.set("color", color)?;
                    t.set("amount", amount)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // equip_on_mobile(mobile_serial, item) → boolean
        // Equip an item on a mobile (e.g. mount on Layer::Mount).
        // item: { serial, graphic, layer (wire byte), color? }
        methods.add_async_method("equip_on_mobile", |_lua, this, (mobile_serial, item_table): (u32, LuaTable)| async move {
            let serial: u32 = item_table.get("serial")?;
            let graphic: u16 = item_table.get("graphic")?;
            let layer_byte: u8 = item_table.get("layer")?;
            let color: Option<u16> = item_table.get("color").ok();

            let layer = packets::layer::Layer::from_wire(layer_byte);
            let item = packets::world::EquippedItem {
                serial,
                graphic,
                layer,
                color,
            };
            let ok = this.engine.equip_on_mobile(mobile_serial, item).await;
            Ok(LuaValue::Boolean(ok))
        });

        // unequip_from_mobile(mobile_serial, item_serial) → table | nil
        // Unequip an item from a mobile by item serial.
        // Returns the removed item { serial, graphic, layer, color } or nil.
        methods.add_async_method("unequip_from_mobile", |lua, this, (mobile_serial, item_serial): (u32, u32)| async move {
            let result = this.engine.unequip_from_mobile(mobile_serial, item_serial).await;
            match result {
                Some(eq) => {
                    let t = lua.create_table()?;
                    t.set("serial", eq.serial)?;
                    t.set("graphic", eq.graphic)?;
                    t.set("layer", eq.layer.to_wire())?;
                    t.set("color", eq.color.unwrap_or(0))?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // get_item_props(serial) → table | nil
        // Returns { name?, weight_override?, meta: { key = value, ... } } or nil.
        // meta values: integers, floats, strings, booleans (auto-typed).
        methods.add_async_method("get_item_props", |lua, this, serial: u32| async move {
            let props = this.engine.get_item_props(serial).await;
            match props {
                Some(p) => item_props_to_lua(&lua, &p).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });

        // set_item_props(serial, props_table | nil)
        // Sets or removes item properties.
        // props_table: { name?: string, weight_override?: number, meta?: { key = value, ... } }
        // Pass nil to remove all properties for this serial.
        methods.add_async_method("set_item_props", |_lua, this, (serial, props_value): (u32, LuaValue)| async move {
            let props = match props_value {
                LuaValue::Nil => None,
                LuaValue::Table(t) => Some(lua_to_item_props(&t)?),
                _ => return Err(LuaError::external("set_item_props: expected table or nil")),
            };
            this.engine.set_item_props(serial, props).await;
            Ok(())
        });

        // allocate_serial() → number
        // Allocate a fresh unique serial from the engine.
        methods.add_async_method("allocate_serial", |_lua, this, ()| async move {
            let serial = this.engine.allocate_serial().await;
            Ok(serial)
        });
    }
}

// ── Session userdata ──────────────────────────────────────────────────────

/// Lua `session` object — typed client interaction methods.
///
/// All S→C packets are assembled by Rust — Lua never touches raw bytes.
#[derive(Clone)]
struct LuaSession {
    player_serial: u32,
    map_id: u8,
    action_tx: mpsc::Sender<LuaSessionAction>,
    engine: EngineProxy<DemoCommand>,
    /// Shared flag: `true` when a blocking gump is open.
    blocking_gump_flag: Arc<AtomicBool>,
}

impl LuaUserData for LuaSession {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // ── Query methods ────────────────────────────────────────────

        // player_serial() → number
        methods.add_method("player_serial", |_lua, this, ()| {
            Ok(this.player_serial)
        });

        // map_id() → number
        methods.add_method("map_id", |_lua, this, ()| {
            Ok(this.map_id)
        });

        // has_blocking_gump() → boolean
        // Returns true if a blocking gump (e.g. travel stone menu) is
        // currently open, preventing spells and skills.
        methods.add_method("has_blocking_gump", |_lua, this, ()| {
            Ok(this.blocking_gump_flag.load(Ordering::Relaxed))
        });

        // player_position() → table { x, y, z, direction, map_id } | nil
        methods.add_async_method("player_position", |lua, this, ()| async move {
            let entity = this.engine.get_entity(this.player_serial).await;
            match entity.as_ref().and_then(|e| e.mobile()) {
                Some(m) => {
                    let t = lua.create_table()?;
                    t.set("x", m.x)?;
                    t.set("y", m.y)?;
                    t.set("z", m.z)?;
                    t.set("direction", m.direction)?;
                    t.set("map_id", this.map_id)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // ── Combat packets ───────────────────────────────────────────

        // send_war_mode(fighting: bool)
        methods.add_method("send_war_mode", |_lua, this, fighting: bool| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendWarMode { fighting });
            Ok(())
        });

        // send_attack_response(target_serial: number)
        // Pass 0 to cancel (refuse) the attack.
        methods.add_method("send_attack_response", |_lua, this, target_serial: u32| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendAttackResponse { target_serial });
            Ok(())
        });

        // send_fight_occurring(attacker: number, defender: number)
        methods.add_method("send_fight_occurring", |_lua, this, (attacker, defender): (u32, u32)| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendFightOccurring { attacker, defender });
            Ok(())
        });

        // ── Targeting ────────────────────────────────────────────────

        // send_target_cursor(cursor_id: number, cursor_type: number)
        // cursor_type: 0 = neutral, 1 = harmful (red), 2 = helpful (blue)
        methods.add_method("send_target_cursor", |_lua, this, (cursor_id, cursor_type): (u32, u8)| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendTargetCursor { cursor_id, cursor_type });
            Ok(())
        });

        // cancel_target(cursor_id: number)
        methods.add_method("cancel_target", |_lua, this, cursor_id: u32| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendCancelTarget { cursor_id });
            Ok(())
        });

        // ── Messages ─────────────────────────────────────────────────

        // send_system_message(message: string)
        // Red system message in the bottom-left corner.
        methods.add_method("send_system_message", |_lua, this, message: String| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendSystemMessage { message });
            Ok(())
        });

        // send_overhead_message(serial: number, message: string, color?: number)
        // Overhead speech bubble on an entity (visible only to this client).
        methods.add_method("send_overhead_message", |_lua, this, (serial, message, color): (u32, String, Option<u16>)| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendOverheadMessage {
                serial,
                message,
                color: color.unwrap_or(0x03B2),
            });
            Ok(())
        });

        // send_unicode_speech(params: table)
        // Full control over UnicodeSpeech (0xAE) — for heal feedback "+25" etc.
        // params: { serial, graphic, color, font, name, message }
        methods.add_method("send_unicode_speech", |_lua, this, params: LuaTable| {
            let serial: u32 = params.get("serial").unwrap_or(0);
            let graphic: u16 = params.get("graphic").unwrap_or(0);
            let color: u16 = params.get("color").unwrap_or(0x03B2);
            let font: u16 = params.get("font").unwrap_or(3);
            let name: String = params.get("name").unwrap_or_default();
            let message: String = params.get("message").unwrap_or_default();
            let _ = this.action_tx.try_send(LuaSessionAction::SendUnicodeSpeech {
                serial, graphic, color, font, name, message,
            });
            Ok(())
        });

        // ── Spell effects ────────────────────────────────────────────

        // send_fizzle(serial: number, x: number, y: number, z: number, message?: string)
        // Fizzle effect: system text + sound + visual (3 packets).
        methods.add_method("send_fizzle", |_lua, this, (serial, x, y, z, message): (u32, u16, u16, i8, Option<String>)| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendFizzle {
                serial, x, y, z,
                message: message.unwrap_or_else(|| "The spell fizzles.".to_string()),
            });
            Ok(())
        });

        // ── Equipment / object packets ───────────────────────────────

        // send_equip_item(params: table)
        // Sends EquipItem (0x2E) to the client — e.g. show a mount on a mobile.
        // params: { item_serial, graphic, layer (wire byte), mobile_serial, color? }
        methods.add_method("send_equip_item", |_lua, this, params: LuaTable| {
            let item_serial: u32 = params.get("item_serial")?;
            let graphic: u16 = params.get("graphic")?;
            let layer: u8 = params.get("layer")?;
            let mobile_serial: u32 = params.get("mobile_serial")?;
            let color: u16 = params.get("color").unwrap_or(0);
            let _ = this.action_tx.try_send(LuaSessionAction::SendEquipItem {
                item_serial, graphic, layer, mobile_serial, color,
            });
            Ok(())
        });

        // send_delete_object(serial: number)
        // Sends DeleteObject (0x1D) to the client — e.g. remove mount visual on dismount.
        methods.add_method("send_delete_object", |_lua, this, serial: u32| {
            let _ = this.action_tx.try_send(LuaSessionAction::SendDeleteObject { serial });
            Ok(())
        });
    }
}

// ── Broadcast userdata ────────────────────────────────────────────────────

/// Lua `broadcast` object — send sound, effect, animation, speech via worker.
#[derive(Clone)]
struct LuaBroadcast {
    action_tx: mpsc::Sender<LuaSessionAction>,
}

impl LuaUserData for LuaBroadcast {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // sound(id, x, y, z)
        methods.add_method("sound", |_lua, this, (sound_id, x, y, z): (u16, u16, u16, i16)| {
            let _ = this.action_tx.try_send(LuaSessionAction::BroadcastSound {
                sound_id, x, y, z,
            });
            Ok(())
        });

        // effect(params)
        methods.add_method("effect", |_lua, this, p: crate::lua_script::params::EffectParams| {
            let _ = this.action_tx.try_send(LuaSessionAction::BroadcastEffect {
                direction_type: p.direction_type,
                source_serial: p.source_serial,
                target_serial: p.target_serial,
                graphic: p.graphic,
                x: p.x, y: p.y, z: p.z,
                target_x: p.target_x, target_y: p.target_y, target_z: p.target_z,
                speed: p.speed, duration: p.duration,
                fixed_direction: p.fixed_direction, explode: p.explode,
            });
            Ok(())
        });

        // animation(serial, action, frame_count, opts)
        methods.add_async_method("animation", |_lua, this,
            (serial, action, frame_count, a): (u32, u16, u8, crate::lua_script::params::AnimateOpts)|
        {
            async move {
                let _ = this.action_tx.try_send(LuaSessionAction::BroadcastAnimation {
                    serial, action, frame_count,
                    repeat_count: a.repeat_count,
                    reverse: a.reverse,
                    repeat: a.repeat,
                    frame_delay: a.frame_delay,
                    x: 0, y: 0, // TODO: fetch entity position
                });
                Ok(())
            }
        });

        // speech(serial, msg, opts)
        methods.add_method("speech", |_lua, this,
            (serial, graphic, message, s): (u32, u16, String, crate::lua_script::params::SayOpts)|
        {
            let _ = this.action_tx.try_send(LuaSessionAction::BroadcastSpeech {
                serial, graphic,
                speech_type: s.speech_type,
                color: s.color,
                font: s.font,
                name: s.name.unwrap_or_default(),
                message,
                x: 0, y: 0,
            });
            Ok(())
        });
    }
}

// ── Globals registration ──────────────────────────────────────────────────

pub(super) fn register_session_globals(
    lua: &Lua,
    script_name: &str,
    worker_tx: DemoWorkerTx,
    event_rx: mpsc::Receiver<SessionLuaEvent>,
    action_tx: mpsc::Sender<LuaSessionAction>,
    cancel: CancellationToken,
    player_serial: u32,
    map_id: u8,
    blocking_gump_flag: Arc<AtomicBool>,
) -> Result<(), LuaError> {
    let globals = lua.globals();

    // engine — async RPC to game engine
    globals.set("engine", LuaEngine {
        engine: EngineProxy::new(worker_tx.clone(), map_id),
    })?;

    // session — client interaction
    globals.set("session", LuaSession {
        player_serial,
        map_id,
        action_tx: action_tx.clone(),
        engine: EngineProxy::new(worker_tx.clone(), map_id),
        blocking_gump_flag,
    })?;

    // broadcast — sound, effect, animation, speech
    globals.set("broadcast", LuaBroadcast {
        action_tx: action_tx.clone(),
    })?;

    // log(msg)
    let name = script_name.to_string();
    let log_fn = lua.create_function(move |_lua, msg: String| {
        info!("[session-lua:{}] {}", name, msg);
        Ok(())
    })?;
    globals.set("log", log_fn)?;

    // sleep(ms) — async, cancellable
    let cancel_sleep = cancel.clone();
    let sleep_fn = lua.create_async_function(move |_lua, ms: u64| {
        let cancel = cancel_sleep.clone();
        async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    Err(LuaError::external("script cancelled"))
                }
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {
                    Ok(())
                }
            }
        }
    })?;
    globals.set("sleep", sleep_fn)?;

    // Event handling — buffered mpsc receiver behind Arc<Mutex<>>
    let event_state = Arc::new(std::sync::Mutex::new(SessionEventState::new(event_rx)));

    // poll_event() → table | nil  (non-blocking)
    {
        let state = event_state.clone();
        let poll_fn = lua.create_function(move |lua, ()| {
            let mut state = state.lock().map_err(|e| LuaError::external(e.to_string()))?;
            match state.try_recv() {
                Some(event) => session_event_to_lua(&lua, &event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        })?;
        globals.set("poll_event", poll_fn)?;
    }

    // wait_event(timeout_ms) → (table | nil), elapsed_ms  (async)
    // Returns actual wall-clock elapsed time as the second value so Lua
    // timers can tick accurately even when events arrive early.
    {
        let state = event_state.clone();
        let cancel_wait = cancel.clone();
        let wait_fn = lua.create_async_function(move |lua, timeout_ms: u64| {
            let state = state.clone();
            let cancel = cancel_wait.clone();
            async move {
                let start = tokio::time::Instant::now();
                let deadline = start + Duration::from_millis(timeout_ms);

                // Helper: compute elapsed ms since start (minimum 1).
                let elapsed_ms = |start: tokio::time::Instant| -> u64 {
                    start.elapsed().as_millis().max(1) as u64
                };

                // Check buffer first.
                {
                    let mut s = state.lock().map_err(|e| LuaError::external(e.to_string()))?;
                    if let Some(event) = s.try_recv() {
                        let tbl = session_event_to_lua(&lua, &event)?;
                        return Ok((LuaValue::Table(tbl), elapsed_ms(start)));
                    }
                }

                // Wait for new events.
                loop {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return Ok((LuaValue::Nil, elapsed_ms(start)));
                    }

                    {
                        let mut s = state.lock().map_err(|e| LuaError::external(e.to_string()))?;
                        s.drain_channel();
                        if let Some(event) = s.pop_buffered() {
                            let tbl = session_event_to_lua(&lua, &event)?;
                            return Ok((LuaValue::Table(tbl), elapsed_ms(start)));
                        }
                    }

                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            return Err(LuaError::external("script cancelled"));
                        }
                        _ = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(50))) => {}
                    }
                }
            }
        })?;
        globals.set("wait_event", wait_fn)?;
    }

    Ok(())
}

// ── Session event state ───────────────────────────────────────────────────

/// Buffers session events from the mpsc channel.
struct SessionEventState {
    rx: mpsc::Receiver<SessionLuaEvent>,
    buffer: std::collections::VecDeque<SessionLuaEvent>,
}

impl SessionEventState {
    fn new(rx: mpsc::Receiver<SessionLuaEvent>) -> Self {
        Self {
            rx,
            buffer: std::collections::VecDeque::new(),
        }
    }

    /// Drain all available events from the channel into the buffer.
    fn drain_channel(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.buffer.push_back(event);
        }
    }

    fn pop_buffered(&mut self) -> Option<SessionLuaEvent> {
        self.buffer.pop_front()
    }

    fn try_recv(&mut self) -> Option<SessionLuaEvent> {
        self.drain_channel();
        self.pop_buffered()
    }
}

// ── SpellDef → Lua table ──────────────────────────────────────────────────

/// Convert a `SpellDef` to a Lua table with all fields.
fn spell_def_to_lua_table(lua: &Lua, spell: &crate::magic::SpellDef) -> LuaResult<LuaTable> {
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
    // Reagents as a Lua array table.
    let reagents = lua.create_table()?;
    for (i, &r) in spell.reagents.iter().enumerate() {
        reagents.set(i + 1, r)?;
    }
    t.set("reagents", reagents)?;
    Ok(t)
}

// ── Event conversion to Lua ───────────────────────────────────────────────

fn session_event_to_lua(lua: &Lua, event: &SessionLuaEvent) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    match event {
        // ── Typed packet events ──────────────────────────────────────
        SessionLuaEvent::CastSpell { spell_id } => {
            t.set("type", "cast_spell")?;
            t.set("spell_id", *spell_id)?;
            // Backward-compat fields (old scripts check ev.id == 0x12)
            t.set("id", 0x12u8)?;
            t.set("command_type", "CastSpell")?;
            t.set("command", spell_id.to_string())?;
        }
        SessionLuaEvent::UseSkill { skill_id } => {
            t.set("type", "use_skill")?;
            t.set("skill_id", *skill_id)?;
            t.set("id", 0x12u8)?;
            t.set("command_type", "UseSkill")?;
            t.set("command", format!("{} 0", skill_id))?;
        }
        SessionLuaEvent::TargetCursor {
            cursor_type, cursor_id, target_serial,
            target_x, target_y, target_z, target_graphic,
        } => {
            t.set("type", "target_cursor")?;
            t.set("id", 0x6Cu8)?;
            t.set("cursor_type", *cursor_type)?;
            t.set("cursor_id", *cursor_id)?;
            t.set("target_serial", *target_serial)?;
            t.set("target_x", *target_x)?;
            t.set("target_y", *target_y)?;
            t.set("target_z", *target_z)?;
            t.set("target_graphic", *target_graphic)?;
        }
        SessionLuaEvent::DoubleClick { serial, paperdoll } => {
            t.set("type", "double_click")?;
            t.set("id", 0x06u8)?;
            t.set("serial", *serial)?;
            t.set("paperdoll", *paperdoll)?;
        }
        SessionLuaEvent::WarMode { fighting } => {
            t.set("type", "war_mode")?;
            t.set("id", 0x72u8)?;
            t.set("war_mode", *fighting)?;
            t.set("fighting", *fighting)?;
        }
        SessionLuaEvent::AttackRequest { target } => {
            t.set("type", "attack_request")?;
            t.set("id", 0x05u8)?;
            t.set("target_serial", *target)?;
        }
        SessionLuaEvent::CastTargetedSpell { spell_id, target } => {
            t.set("type", "cast_targeted_spell")?;
            t.set("id", 0xBFu8)?;
            t.set("spell_id", *spell_id)?;
            t.set("target_serial", *target)?;
        }
        SessionLuaEvent::Emote { action } => {
            t.set("type", "emote")?;
            t.set("id", 0x12u8)?;
            t.set("command_type", "Action")?;
            t.set("command", action.as_str())?;
            t.set("action", action.as_str())?;
        }

        // ── Legacy raw packet (backward compat) ──────────────────────
        SessionLuaEvent::Packet { id, data } => {
            t.set("type", "packet")?;
            t.set("id", *id)?;
            t.set("data", lua.create_string(data)?)?;
            parse_packet_fields(lua, &t, *id, data)?;
        }

        // ── World events ─────────────────────────────────────────────
        SessionLuaEvent::WorldEvent(event) => {
            world_event_to_session_lua(lua, &t, event)?;
        }
    }
    Ok(t)
}

/// Parse common fields from well-known UO packets into the Lua table.
///
/// **Legacy**: only used for the `SessionLuaEvent::Packet` backward-compat
/// variant.  New code should use typed `SessionLuaEvent` variants instead.
fn parse_packet_fields(_lua: &Lua, t: &LuaTable, id: u8, data: &[u8]) -> LuaResult<()> {
    use packets::traits::BasicPacket;
    use protocol::prelude::ManualPacket;

    match id {
        // WarMode (0x72)
        0x72 => {
            if let Ok(pkt) = packets::system::WarMode::from_bytes(data) {
                t.set("war_mode", pkt.is_fighting())?;
            }
        }
        // RequestAttack (0x05)
        0x05 => {
            if let Ok(pkt) = packets::interaction::RequestAttack::from_bytes(data) {
                t.set("target_serial", pkt.target_id)?;
            }
        }
        // DoubleClick (0x06)
        0x06 => {
            if data.len() >= 5 {
                let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                t.set("serial", serial & 0x7FFF_FFFF)?;
                t.set("paperdoll", serial & 0x8000_0000 != 0)?;
            }
        }
        // SingleClick (0x09)
        0x09 => {
            if data.len() >= 5 {
                let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                t.set("serial", serial)?;
            }
        }
        // TextCommand (0x12) — spell cast, skill use
        0x12 => {
            if let Ok(pkt) = packets::action::TextCommand::from_bytes(data) {
                match &pkt {
                    packets::action::TextCommand::CastSpell { spell } => {
                        t.set("command_type", "CastSpell")?;
                        t.set("command", spell.to_string())?;
                    }
                    packets::action::TextCommand::UseSkill { skill } => {
                        t.set("command_type", "UseSkill")?;
                        t.set("command", skill.to_string())?;
                    }
                    packets::action::TextCommand::Action { action } => {
                        t.set("command_type", "Action")?;
                        t.set("command", action.to_string())?;
                    }
                    packets::action::TextCommand::OpenDoor => {
                        t.set("command_type", "OpenDoor")?;
                    }
                }
            }
        }
        // GetMobileStatus (0x34)
        0x34 => {
            if data.len() >= 10 {
                let serial = u32::from_be_bytes([data[5], data[6], data[7], data[8]]);
                let status_type = data[9];
                t.set("serial", serial)?;
                t.set("status_type", status_type)?;
            }
        }
        // TargetCursor response (0x6C)
        0x6C => {
            if data.len() >= 19 {
                let cursor_type = data[1];
                let cursor_id = u32::from_be_bytes([data[2], data[3], data[4], data[5]]);
                let target_serial = u32::from_be_bytes([data[7], data[8], data[9], data[10]]);
                let target_x = u16::from_be_bytes([data[11], data[12]]);
                let target_y = u16::from_be_bytes([data[13], data[14]]);
                let target_z = data[16] as i8;
                let target_graphic = u16::from_be_bytes([data[17], data[18]]);
                t.set("cursor_type", cursor_type)?;
                t.set("cursor_id", cursor_id)?;
                t.set("target_serial", target_serial)?;
                t.set("target_x", target_x)?;
                t.set("target_y", target_y)?;
                t.set("target_z", target_z)?;
                t.set("target_graphic", target_graphic)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Convert a WorldEvent to a Lua table for session scripts.
fn world_event_to_session_lua(lua: &Lua, t: &LuaTable, event: &WorldEvent) -> LuaResult<()> {
    match event {
        WorldEvent::DamageDealt { serial, source_serial, amount, new_hits, max_hits, .. } => {
            t.set("type", "damage_dealt")?;
            t.set("serial", *serial)?;
            t.set("source_serial", *source_serial)?;
            t.set("amount", *amount)?;
            t.set("new_hits", *new_hits)?;
            t.set("max_hits", *max_hits)?;
        }
        WorldEvent::MobileKilled { serial, corpse_serial, .. } => {
            t.set("type", "mobile_killed")?;
            t.set("serial", *serial)?;
            t.set("corpse_serial", *corpse_serial)?;
        }
        WorldEvent::EntityMoved { serial, old_pos, new_pos, .. } => {
            t.set("type", "entity_moved")?;
            t.set("serial", *serial)?;
            t.set("old_x", old_pos.pos3d().x)?;
            t.set("old_y", old_pos.pos3d().y)?;
            t.set("new_x", new_pos.pos3d().x)?;
            t.set("new_y", new_pos.pos3d().y)?;
            t.set("direction", new_pos.facing.raw())?;
        }
        WorldEvent::EntityRemoved { serial, .. } => {
            t.set("type", "entity_removed")?;
            t.set("serial", *serial)?;
        }
        WorldEvent::MobileHealed { serial, amount, new_hits, max_hits, .. } => {
            t.set("type", "mobile_healed")?;
            t.set("serial", *serial)?;
            t.set("amount", *amount)?;
            t.set("new_hits", *new_hits)?;
            t.set("max_hits", *max_hits)?;
        }
        WorldEvent::ManaStaminaChanged { serial, mana, max_mana, stamina, max_stamina, .. } => {
            t.set("type", "mana_stamina_changed")?;
            t.set("serial", *serial)?;
            t.set("mana", *mana)?;
            t.set("max_mana", *max_mana)?;
            t.set("stamina", *stamina)?;
            t.set("max_stamina", *max_stamina)?;
        }
        // For other events, provide type + serial if available.
        WorldEvent::ContainerContentsUpdated { container_serial, changes, .. } => {
            use framework::continuum::ContainerContentChange;
            t.set("type", "container_contents_updated")?;
            t.set("container_serial", *container_serial)?;
            let changes_table = lua.create_table()?;
            for (i, change) in changes.iter().enumerate() {
                let ct = lua.create_table()?;
                match change {
                    ContainerContentChange::ItemAdded { item_serial, graphic, amount, x, y, color } => {
                        ct.set("action", "added")?;
                        ct.set("item_serial", *item_serial)?;
                        ct.set("graphic", *graphic)?;
                        ct.set("amount", *amount)?;
                        ct.set("x", *x)?;
                        ct.set("y", *y)?;
                        ct.set("color", *color)?;
                    }
                    ContainerContentChange::ItemRemoved { item_serial } => {
                        ct.set("action", "removed")?;
                        ct.set("item_serial", *item_serial)?;
                    }
                    ContainerContentChange::ItemUpdated { item_serial, graphic, amount, x, y, color } => {
                        ct.set("action", "updated")?;
                        ct.set("item_serial", *item_serial)?;
                        ct.set("graphic", *graphic)?;
                        ct.set("amount", *amount)?;
                        ct.set("x", *x)?;
                        ct.set("y", *y)?;
                        ct.set("color", *color)?;
                    }
                }
                changes_table.set(i + 1, ct)?;
            }
            t.set("changes", changes_table)?;
        }
        _other => {
            t.set("type", "world_event")?;
            // Generic fallback — Lua can still see it's some event.
        }
    }
    Ok(())
}

// ── Entity → Lua table conversion ────────────────────────────────────────

fn entity_to_lua(lua: &Lua, entity: &DemoEntity) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    match entity {
        DemoEntity::Mobile(m) => {
            t.set("type", "mobile")?;
            t.set("is_mobile", true)?;
            t.set("serial", m.serial)?;
            t.set("graphic", m.graphic)?;
            t.set("x", m.x)?;
            t.set("y", m.y)?;
            t.set("z", m.z)?;
            t.set("direction", m.direction)?;
            t.set("color", m.color)?;
            t.set("name", m.name.as_str())?;
            t.set("hits", m.hits)?;
            t.set("hits_max", m.hits_max)?;
            t.set("mana", m.mana)?;
            t.set("mana_max", m.mana_max)?;
            t.set("stamina", m.stamina)?;
            t.set("stamina_max", m.stamina_max)?;
            t.set("str", m.str_)?;
            t.set("dex", m.dex)?;
            t.set("int", m.int)?;
            use packets::movement::Notoriety;
            let noto = match m.notoriety {
                Notoriety::Invalid => 0u8,
                Notoriety::Innocent => 1,
                Notoriety::Ally => 2,
                Notoriety::Attackable => 3,
                Notoriety::Criminal => 4,
                Notoriety::Enemy => 5,
                Notoriety::Murderer => 6,
                Notoriety::Translucent => 7,
                Notoriety::Unknown(v) => v,
            };
            t.set("notoriety", noto)?;

            // Equipment list — each entry: { serial, graphic, layer, color }
            let items_table = lua.create_table()?;
            for (i, eq) in m.items.iter().enumerate() {
                let item = lua.create_table()?;
                item.set("serial", eq.serial)?;
                item.set("graphic", eq.graphic)?;
                item.set("layer", eq.layer.to_wire())?;
                item.set("color", eq.color.unwrap_or(0))?;
                items_table.set(i + 1, item)?;
            }
            t.set("items", items_table)?;
        }
        DemoEntity::Item {
            serial, graphic, color, amount, x, y, z, is_container, ..
        } => {
            t.set("type", "item")?;
            t.set("serial", *serial)?;
            t.set("graphic", *graphic)?;
            t.set("color", *color)?;
            t.set("amount", *amount)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
            t.set("is_container", *is_container)?;
        }
        DemoEntity::Multi { serial, graphic, x, y, z, .. } => {
            t.set("type", "multi")?;
            t.set("serial", *serial)?;
            t.set("graphic", *graphic)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
        }
    }
    Ok(t)
}

// ── ItemProps ↔ Lua conversion ────────────────────────────────────────────

/// Convert `ItemProps` to a Lua table.
///
/// ```lua
/// {
///   name = "Marked Rune",         -- or nil
///   weight_override = 10,         -- or nil
///   meta = {
///     mark_x = 1234,              -- MetaValue::Int
///     mark_y = 567,               -- MetaValue::Int
///     blessed = true,             -- MetaValue::Bool
///     crafted_by = "Blackthorn",  -- MetaValue::Str
///     speed = 1.5,                -- MetaValue::Float
///   }
/// }
/// ```
fn item_props_to_lua(lua: &Lua, props: &ItemProps) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;

    // name
    match props.name() {
        Some(name) => t.set("name", name)?,
        None => t.set("name", LuaValue::Nil)?,
    }

    // weight_override
    match props.weight_override {
        Some(w) => t.set("weight_override", w)?,
        None => t.set("weight_override", LuaValue::Nil)?,
    }

    // meta
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

    Ok(t)
}

/// Convert a Lua table to `ItemProps`.
///
/// Accepts the same shape as `item_props_to_lua` produces.
/// Missing fields default to `None` / empty.
fn lua_to_item_props(t: &LuaTable) -> LuaResult<ItemProps> {
    let name: Option<String> = t.get("name").ok();
    let weight_override: Option<u16> = t.get("weight_override").ok();

    let mut meta = std::collections::HashMap::new();
    if let Ok(meta_table) = t.get::<LuaTable>("meta") {
        for pair in meta_table.pairs::<String, LuaValue>() {
            let (key, value) = pair?;
            let mv = match value {
                LuaValue::Integer(v) => MetaValue::Int(v),
                LuaValue::Number(v) => {
                    // If the float is actually a whole number, store as Int
                    // for cleaner round-tripping.
                    if v.fract() == 0.0 && v.abs() < i64::MAX as f64 {
                        MetaValue::Int(v as i64)
                    } else {
                        MetaValue::Float(v)
                    }
                }
                LuaValue::String(s) => MetaValue::Str(s.to_str()?.to_string()),
                LuaValue::Boolean(b) => MetaValue::Bool(b),
                _ => continue, // skip nil, table, function, etc.
            };
            meta.insert(key, mv);
        }
    }

    Ok(ItemProps {
        text: name.map(|n| ObjectText::with_title(n)).unwrap_or_default(),
        weight_override,
        meta,
    })
}
