//! Spawn points: zones that keep a population of monsters alive, respawning
//! them a configurable delay after they die.
//!
//! ## Design
//!
//! Spawn logic runs inside the worker's zone tick (see [`SpawnManager::tick`]),
//! mirroring the existing `sweep_poison` / `sweep_criminal_flags` pattern in
//! [`common::uo_engine::base_handler`].  This avoids the [`EntityController`](framework::anima::EntityController)
//! limitation that controllers cannot create new entities — only the handler
//! (which owns the zone and serial allocator) can spawn.
//!
//! ## Lifecycle
//!
//! * Spawn point *definitions* are loaded once at startup (from a built-in
//!   table or an optional JSON file) and are **not** persisted — after a
//!   restart the points reload and re-populate from scratch.
//! * Spawned monsters are *ephemeral*: they are ordinary world entities, but
//!   the manager never tries to save them.  If a `.clear` wipes a zone, the
//!   manager simply re-spawns up to `max_count` on the next tick.
//!
//! ## Death detection
//!
//! Each tick the manager checks which of its tracked serials still exist in
//! `zone.store`.  A serial that has vanished (the monster died and
//! `handle_kill_mobile` removed it) is dropped from the live set and a respawn
//! timer is queued for `respawn_delay_ms` later.
//!
//! ## AI
//!
//! Respawned monsters get a Rust [`MonsterController`](crate::controller_registry::MonsterController)
//! attached directly, and the controller's persistent ID is written to
//! `item_props.meta["controller"]` so the AI is restored after `.save`/`.load`
//! via the existing `SnapshotRestored` path.

use serde::{Deserialize, Serialize};

use framework::continuum::WorldEvent;
use framework::continuum::ZoneItemProps;
use framework::ecumene::Entity as EngineEntity;
use packets::layer::Layer;
use packets::movement::Notoriety;
use packets::world::EquippedItem;

use common::uo_engine::entity::{DemoEntity, MobileData};
use common::uo_engine::item_props::{ItemProps, MetaValue};
use common::uo_engine::serial_alloc::SerialAllocator;

use crate::controller_registry::MonsterCfg;
use crate::DemoZone;

// ── Meta keys ──────────────────────────────────────────────────────────────

/// Meta key on a spawned monster that records which spawn point or spawner
/// object created it.  Used to re-adopt the monster after a `.save`/`.load`
/// cycle so the manager does not double-spawn.
///
/// Value format:
/// - Static spawn point: the point's `id` string (stable across restarts).
/// - Dynamic spawner object: `"dyn:XXXXXXXX"` where `XXXXXXXX` is the
///   spawner item's serial in lower-case hex.
pub(crate) const META_SPAWN_ORIGIN: &str = "spawn_origin";

// ── Equipment piece ────────────────────────────────────────────────────────

/// One equipped item on a spawned monster (e.g. a weapon or armor piece).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquipPiece {
    pub graphic: u16,
    /// Wire layer byte (see `packets::layer::Layer`).
    pub layer: u8,
    #[serde(default)]
    pub color: u16,
}

// ── Mob template ─────────────────────────────────────────────────────────

/// Static definition of a monster kind that a spawn point can produce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobTemplate {
    /// Body graphic ID.
    pub graphic: u16,
    /// Display name.
    pub name: String,
    #[serde(default)]
    pub color: u16,
    /// Wire notoriety byte (1=Innocent … 6=Murderer).  Defaults to 6.
    #[serde(default = "default_notoriety")]
    pub notoriety: u8,
    /// Max hit points (current HP starts at this value).
    #[serde(default = "default_hits")]
    pub hits: u16,
    #[serde(default = "default_stat")]
    pub str_: u16,
    #[serde(default = "default_stat")]
    pub dex: u16,
    #[serde(default = "default_stat")]
    pub int_: u16,
    /// Equipped items (weapons, armor).
    #[serde(default)]
    pub items: Vec<EquipPiece>,
    // ── AI config ──
    #[serde(default = "default_aggro")]
    pub aggro_range: u16,
    #[serde(default = "default_leash")]
    pub leash_range: u16,
    #[serde(default = "default_dmg_min")]
    pub damage_min: u16,
    #[serde(default = "default_dmg_max")]
    pub damage_max: u16,
    #[serde(default = "default_swing")]
    pub swing_delay_ms: u64,
    /// Persistent controller id to attach to spawned monsters, e.g.
    /// `"monster:aggro=10,leash=20,dmg=8-18,swing=2500"`,
    /// `"lua:monster_ctrl.lua"`, or `"wander:5"`.
    ///
    /// When `None` (the default), a `"monster:<ai_cfg>"` id is built from the
    /// template's `aggro_range`/`leash_range`/`damage_*`/`swing_delay_ms`
    /// fields, preserving the original behaviour.  This makes the AI type a
    /// transparent, per-template choice resolved through
    /// [`crate::controller_registry::create_controller`].
    #[serde(default)]
    pub controller: Option<String>,
}

