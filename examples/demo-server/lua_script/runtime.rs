//! Demo-server Lua bindings: `World` userdata, entity conversions.
//!
//! The shared runtime (globals, event handling, script lifecycle) is
//! provided by [`framework::mitos`].  This module defines:
//! - [`DemoBackend`] — the [`ScriptingBackend`](framework::mitos::ScriptingBackend) implementation
//! - [`LuaWorld`] — the demo-server specific `World` userdata
//! - Entity/event conversion helpers

use std::path::PathBuf;
use std::sync::Arc;

use mlua::prelude::*;

use framework::continuum::{WorldEvent, WorkerCommand};
use framework::ecumene::Entity as _;
use framework::mitos;
use u_core::{Facing, Heading};

use common::uo_engine::entity::{DemoEntity, MobileData};
use common::uo_engine::rpc::EngineProxy;
use common::uo_engine::item_props::{ItemProps, MetaValue, ObjectText};
use common::uo_engine::serial_alloc::SerialAllocator;

use crate::{DemoCommand, DemoWorkerTx};

// ── ScriptingBackend implementation ───────────────────────────────────────

/// Demo-server scripting backend — connects Lua scripts to the game engine.
#[derive(Clone)]
pub(crate) struct DemoBackend {
    pub worker_tx: DemoWorkerTx,
    pub event_tx: tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    pub serial_alloc: Arc<SerialAllocator>,
    pub scripts_dir: PathBuf,
}

impl mitos::ScriptingBackend for DemoBackend {
    type Event = WorldEvent;

    fn event_to_lua(&self, lua: &Lua, event: &Self::Event) -> LuaResult<LuaValue> {
        world_event_to_lua(lua, event).map(LuaValue::Table)
    }

    fn create_world_constructor(&self, lua: &Lua) -> LuaResult<LuaFunction> {
        let tx = self.worker_tx.clone();
        let etx = self.event_tx.clone();
        let sa = self.serial_alloc.clone();
        let sd = self.scripts_dir.clone();
        lua.create_function(move |_lua, map_id: u8| {
            Ok(LuaWorld {
                engine: EngineProxy::new(tx.clone(), map_id),
                event_tx: etx.clone(),
                serial_alloc: sa.clone(),
                scripts_dir: sd.clone(),
            })
        })
    }

    fn log_prefix(&self) -> &str {
        "lua"
    }
}

// ── World userdata ────────────────────────────────────────────────────────

#[derive(Clone)]
struct LuaWorld {
    engine: EngineProxy<DemoCommand>,
    event_tx: tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    serial_alloc: Arc<SerialAllocator>,
    scripts_dir: PathBuf,
}