fn default_notoriety() -> u8 { 6 }
fn default_hits() -> u16 { 50 }
fn default_stat() -> u16 { 50 }
fn default_aggro() -> u16 { 10 }
fn default_leash() -> u16 { 20 }
fn default_dmg_min() -> u16 { 5 }
fn default_dmg_max() -> u16 { 15 }
fn default_swing() -> u64 { 2500 }

impl MobTemplate {
    fn ai_cfg(&self) -> MonsterCfg {
        MonsterCfg {
            aggro_range: self.aggro_range,
            leash_range: self.leash_range,
            damage_min: self.damage_min,
            damage_max: self.damage_max,
            swing_delay_ms: self.swing_delay_ms,
        }
    }

    /// Persistent controller id to attach to a monster spawned from this
    /// template.  Uses the explicit `controller` field when set, otherwise
    /// builds a `"monster:..."` id from the AI config fields.
    fn controller_id(&self) -> String {
        match &self.controller {
            Some(id) if !id.is_empty() => id.clone(),
            _ => self.ai_cfg().controller_id(),
        }
    }
}

// ── Spawn point definition ─────────────────────────────────────────────────

/// A spawn point: keeps `max_count` monsters of `template` alive within
/// `radius` tiles of `(x, y)` on map `map_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPoint {
    /// Human-readable identifier (for logging).
    pub id: String,
    pub map_id: u8,
    pub x: u16,
    pub y: u16,
    #[serde(default)]
    pub z: i8,
    /// Spawn scatter radius (tiles) around the center.
    #[serde(default = "default_radius")]
    pub radius: u16,
    /// Template key into [`MobTemplate`] table.
    pub template: String,
    /// How many monsters to keep alive at once.
    #[serde(default = "default_max_count")]
    pub max_count: u8,
    /// Delay (ms) after a monster dies before a replacement spawns.
    #[serde(default = "default_respawn_delay")]
    pub respawn_delay_ms: u64,
}

fn default_radius() -> u16 { 6 }
fn default_max_count() -> u8 { 2 }
fn default_respawn_delay() -> u64 { 15_000 }

/// File format for `--spawn-points`: templates + points in one document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    /// Named mob templates referenced by `SpawnPoint::template`.
    pub templates: std::collections::HashMap<String, MobTemplate>,
    /// The spawn points.
    pub points: Vec<SpawnPoint>,
}

// ── Runtime state ──────────────────────────────────────────────────────────

struct SpawnState {
    def: SpawnPoint,
    template: MobTemplate,
    /// Serials of monsters currently alive from this point.
    alive: Vec<u32>,
    /// Pending respawn deadlines (epoch ms).  Length = number of slots
    /// waiting to be refilled.
    respawn_at: Vec<u64>,
}

/// Runtime population state for a dynamic (object-backed) spawner.  Keyed by
/// the spawner object's serial.  Parameters live in the object's
/// `item_props.meta`; only the live/respawn tracking is held here.
#[derive(Default)]
struct DynSpawnState {
    /// Serials of monsters currently alive from this spawner.
    alive: Vec<u32>,
    /// Pending respawn deadlines (epoch ms).
    respawn_at: Vec<u64>,
}

/// Manager owning all spawn points for the server.  Lives in the handler and
/// is ticked once per worker tick per zone.
pub struct SpawnManager {
    points: Vec<SpawnState>,
    /// Template table, shared by static points and dynamic spawner objects.
    templates: std::collections::HashMap<String, MobTemplate>,
    /// Per-spawner-object runtime population state (ephemeral).
    dynamic: std::collections::HashMap<u32, DynSpawnState>,
    /// Map ids whose pre-existing (snapshot-restored) monsters have already
    /// been re-adopted into the live tracking.  Bootstrap for a zone is
    /// deferred until adoption completes so that restored monsters are not
    /// double-counted.
    adopted_zones: std::collections::HashSet<u8>,
    /// Map ids that have been flagged as needing re-adoption on the next tick
    /// (set by `adopt_zone`, cleared once the zone's bootstrap has run).
    /// Used by `has_pending_adopt` to keep the worker awake until all pending
    /// adoptions have been followed by at least one bootstrap tick.
    pending_adopt: std::collections::HashSet<u8>,
}

impl SpawnManager {
    /// Build a manager from a [`SpawnConfig`].  Spawn points referencing an
    /// unknown template are skipped with a warning.
    pub fn new(config: SpawnConfig) -> Self {
        let mut points = Vec::new();
        for def in config.points {
            match config.templates.get(&def.template) {
                Some(tpl) => points.push(SpawnState {
                    template: tpl.clone(),
                    def,
                    alive: Vec::new(),
                    respawn_at: Vec::new(),
                }),
                None => {
                    log::warn!(
                        "[spawn] point {:?} references unknown template {:?} — skipped",
                        def.id, def.template,
                    );
                }
            }
        }
        log::info!("[spawn] {} spawn point(s) active", points.len());
        Self {
            points,
            templates: config.templates,
            dynamic: std::collections::HashMap::new(),
            adopted_zones: std::collections::HashSet::new(),
            pending_adopt: std::collections::HashSet::new(),
        }
    }

    /// Empty manager (no spawn points), but still carrying the built-in
    /// template table so admin spawner objects can be configured.
    pub fn empty() -> Self {
        Self {
            points: Vec::new(),
            templates: default_config().templates,
            dynamic: std::collections::HashMap::new(),
            adopted_zones: std::collections::HashSet::new(),
            pending_adopt: std::collections::HashSet::new(),
        }
    }

    fn now_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Earliest pending respawn deadline across all points, so the worker can
    /// schedule a wakeup.  Returns `None` if nothing is pending.
    pub fn next_tick_at(&self) -> Option<tokio::time::Instant> {
        let now = Self::now_ms();
        let mut soonest: Option<u64> = None;
        for p in &self.points {
            for &due in &p.respawn_at {
                soonest = Some(soonest.map_or(due, |s| s.min(due)));
            }
        }
        for d in self.dynamic.values() {
            for &due in &d.respawn_at {
                soonest = Some(soonest.map_or(due, |s| s.min(due)));
            }
        }
        soonest.map(|due| {
            let delay = due.saturating_sub(now);
            tokio::time::Instant::now() + std::time::Duration::from_millis(delay)
        })
    }

    /// Returns `true` while there are zones that have been adopted from a
    /// snapshot but have not yet had their bootstrap tick run.  Used by
    /// `DemoHandler::next_tick_at` to keep the worker awake until the
    /// bootstrap has had a chance to execute.
    pub fn has_pending_adopt(&self) -> bool {
        !self.pending_adopt.is_empty()
    }