impl LuaUserData for LuaWorld {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // ── get_entity(serial) → table | nil ──────────────────────────
        methods.add_async_method("get_entity", |lua, this, serial: u32| async move {
            let entity = this.engine.get_entity(serial).await;
            match entity {
                Some(e) => entity_to_lua(&lua, &e).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── step(serial, direction) → table | nil ─────────────────────
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

        // ── teleport(serial, x, y, z) ────────────────────────────────
        methods.add_async_method("teleport", |_lua, this, (serial, x, y, z): (u32, u16, u16, i8)| async move {
            this.engine.teleport(serial, x, y, z, None).await;
            Ok(())
        });

        // ── query_area(x1, y1, x2, y2) → table of entity tables ──────
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

        // ── test_step(x, y, z, direction) → number | nil ─────────────
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

        // ── resolve_z(x, y, z_hint, direction) → number | nil ────────
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

        // ── has_los(x1, y1, z1, x2, y2, z2) → boolean ─────────────
        methods.add_async_method("has_los", |_lua, this, (x1, y1, z1, x2, y2, z2): (u16, u16, i16, u16, u16, i16)| async move {
            let result = this.engine.check_los(x1, y1, z1, x2, y2, z2).await;
            Ok(LuaValue::Boolean(result))
        });

        // ── play_sound(sound_id, x, y, z) ──────────────────────────
        methods.add_async_method("play_sound", |_lua, this, (sound_id, x, y, z): (u16, u16, u16, i16)| async move {
            let cmd = WorkerCommand::MapCommand(
                this.engine.world,
                crate::DemoCommand::BroadcastSound { sound_id, x, y, z },
            );
            this.engine.tx().send(cmd).await
                .map_err(|e| LuaError::external(format!("{e}")))?;
            Ok(())
        });

        // ── effect(params) ────────────────────────────────────────────
        methods.add_async_method("effect", |_lua, this, p: mitos::EffectParams| async move {
            let cmd = WorkerCommand::MapCommand(
                this.engine.world,
                crate::DemoCommand::BroadcastEffect {
                    direction_type: p.direction_type,
                    source_serial: p.source_serial,
                    target_serial: p.target_serial,
                    graphic: p.graphic,
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    target_x: p.target_x,
                    target_y: p.target_y,
                    target_z: p.target_z,
                    speed: p.speed,
                    duration: p.duration,
                    fixed_direction: p.fixed_direction,
                    explode: p.explode,
                },
            );
            this.engine.tx().send(cmd).await
                .map_err(|e| LuaError::external(format!("{e}")))?;
            Ok(())
        });

        // ── animate(serial, action, frame_count, opts) ────────────────
        methods.add_async_method("animate", |_lua, this,
            (serial, action, frame_count, a): (u32, u16, u8, mitos::AnimateOpts)|
        {
            async move {
                let (x, y) = match this.engine.get_entity(serial).await {
                    Some(e) => { let (x, y, _) = e.xyz(); (x, y) }
                    None => (0, 0),
                };
                let cmd = WorkerCommand::MapCommand(
                    this.engine.world,
                    crate::DemoCommand::BroadcastAnimation {
                        serial,
                        action,
                        frame_count,
                        repeat_count: a.repeat_count,
                        reverse: a.reverse,
                        repeat: a.repeat,
                        frame_delay: a.frame_delay,
                        x,
                        y,
                    },
                );
                this.engine.tx().send(cmd).await
                    .map_err(|e| LuaError::external(format!("{e}")))?;
                Ok(())
            }
        });

        // ── say(serial, message, opts) ────────────────────────────────
        methods.add_async_method("say", |_lua, this,
            (serial, message, s): (u32, String, mitos::SayOpts)|
        {
            async move {
                let (graphic, x, y, entity_name) =
                    match this.engine.get_entity(serial).await {
                        Some(ref e) => {
                            let (x, y, _) = e.xyz();
                            let name = e.mobile().map(|m| m.name.to_string()).unwrap_or_default();
                            (e.graphic(), x, y, name)
                        }
                        None => (0, 0, 0, String::new()),
                    };

                let speaker_name = s.name.unwrap_or(entity_name);

                let cmd = WorkerCommand::MapCommand(
                    this.engine.world,
                    crate::DemoCommand::BroadcastSpeech {
                        serial,
                        graphic,
                        speech_type: s.speech_type,
                        color: s.color,
                        font: s.font,
                        name: speaker_name,
                        message,
                        x,
                        y,
                    },
                );
                this.engine.tx().send(cmd).await
                    .map_err(|e| LuaError::external(format!("{e}")))?;
                Ok(())
            }
        });

        // ── map_id() → number ─────────────────────────────────────────
        methods.add_method("map_id", |_lua, this, ()| {
            Ok(this.engine.world)
        });

        // ── deal_damage(serial, amount, source_serial?) → table | nil ──
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
                        // Schedule corpse decay for auto-killed entities.
                        crate::game_util::schedule_corpse_decay(
                            this.engine.tx(),
                            this.engine.world,
                            kill.corpse_serial,
                        );
                    }
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });

        // ── heal_entity(serial, amount) → number | nil ────────────────
        methods.add_async_method("heal_entity", |_lua, this, (serial, amount): (u32, u16)| async move {
            let result = this.engine.heal(serial, amount).await;
            match result {
                Some(new_hits) => Ok(LuaValue::Integer(new_hits as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── modify_mana(serial, delta) → number | nil ─────────────────
        methods.add_async_method("modify_mana", |_lua, this, (serial, delta): (u32, i32)| async move {
            let result = this.engine.modify_mana(serial, delta).await;
            match result {
                Some(new_mana) => Ok(LuaValue::Integer(new_mana as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── modify_stamina(serial, delta) → number | nil ──────────────
        methods.add_async_method("modify_stamina", |_lua, this, (serial, delta): (u32, i32)| async move {
            let result = this.engine.modify_stamina(serial, delta).await;
            match result {
                Some(new_stamina) => Ok(LuaValue::Integer(new_stamina as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── consume_mana(serial, amount) → number | nil ───────────────
        methods.add_async_method("consume_mana", |_lua, this, (serial, amount): (u32, u16)| async move {
            let result = this.engine.consume_mana(serial, amount).await;
            match result {
                Some(new_mana) => Ok(LuaValue::Integer(new_mana as i64)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── get_weight(serial) → table | nil ──────────────────────────
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

        // ── is_mounted(serial) → boolean ──────────────────────────────
        methods.add_async_method("is_mounted", |_lua, this, serial: u32| async move {
            let entity = this.engine.get_entity(serial).await;
            let mounted = entity.as_ref()
                .and_then(|e| e.mobile())
                .map(|m| m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount))
                .unwrap_or(false);
            Ok(LuaValue::Boolean(mounted))
        });

        // ── get_spell(spell_id) → table | nil ────────────────────────
        methods.add_method("get_spell", |lua, _this, spell_id: u16| {
            match crate::magic::get_spell(spell_id) {
                Some(spell) => spell_to_lua(&lua, spell).map(|t| LuaValue::Table(t)),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── cast_spell(caster_serial, target_serial, spell_id) → boolean
        methods.add_async_method("cast_spell", |_lua, this,
            (caster, target, spell_id): (u32, u32, u16)|
        {
            async move {
                let spell = match crate::magic::get_spell(spell_id) {
                    Some(s) => s,
                    None => return Ok(LuaValue::Boolean(false)),
                };
                let _pkts = crate::magic::execute_spell(
                    spell, caster, target, this.engine.tx(), this.engine.world,
                ).await;
                Ok(LuaValue::Boolean(true))
            }
        });

        // ── spawn_npc(params) → serial ────────────────────────────────
        methods.add_async_method("spawn_npc", |_lua, this, params: LuaTable| async move {
            let serial = this.serial_alloc.alloc_mobile()
                .ok_or_else(|| LuaError::external("mobile serial space exhausted"))?;
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

            let notoriety = notoriety_from_u8(notoriety_val);
            let items = parse_equipment_items(&params, serial, &this.serial_alloc);

            let entity = DemoEntity::Mobile(MobileData {
                serial, graphic, x, y, z, direction, color,
                status: packets::mobile_flags::MobileFlags(0),
                notoriety, items, name, hits, hits_max,
                mana: hits_max, mana_max: hits_max,
                stamina: hits_max, stamina_max: hits_max,
                str_: 50, dex: 50, int: 50,
                is_player: false, dead: false, living_graphic: 0,
                noto_class: noto_class_from_wire(notoriety),
                ..Default::default()
            });

            this.engine.spawn_entity(serial, entity).await;
            Ok(serial)
        });

        // ── update_entity(serial, params) ─────────────────────────────
        methods.add_async_method("update_entity", |_lua, this, (serial, params): (u32, LuaTable)| async move {
            let current = this.engine.get_entity(serial).await;
            let Some(m) = current.as_ref().and_then(|e| e.mobile()) else {
                return Ok(());
            };
            let cur_graphic = m.graphic;
            let (cur_x, cur_y, cur_z) = (m.x, m.y, m.z);
            let cur_dir = m.direction;
            let cur_color = m.color;
            let cur_status = m.status;
            let cur_noto = m.notoriety;
            let cur_items = m.items.to_vec();
            let cur_name = m.name.to_string();
            let cur_hits = m.hits;
            let cur_hmax = m.hits_max;
            let cur_mana = m.mana;
            let cur_mana_max = m.mana_max;
            let cur_stamina = m.stamina;
            let cur_stamina_max = m.stamina_max;
            let cur_str = m.str_;
            let cur_dex = m.dex;
            let cur_int = m.int;
            let cur_is_player = m.is_player;
            let cur_dead = m.dead;
            let cur_living_graphic = m.living_graphic;
            let cur_noto_class = m.noto_class;
            let cur_guild_id = m.guild_id;
            let cur_murders = m.murders;
            let cur_karma = m.karma;
            let cur_fame = m.fame;
            let cur_criminal_until = m.criminal_until_ms;
            let cur_aggressors = m.aggressors.clone();

            let graphic: u16 = params.get("graphic").unwrap_or(cur_graphic);
            let color: u16 = params.get("color").unwrap_or(cur_color);
            let direction: u8 = params.get("direction").unwrap_or(cur_dir);
            let name: String = params.get("name").unwrap_or(cur_name);
            let notoriety_val: u8 = params.get("notoriety").ok()
                .unwrap_or(notoriety_to_u8(cur_noto));
            let hits: u16 = params.get("hits").unwrap_or(cur_hits);
            let hits_max: u16 = params.get("hits_max").unwrap_or(cur_hmax);

            let items = if params.contains_key("items")? {
                parse_equipment_items(&params, serial, &this.serial_alloc)
            } else {
                cur_items
            };

            let entity = DemoEntity::Mobile(MobileData {
                serial, graphic, x: cur_x, y: cur_y, z: cur_z,
                direction, color, status: cur_status,
                notoriety: notoriety_from_u8(notoriety_val),
                items, name, hits, hits_max,
                mana: cur_mana, mana_max: cur_mana_max,
                stamina: cur_stamina, stamina_max: cur_stamina_max,
                str_: cur_str, dex: cur_dex, int: cur_int,
                is_player: cur_is_player, dead: cur_dead,
                living_graphic: cur_living_graphic,
                noto_class: cur_noto_class,
                guild_id: cur_guild_id,
                murders: cur_murders,
                karma: cur_karma,
                fame: cur_fame,
                criminal_until_ms: cur_criminal_until,
                aggressors: cur_aggressors,
                ..Default::default()
            });

            this.engine.update_entity(serial, entity).await;
            Ok(())
        });

        // ── spawn_item(params) → serial ───────────────────────────────
        methods.add_async_method("spawn_item", |_lua, this, params: LuaTable| async move {
            let serial = this.serial_alloc.alloc_item()
                .ok_or_else(|| LuaError::external("item serial space exhausted"))?;
            let graphic: u16 = params.get("graphic").unwrap_or(0);
            let x: u16 = params.get("x").unwrap_or(0);
            let y: u16 = params.get("y").unwrap_or(0);
            let z: i8 = params.get("z").unwrap_or(0);
            let color: u16 = params.get("color").unwrap_or(0);
            let amount: u16 = params.get("amount").unwrap_or(0);
            let hidden: bool = params.get("hidden").unwrap_or(false);

            let entity = DemoEntity::Item {
                serial, graphic, color,
                amount: if amount > 0 { amount } else { 1 },
                x, y, z, is_container: false, hidden,
                facing: None,
            };

            this.engine.spawn_entity(serial, entity).await;
            Ok(serial)
        });

        // ── remove_entity(serial) ─────────────────────────────────────
        methods.add_async_method("remove_entity", |_lua, this, serial: u32| async move {
            this.engine.remove_entity(serial).await;
            Ok(())
        });

        // ── kill_mobile(serial) ───────────────────────────────────────
        methods.add_async_method("kill_mobile", |_lua, this, serial: u32| async move {
            // Use the engine's atomic KillMobile command which creates a
            // persistent lootable corpse with equipment + loot table items.

            // Generate loot from the demo-server loot tables.
            let loot = crate::loot::generate_loot(serial, &this.engine).await;
            if let Some(kill_result) = this.engine.kill_mobile(serial, loot, None).await {
                // Schedule corpse decay.
                crate::game_util::schedule_corpse_decay(
                    this.engine.tx(),
                    this.engine.world,
                    kill_result.corpse_serial,
                );
            }

            Ok(())
        });

        // ── set_light(level) ──────────────────────────────────────────
        methods.add_method("set_light", |_lua, this, level: u8| {
            let _ = this.event_tx.send(WorldEvent::GlobalLight { map_id: this.engine.world, level });
            Ok(())
        });

        // ── set_weather(type, num_effects, temperature) ───────────────
        methods.add_method("set_weather", |_lua, this, (weather_type, num_effects, temperature): (u8, Option<u8>, Option<u8>)| {
            let _ = this.event_tx.send(WorldEvent::Weather {
                map_id: this.engine.world,
                weather_type,
                num_effects: num_effects.unwrap_or(0x40),
                temperature: temperature.unwrap_or(0x10),
            });
            Ok(())
        });

        // ── set_season(season, play_sound) ────────────────────────────
        methods.add_method("set_season", |_lua, this, (season, play_sound): (u8, Option<bool>)| {
            let _ = this.event_tx.send(WorldEvent::Season {
                map_id: this.engine.world,
                season,
                play_sound: play_sound.unwrap_or(true),
            });
            Ok(())
        });

        // ── play_music(music_id) ──────────────────────────────────────
        methods.add_method("play_music", |_lua, this, music_id: u16| {
            let _ = this.event_tx.send(WorldEvent::Music { map_id: this.engine.world, music_id });
            Ok(())
        });

        // ── persist(serial) ──────────────────────────────────────────
        methods.add_method("persist", |_lua, this, serial: u32| {
            this.serial_alloc.mark_persistent(serial);
            Ok(())
        });

        // ── get_item_props(serial) → table | nil ─────────────────────
        methods.add_async_method("get_item_props", |lua, this, serial: u32| async move {
            let props = this.engine.get_item_props(serial).await;
            match props {
                Some(p) => item_props_to_lua(&lua, &p).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── set_item_props(serial, props_table | nil) ────────────────
        methods.add_async_method("set_item_props", |_lua, this, (serial, props_value): (u32, LuaValue)| async move {
            let props = match props_value {
                LuaValue::Nil => None,
                LuaValue::Table(t) => Some(lua_to_item_props(&t)?),
                _ => return Err(LuaError::external("set_item_props: expected table or nil")),
            };
            this.engine.set_item_props(serial, props).await;
            Ok(())
        });

        // ── attach_controller(serial, script_path) ───────────────────
        methods.add_async_method("attach_controller", |_lua, this, (serial, path): (u32, String)| async move {
            let full_path = this.scripts_dir.join(&path);
            let controller = super::lua_controller::LuaController::from_file(&full_path, Some(&this.scripts_dir))
                .map_err(|e| LuaError::external(e))?;
            let controller_id = crate::controller_registry::controller_id("lua", &path);
            let cmd = WorkerCommand::MapCommand(
                this.engine.world,
                crate::DemoCommand::AttachControllerPersist {
                    serial,
                    controller: Box::new(controller),
                    controller_id,
                },
            );
            this.engine.tx().send(cmd).await
                .map_err(|e| LuaError::external(format!("{e}")))?;
            Ok(())
        });

        // ── Targeted player events ────────────────────────────────────

        // send_gump(target_player, source_serial, gump_id, x, y, layout, text_lines, blocking?)
        methods.add_method("send_gump", |_lua, this,
            (target_player, source_serial, gump_id, gump_x, gump_y, layout, text_lines, blocking):
            (u32, u32, u32, u32, u32, String, Vec<String>, Option<bool>)| {
            let _ = this.event_tx.send(WorldEvent::TargetedGump {
                map_id: this.engine.world,
                target_player,
                source_serial,
                gump_id,
                gump_x,
                gump_y,
                layout,
                text_lines,
                pos_x: 0,
                pos_y: 0,
                blocking: blocking.unwrap_or(false),
            });
            Ok(())
        });

        // send_message(target_player, message, color?)
        methods.add_method("send_message", |_lua, this,
            (target_player, message, color): (u32, String, Option<u16>)| {
            let _ = this.event_tx.send(WorldEvent::TargetedMessage {
                map_id: this.engine.world,
                target_player,
                message,
                color: color.unwrap_or(0x03B2),
                pos_x: 0,
                pos_y: 0,
            });
            Ok(())
        });

        // close_gump(target_player, gump_id)
        methods.add_method("close_gump", |_lua, this,
            (target_player, gump_id): (u32, u32)| {
            let _ = this.event_tx.send(WorldEvent::TargetedCloseGump {
                map_id: this.engine.world,
                target_player,
                gump_id,
                pos_x: 0,
                pos_y: 0,
            });
            Ok(())
        });

        // send_target_cursor(target_player, cursor_id, cursor_type?)
        methods.add_method("send_target_cursor", |_lua, this,
            (target_player, cursor_id, cursor_type): (u32, u32, Option<u8>)| {
            let _ = this.event_tx.send(WorldEvent::TargetedTargetCursor {
                map_id: this.engine.world,
                target_player,
                cursor_id,
                cursor_type: cursor_type.unwrap_or(0),
            });
            Ok(())
        });
    }
}

// ── Entity → Lua table conversion ────────────────────────────────────────

use packets::movement::Notoriety;

fn notoriety_to_u8(n: Notoriety) -> u8 {
    match n {
        Notoriety::Invalid => 0,
        Notoriety::Innocent => 1,
        Notoriety::Ally => 2,
        Notoriety::Attackable => 3,
        Notoriety::Criminal => 4,
        Notoriety::Enemy => 5,
        Notoriety::Murderer => 6,
        Notoriety::Translucent => 7,
        Notoriety::Unknown(v) => v,
    }
}

pub(crate) fn notoriety_from_u8(val: u8) -> Notoriety {
    match val {
        0 => Notoriety::Invalid,
        1 => Notoriety::Innocent,
        2 => Notoriety::Ally,
        3 => Notoriety::Attackable,
        4 => Notoriety::Criminal,
        5 => Notoriety::Enemy,
        6 => Notoriety::Murderer,
        7 => Notoriety::Translucent,
        v => Notoriety::Unknown(v),
    }
}

/// Map a wire notoriety value to an intrinsic
/// [`NotorietyClass`](common::uo_engine::notoriety::NotorietyClass) for a
/// Lua-spawned NPC.
fn noto_class_from_wire(n: Notoriety) -> common::uo_engine::notoriety::NotorietyClass {
    use common::uo_engine::notoriety::NotorietyClass;
    match n {
        Notoriety::Innocent => NotorietyClass::Innocent,
        Notoriety::Criminal => NotorietyClass::Criminal,
        Notoriety::Murderer => NotorietyClass::Murderer,
        Notoriety::Enemy => NotorietyClass::Enemy,
        _ => NotorietyClass::Neutral,
    }
}

pub(crate) fn entity_to_lua(lua: &Lua, entity: &DemoEntity) -> LuaResult<LuaTable> {
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
            t.set("notoriety", notoriety_to_u8(m.notoriety))?;
            t.set("status", m.status.0)?;
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
        DemoEntity::Multi {
            serial, graphic, x, y, z, ..
        } => {
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

// ── WorldEvent → Lua table conversion ────────────────────────────────────

pub(crate) fn world_event_to_lua(lua: &Lua, event: &WorldEvent) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    match event {
        WorldEvent::EntityMoved { map_id, serial, old_pos, new_pos, entity, is_teleport } => {
            t.set("type", "entity_moved")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("old_x", old_pos.pos3d().x)?;
            t.set("old_y", old_pos.pos3d().y)?;
            t.set("old_z", old_pos.pos3d().z)?;
            t.set("new_x", new_pos.pos3d().x)?;
            t.set("new_y", new_pos.pos3d().y)?;
            t.set("new_z", new_pos.pos3d().z)?;
            t.set("direction", new_pos.facing.raw())?;
            t.set("is_teleport", *is_teleport)?;
            if let Some(snap) = entity {
                t.set("graphic", snap.graphic)?;
                t.set("hue", snap.hue)?;
                t.set("notoriety", snap.notoriety)?;
            }
        }
        WorldEvent::EntitySpawned { map_id, serial, pos, entity } => {
            t.set("type", "entity_spawned")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("x", pos.x)?;
            t.set("y", pos.y)?;
            t.set("z", pos.z)?;
            if let Some(snap) = entity {
                t.set("graphic", snap.graphic)?;
                t.set("hue", snap.hue)?;
            }
        }
        WorldEvent::EntityRemoved { map_id, serial, last_pos } => {
            t.set("type", "entity_removed")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("x", last_pos.x)?;
            t.set("y", last_pos.y)?;
            t.set("z", last_pos.z)?;
        }
        WorldEvent::EntityUpdated { map_id, serial, pos, entity } => {
            t.set("type", "entity_updated")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("x", pos.x)?;
            t.set("y", pos.y)?;
            t.set("z", pos.z)?;
            if let Some(snap) = entity {
                t.set("graphic", snap.graphic)?;
                t.set("hue", snap.hue)?;
            }
        }
        WorldEvent::SoundPlayed { map_id, sound_id, x, y, z } => {
            t.set("type", "sound_played")?;
            t.set("map_id", *map_id)?;
            t.set("sound_id", *sound_id)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
        }
        WorldEvent::EffectPlayed {
            map_id, direction_type, source_serial, target_serial,
            graphic, x, y, z, target_x, target_y, target_z,
            speed, duration, fixed_direction, explode,
        } => {
            t.set("type", "effect_played")?;
            t.set("map_id", *map_id)?;
            t.set("direction_type", *direction_type)?;
            t.set("source_serial", *source_serial)?;
            t.set("target_serial", *target_serial)?;
            t.set("graphic", *graphic)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
            t.set("target_x", *target_x)?;
            t.set("target_y", *target_y)?;
            t.set("target_z", *target_z)?;
            t.set("speed", *speed)?;
            t.set("duration", *duration)?;
            t.set("fixed_direction", *fixed_direction)?;
            t.set("explode", *explode)?;
        }
        WorldEvent::AnimationPlayed {
            map_id, serial, action, frame_count,
            repeat_count, reverse, repeat, frame_delay, x, y,
        } => {
            t.set("type", "animation_played")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("action", *action)?;
            t.set("frame_count", *frame_count)?;
            t.set("repeat_count", *repeat_count)?;
            t.set("reverse", *reverse)?;
            t.set("repeat", *repeat)?;
            t.set("frame_delay", *frame_delay)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
        }
        WorldEvent::Speech {
            map_id, serial, graphic, speech_type,
            color, font, name, message, x, y,
        } => {
            t.set("type", "speech")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("graphic", *graphic)?;
            t.set("speech_type", *speech_type)?;
            t.set("color", *color)?;
            t.set("font", *font)?;
            t.set("name", name.as_str())?;
            t.set("message", message.as_str())?;
            t.set("x", *x)?;
            t.set("y", *y)?;
        }
        WorldEvent::GlobalLight { map_id, level } => {
            t.set("type", "global_light")?;
            t.set("map_id", *map_id)?;
            t.set("level", *level)?;
        }
        WorldEvent::Weather { map_id, weather_type, num_effects, temperature } => {
            t.set("type", "weather")?;
            t.set("map_id", *map_id)?;
            t.set("weather_type", *weather_type)?;
            t.set("num_effects", *num_effects)?;
            t.set("temperature", *temperature)?;
        }
        WorldEvent::Season { map_id, season, play_sound } => {
            t.set("type", "season")?;
            t.set("map_id", *map_id)?;
            t.set("season", *season)?;
            t.set("play_sound", *play_sound)?;
        }
        WorldEvent::Music { map_id, music_id } => {
            t.set("type", "music")?;
            t.set("map_id", *map_id)?;
            t.set("music_id", *music_id)?;
        }
        WorldEvent::MobileKilled { map_id, serial, corpse_serial, x, y, z, .. } => {
            t.set("type", "mobile_killed")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("corpse_serial", *corpse_serial)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
        }
        WorldEvent::PlayerDied { map_id, serial, corpse_serial, x, y, z, .. } => {
            t.set("type", "player_died")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("corpse_serial", *corpse_serial)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
        }
        WorldEvent::PlayerResurrected { map_id, serial, x, y, z, new_hits, max_hits, .. } => {
            t.set("type", "player_resurrected")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("z", *z)?;
            t.set("new_hits", *new_hits)?;
            t.set("max_hits", *max_hits)?;
        }
        WorldEvent::GhostVisibilityChanged { map_id, serial, visible, x, y, .. } => {
            t.set("type", "ghost_visibility_changed")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("visible", *visible)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
        }
        WorldEvent::DamageDealt { map_id, serial, source_serial, amount, new_hits, max_hits, x, y } => {
            t.set("type", "damage_dealt")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("source_serial", *source_serial)?;
            t.set("amount", *amount)?;
            t.set("new_hits", *new_hits)?;
            t.set("max_hits", *max_hits)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
        }
        WorldEvent::MobileHealed { map_id, serial, amount, new_hits, max_hits, x, y } => {
            t.set("type", "mobile_healed")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("amount", *amount)?;
            t.set("new_hits", *new_hits)?;
            t.set("max_hits", *max_hits)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
        }
        WorldEvent::ManaStaminaChanged { map_id, serial, mana, max_mana, stamina, max_stamina, x, y } => {
            t.set("type", "mana_stamina_changed")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *serial)?;
            t.set("mana", *mana)?;
            t.set("max_mana", *max_mana)?;
            t.set("stamina", *stamina)?;
            t.set("max_stamina", *max_stamina)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
        }
        WorldEvent::ContainerContentsUpdated { map_id, container_serial, x, y, changes } => {
            t.set("type", "container_contents_updated")?;
            t.set("map_id", *map_id)?;
            t.set("container_serial", *container_serial)?;
            t.set("x", *x)?;
            t.set("y", *y)?;
            t.set("change_count", changes.len())?;
        }
        WorldEvent::ShipMoved { map_id, ship_serial, ship_new_pos, passengers, .. } => {
            t.set("type", "ship_moved")?;
            t.set("map_id", *map_id)?;
            t.set("serial", *ship_serial)?;
            t.set("x", ship_new_pos.x)?;
            t.set("y", ship_new_pos.y)?;
            t.set("z", ship_new_pos.z)?;
            t.set("passenger_count", passengers.len())?;
        }
        WorldEvent::TargetedGump { .. }
        | WorldEvent::TargetedMessage { .. }
        | WorldEvent::TargetedCloseGump { .. }
        | WorldEvent::TargetedTargetCursor { .. }
        | WorldEvent::TargetedCrossWorldTeleport { .. }
        | WorldEvent::SnapshotRestored { .. }
        | WorldEvent::BaseStatChanged { .. } => {
            t.set("type", "targeted_event")?;
        }
    }
    Ok(t)
}

// ── Spell → Lua table conversion ─────────────────────────────────────────

fn spell_to_lua(lua: &Lua, spell: &crate::magic::SpellDef) -> LuaResult<LuaTable> {
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
    Ok(t)
}

// ── Equipment item parsing ───────────────────────────────────────────────

fn parse_equipment_items(
    params: &LuaTable,
    _owner_serial: u32,
    serial_alloc: &SerialAllocator,
) -> Vec<packets::world::EquippedItem> {
    let items_table: Option<LuaTable> = params.get("items").ok();
    let Some(items_table) = items_table else {
        return vec![];
    };

    let mut result = Vec::new();
    for pair in items_table.sequence_values::<LuaTable>() {
        let Ok(item) = pair else { continue };
        let graphic: u16 = item.get("graphic").unwrap_or(0);
        let layer_val: u8 = item.get("layer").unwrap_or(0);
        let color: Option<u16> = item.get("color").ok();
        let serial: u32 = item.get("serial").unwrap_or_else(|_| {
            serial_alloc.alloc_item().expect("item serial space exhausted")
        });

        let layer = packets::layer::Layer::from_wire(layer_val);
        result.push(packets::world::EquippedItem { serial, graphic, layer, color });
    }
    result
}

// ── ItemProps ↔ Lua conversion ───────────────────────────────────────────

fn item_props_to_lua(lua: &Lua, props: &ItemProps) -> LuaResult<LuaTable> {
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
    Ok(t)
}

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
    Ok(ItemProps {
        text: name.map(|n| ObjectText::with_title(n)).unwrap_or_default(),
        weight_override,
        meta,
    })
}