    /// Re-adopt monsters that were restored from a snapshot into the spawn
    /// manager's live tracking.
    ///
    /// Called by `DemoHandler::tick` immediately after `rebuild_active_sets`
    /// (i.e. in the same tick where `RestoreSnapshot` was processed), so the
    /// zone is guaranteed to be fully populated.
    ///
    /// The method scans `zone.item_props` for entries carrying
    /// [`META_SPAWN_ORIGIN`] and routes each found mobile back to its parent
    /// static point or dynamic spawner.  After adoption the zone is marked as
    /// ready for bootstrap (`adopted_zones`), and `pending_adopt` is set so
    /// the worker fires one more tick for the bootstrap to run.
    pub fn adopt_zone(&mut self, zone: &DemoZone) {
        let map_id = zone.map_id;

        // Collect (monster_serial, origin_key) for all mobiles that carry a
        // spawn-origin meta and still exist in the zone.
        let found: Vec<(u32, String)> = zone.item_props.iter()
            .filter_map(|(&serial, props)| {
                let origin = props.get_meta_str(META_SPAWN_ORIGIN)?;
                // Only live mobiles — items and dead mobiles are not tracked.
                zone.store.get(serial)?.mobile()?;
                Some((serial, origin.to_string()))
            })
            .collect();

        let mut adopted = 0usize;
        for (serial, origin) in &found {
            if let Some(rest) = origin.strip_prefix("dyn:") {
                // Dynamic spawner: parse the hex serial.
                if let Ok(spawner_serial) = u32::from_str_radix(rest, 16) {
                    self.dynamic
                        .entry(spawner_serial)
                        .or_default()
                        .alive
                        .push(*serial);
                    adopted += 1;
                }
            } else {
                // Static spawn point: match by id on this map.
                if let Some(p) = self.points.iter_mut()
                    .find(|p| p.def.map_id == map_id && p.def.id == *origin)
                {
                    p.alive.push(*serial);
                    adopted += 1;
                }
            }
        }

        if adopted > 0 {
            log::info!(
                "[spawn] map {} — re-adopted {} monster(s) from snapshot",
                map_id, adopted,
            );
        }

        // Mark the zone as adopted.  Any subsequent bootstrap tick will see the
        // restored monsters in `alive` and not over-spawn.
        self.adopted_zones.insert(map_id);
        // Signal the worker to fire one more tick so bootstrap can run.
        self.pending_adopt.insert(map_id);
    }

    /// Tick all spawn points whose `map_id` matches this `zone`.
    ///
    /// `restore_pending` should be `true` when the handler knows a
    /// `RestoreSnapshot` has been processed for this zone but the spawn
    /// manager's `adopt_zone` has not yet run (i.e. `sets_need_rescan > 0`
    /// in the handler).  While `true` the bootstrap step is skipped so that
    /// restored monsters are not double-counted.
    ///
    /// `attach` is a callback that attaches an AI controller to a serial,
    /// given the controller's persistent id string (e.g. `"monster:..."` or
    /// `"lua:script.lua"`).  The handler resolves the id through the
    /// controller registry and wires `BaseHandler::host.attach` + equipment
    /// indexing — see `DemoHandler::tick`.
    pub fn tick<F>(
        &mut self,
        zone: &mut DemoZone,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
        serial_alloc: &SerialAllocator,
        restore_pending: bool,
        mut attach: F,
    ) where
        F: FnMut(u32, &str),
    {
        let now = Self::now_ms();
        let map_id = zone.map_id;

        // If this zone has not been adopted yet (i.e. it might contain
        // snapshot-restored monsters not yet in our tracking) decide whether
        // to adopt now or wait for the restore to complete.
        if !self.adopted_zones.contains(&map_id) {
            if restore_pending {
                // RestoreSnapshot has not landed yet — skip everything for
                // this zone until the next tick after adopt_zone is called.
                return;
            }
            // No snapshot restore in flight: adopt immediately (there is
            // nothing to re-adopt from a fresh start, but we still need to
            // mark the zone as ready for bootstrap).
            self.adopt_zone(zone);
            // adopt_zone inserts into pending_adopt; clear it right away so
            // the worker does not spin — we are about to run bootstrap below.
            self.pending_adopt.remove(&map_id);
        }

        // Clear any pending-adopt flag for this zone: bootstrap is now running.
        self.pending_adopt.remove(&map_id);

        for point in &mut self.points {
            if point.def.map_id != map_id {
                continue;
            }

            // 1. Reap dead monsters: any tracked serial no longer in the store
            //    has died — queue a respawn.
            let respawn_delay = point.def.respawn_delay_ms;
            point.alive.retain(|&serial| {
                if zone.store.get(serial).is_some() {
                    true
                } else {
                    point.respawn_at.push(now + respawn_delay);
                    false
                }
            });

            // 2. Bootstrap: if (alive + pending) is below max_count (e.g. at
            //    startup or after a `.clear`), queue immediate spawns.
            let accounted = point.alive.len() + point.respawn_at.len();
            if accounted < point.def.max_count as usize {
                for _ in 0..(point.def.max_count as usize - accounted) {
                    point.respawn_at.push(now);
                }
            }

            // 3. Fire due respawn timers.
            let mut still_pending = Vec::new();
            let due: Vec<u64> = std::mem::take(&mut point.respawn_at);
            for deadline in due {
                if deadline > now {
                    still_pending.push(deadline);
                    continue;
                }
                match Self::spawn_one(
                    point.def.x, point.def.y, point.def.z,
                    point.def.radius, &point.template, &point.def.id,
                    &point.def.id,
                    zone, event_tx, serial_alloc, &mut attach,
                ) {
                    Some(serial) => point.alive.push(serial),
                    None => {
                        // Serial space exhausted or position invalid — retry
                        // shortly instead of dropping the slot.
                        still_pending.push(now + 1_000);
                    }
                }
            }
            point.respawn_at = still_pending;
        }

        // ── Dynamic spawner objects ───────────────────────────────────────
        self.tick_dynamic(zone, event_tx, serial_alloc, &mut attach, now, map_id);
    }

    /// Drive object-backed spawners on this `map_id`.  Parameters are read
    /// from each spawner object's `item_props.meta`; live/respawn tracking is
    /// held in `self.dynamic`.  Spawners flagged for deletion (or whose object
    /// has vanished) are removed here, emitting `EntityRemoved`.
    fn tick_dynamic<F>(
        &mut self,
        zone: &mut DemoZone,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
        serial_alloc: &SerialAllocator,
        attach: &mut F,
        now: u64,
        map_id: u8,
    ) where
        F: FnMut(u32, &str),
    {
        use crate::spawner_object::{SpawnerParams, META_SPAWN_TEMPLATE};

        // Collect spawner objects on this map: (serial, params, x, y, z, delete).
        struct Found {
            serial: u32,
            params: Option<SpawnerParams>,
            x: u16,
            y: u16,
            z: i8,
            delete: bool,
        }

        // Pass 1: read spawner meta (borrows zone.item_props only).
        struct Meta {
            serial: u32,
            params: Option<SpawnerParams>,
            delete: bool,
        }
        let metas: Vec<Meta> = zone.item_props.iter()
            .filter_map(|(&serial, props)| {
                props.get_meta_str(META_SPAWN_TEMPLATE)?;
                Some(Meta {
                    serial,
                    params: SpawnerParams::from_props(props),
                    delete: SpawnerParams::is_delete_flagged(props),
                })
            })
            .collect();

        // Pass 2: resolve positions (borrows zone.store only).
        let mut found: Vec<Found> = Vec::new();
        for m in metas {
            let Some(entity) = zone.store.get(m.serial) else { continue };
            let Some(item) = entity.item() else { continue };
            found.push(Found {
                serial: m.serial,
                params: m.params,
                x: item.x,
                y: item.y,
                z: item.z,
                delete: m.delete,
            });
        }

        // Forget tracking for spawners that no longer exist on this map.
        let live_serials: std::collections::HashSet<u32> =
            found.iter().map(|f| f.serial).collect();
        self.dynamic.retain(|serial, _| live_serials.contains(serial));

        let mut delete_serials: Vec<u32> = Vec::new();

        for f in &found {
            if f.delete {
                delete_serials.push(f.serial);
                continue;
            }
            let Some(params) = &f.params else { continue };
            let Some(tpl) = self.templates.get(&params.template).cloned() else { continue };

            // Pull this spawner's tracking out of the map so we don't hold a
            // borrow of `self.dynamic` across `spawn_one`.
            let mut st = self.dynamic.remove(&f.serial).unwrap_or_default();

            // 1. Reap dead monsters.
            let respawn_delay = params.respawn_delay_ms;
            let mut reaped = Vec::new();
            st.alive.retain(|&s| {
                if zone.store.get(s).is_some() {
                    true
                } else {
                    reaped.push(now + respawn_delay);
                    false
                }
            });
            st.respawn_at.extend(reaped);

            // 2. Bootstrap up to max_count (only when enabled).
            if params.enabled {
                let accounted = st.alive.len() + st.respawn_at.len();
                if accounted < params.max_count as usize {
                    for _ in 0..(params.max_count as usize - accounted) {
                        st.respawn_at.push(now);
                    }
                }
            }

            // 3. Fire due respawn timers (only when enabled).
            let mut still_pending = Vec::new();
            let due: Vec<u64> = std::mem::take(&mut st.respawn_at);
            for deadline in due {
                if !params.enabled {
                    // Drop pending spawns while disabled.
                    continue;
                }
                if deadline > now {
                    still_pending.push(deadline);
                    continue;
                }
                let label = format!("spawner:0x{:08X}", f.serial);
                let origin = format!("dyn:{:08x}", f.serial);
                match Self::spawn_one(
                    f.x, f.y, f.z, params.radius, &tpl, &label, &origin,
                    zone, event_tx, serial_alloc, attach,
                ) {
                    Some(serial) => st.alive.push(serial),
                    None => still_pending.push(now + 1_000),
                }
            }
            st.respawn_at = still_pending;
            self.dynamic.insert(f.serial, st);
        }

        // Remove deleted spawners and emit EntityRemoved.
        for serial in delete_serials {
            let last_pos = zone.store.get(serial)
                .map(EngineEntity::pos)
                .unwrap_or(u_core::Pos3D::new(0, 0, 0));
            zone.remove(serial);
            zone.item_props.remove(serial);
            self.dynamic.remove(&serial);
            let _ = event_tx.send(WorldEvent::EntityRemoved {
                map_id,
                serial,
                last_pos,
            });
            log::info!("[spawn] removed spawner 0x{:08X}", serial);
        }
    }

    /// Spawn a single monster of `tpl` scattered within `radius` tiles of
    /// `(cx, cy, cz)`.  Returns the new serial on success.
    ///
    /// `label` is used for log messages.  `origin` is the stable parent
    /// identifier written to the monster's `item_props.meta` under
    /// [`META_SPAWN_ORIGIN`] so it can be re-adopted after a `.save`/`.load`.
    #[allow(clippy::too_many_arguments)]
    fn spawn_one<F>(
        cx: u16,
        cy: u16,
        cz: i8,
        radius: u16,
        tpl: &MobTemplate,
        label: &str,
        origin: &str,
        zone: &mut DemoZone,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
        serial_alloc: &SerialAllocator,
        attach: &mut F,
    ) -> Option<u32>
    where
        F: FnMut(u32, &str),
    {
        let serial = serial_alloc.alloc_mobile()?;

        // Pick a scattered position within radius, validate standing Z.
        let (x, y, z) = Self::pick_position(cx, cy, cz, radius, zone);

        let notoriety = notoriety_from_u8(tpl.notoriety);

        // Build equipment with freshly allocated item serials.
        let items: Vec<EquippedItem> = tpl.items.iter().filter_map(|e| {
            let item_serial = serial_alloc.alloc_item()?;
            Some(EquippedItem {
                serial: item_serial,
                graphic: e.graphic,
                layer: Layer::from_wire(e.layer),
                color: if e.color == 0 { None } else { Some(e.color) },
            })
        }).collect();

        let entity = DemoEntity::Mobile(MobileData {
            serial,
            graphic: tpl.graphic,
            x, y, z,
            direction: 0,
            color: tpl.color,
            status: packets::mobile_flags::MobileFlags(0),
            notoriety,
            items,
            name: tpl.name.clone(),
            hits: tpl.hits,
            hits_max: tpl.hits,
            mana: tpl.hits,
            mana_max: tpl.hits,
            stamina: tpl.hits,
            stamina_max: tpl.hits,
            str_: tpl.str_,
            dex: tpl.dex,
            int: tpl.int_,
            is_player: false,
            dead: false,
            living_graphic: 0,
            noto_class: noto_class_from_wire(notoriety),
            ..Default::default()
        });

        let pos = EngineEntity::pos(&entity);
        let snap = entity.snapshot();
        zone.spawn(serial, entity);

        // Persist the AI controller id so it survives .save/.load and so the
        // restore path can rebuild any controller type (monster, lua, …).
        let controller_id = tpl.controller_id();
        let mut props = ItemProps::with_name(&tpl.name);
        props.meta.insert(
            "controller".to_string(),
            MetaValue::Str(controller_id.clone()),
        );
        // Record the parent spawn point / spawner so the manager can re-adopt
        // this monster after a `.save` / `--load` restart instead of
        // double-spawning.
        props.meta.insert(
            META_SPAWN_ORIGIN.to_string(),
            MetaValue::Str(origin.to_string()),
        );
        // Also expose the AI tuning via meta so script-based controllers
        // (e.g. `scripts/monster_ctrl.lua`) can read the template's values.
        // Rust controllers decode their params from the id and ignore these.
        props.meta.insert("aggro_range".to_string(), MetaValue::Int(tpl.aggro_range as i64));
        props.meta.insert("leash_range".to_string(), MetaValue::Int(tpl.leash_range as i64));
        props.meta.insert(
            "melee_damage".to_string(),
            MetaValue::Str(format!("{},{}", tpl.damage_min, tpl.damage_max)),
        );
        props.meta.insert("swing_delay".to_string(), MetaValue::Int(tpl.swing_delay_ms as i64));
        zone.item_props.insert(serial, props);

        // Stream the new entity to observers in view.
        let _ = event_tx.send(WorldEvent::EntitySpawned {
            map_id: zone.map_id,
            serial,
            pos,
            entity: snap,
        });

        // Attach the AI controller.  The handler resolves `controller_id`
        // through the controller registry (host.attach + equipment indexing).
        attach(serial, &controller_id);

        log::debug!(
            "[spawn] {:?} spawned {:?} 0x{:08X} at ({},{},{})",
            label, tpl.name, serial, x, y, z,
        );

        Some(serial)
    }

    /// Pick a spawn position within `radius` tiles of `(cx, cy, cz)`,
    /// validating the standing Z via the zone.  Falls back to the center on
    /// failure.
    fn pick_position(cx: u16, cy: u16, cz: i8, radius: u16, zone: &DemoZone) -> (u16, u16, i8) {
        use rand::Rng;
        let mut rng = rand::rng();
        let r = radius as i32;
        if r > 0 {
            for _ in 0..8 {
                let ox = rng.random_range(-r..=r);
                let oy = rng.random_range(-r..=r);
                let x = (cx as i32 + ox).clamp(0, u16::MAX as i32) as u16;
                let y = (cy as i32 + oy).clamp(0, u16::MAX as i32) as u16;
                if let Some(z) = zone.resolve_standing_z(x, y, cz, u_core::Heading::North) {
                    return (x, y, z);
                }
            }
        }
        // Fallback: spawn at center even if Z could not be resolved.
        let z = zone
            .resolve_standing_z(cx, cy, cz, u_core::Heading::North)
            .unwrap_or(cz);
        (cx, cy, z)
    }
}

// ── Notoriety helpers (mirrors lua_script::runtime) ─────────────────────────

fn notoriety_from_u8(val: u8) -> Notoriety {
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

// ── Built-in default config ─────────────────────────────────────────────────

/// Default spawn config used when no `--spawn-points` file is supplied.
///
/// Mirrors the monsters from `scripts/spawn_monster.lua`: an orc, a skeleton,
/// and a troll near Britain.
pub fn default_config() -> SpawnConfig {
    use std::collections::HashMap;

    let mut templates = HashMap::new();

    templates.insert("orc".to_string(), MobTemplate {
        graphic: 0x0011, // orc body
        name: "an orc".to_string(),
        color: 0,
        notoriety: 6,
        hits: 80,
        str_: 60, dex: 50, int_: 30,
        items: vec![
            EquipPiece { graphic: 0x13B9, layer: 0x01, color: 0 }, // leather cap
            EquipPiece { graphic: 0x13CC, layer: 0x0D, color: 0 }, // leather tunic
            EquipPiece { graphic: 0x0F5E, layer: 0x02, color: 0 }, // broadsword
        ],
        aggro_range: 10,
        leash_range: 20,
        damage_min: 8,
        damage_max: 18,
        swing_delay_ms: 2500,
        controller: None,
    });

    templates.insert("skeleton".to_string(), MobTemplate {
        graphic: 0x0032, // skeleton body
        name: "a skeleton".to_string(),
        color: 0,
        notoriety: 6,
        hits: 50,
        str_: 50, dex: 50, int_: 30,
        items: vec![],
        aggro_range: 8,
        leash_range: 15,
        damage_min: 4,
        damage_max: 12,
        swing_delay_ms: 3000,
        controller: None,
    });

    templates.insert("troll".to_string(), MobTemplate {
        graphic: 0x00D2, // troll body
        name: "a troll".to_string(),
        color: 0,
        notoriety: 6,
        hits: 120,
        str_: 90, dex: 40, int_: 30,
        items: vec![],
        aggro_range: 8,
        leash_range: 18,
        damage_min: 12,
        damage_max: 25,
        swing_delay_ms: 3000,
        controller: None,
    });

    // Example Lua-driven monster: identical body to the orc but its AI runs
    // from `scripts/monster_ctrl.lua` instead of the built-in Rust
    // MonsterController.  Spawn it via `.spawner lua-orc`.
    templates.insert("lua-orc".to_string(), MobTemplate {
        graphic: 0x0011, // orc body
        name: "a lua orc".to_string(),
        color: 0,
        notoriety: 6,
        hits: 80,
        str_: 60, dex: 50, int_: 30,
        items: vec![
            EquipPiece { graphic: 0x0F5E, layer: 0x02, color: 0 }, // broadsword
        ],
        aggro_range: 10,
        leash_range: 20,
        damage_min: 8,
        damage_max: 18,
        swing_delay_ms: 2500,
        controller: Some("lua:monster_ctrl.lua".to_string()),
    });

    let points = vec![
        SpawnPoint {
            id: "britain-orcs".to_string(),
            map_id: 0,
            x: 1440, y: 1700, z: 0,
            radius: 6,
            template: "orc".to_string(),
            max_count: 2,
            respawn_delay_ms: 15_000,
        },
        SpawnPoint {
            id: "britain-skeletons".to_string(),
            map_id: 0,
            x: 1445, y: 1705, z: 0,
            radius: 5,
            template: "skeleton".to_string(),
            max_count: 2,
            respawn_delay_ms: 20_000,
        },
        SpawnPoint {
            id: "britain-troll".to_string(),
            map_id: 0,
            x: 1450, y: 1710, z: 0,
            radius: 4,
            template: "troll".to_string(),
            max_count: 1,
            respawn_delay_ms: 30_000,
        },
        // ── World 1 (a second facet, e.g. Trammel) ──────────────────────
        //
        // These points exercise the multi-world support: the worker
        // auto-creates zone 1 on the first spawn tick, and players reaching
        // it (via `.world 1`, a teleporter, or a cross-world recall) find it
        // populated independently of world 0.
        SpawnPoint {
            id: "world1-orcs".to_string(),
            map_id: 1,
            x: 1440, y: 1700, z: 0,
            radius: 8,
            template: "orc".to_string(),
            max_count: 3,
            respawn_delay_ms: 15_000,
        },
        SpawnPoint {
            id: "world1-trolls".to_string(),
            map_id: 1,
            x: 1460, y: 1720, z: 0,
            radius: 6,
            template: "troll".to_string(),
            max_count: 2,
            respawn_delay_ms: 25_000,
        },
    ];

    SpawnConfig { templates, points }
}

/// Load a [`SpawnConfig`] from a JSON file.
pub fn load_config(path: &std::path::Path) -> Result<SpawnConfig, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {}", path.display(), e))?;
    serde_json::from_str(&data)
        .map_err(|e| format!("parse {}: {}", path.display(), e))
}
