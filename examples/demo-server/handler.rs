//! Demo-server command handler: [`DemoHandler`] and the [`DemoZone`] type alias.
//!
//! `DemoHandler` implements [`CommandHandler`] for the demo server.  It
//! delegates shared logic to [`BaseHandler`](common::uo_engine::base_handler::BaseHandler) and adds demo-specific
//! broadcast commands, monster spawn-point management and ship sailing.

use std::sync::Arc;

use framework::continuum::{
    CommandHandler, HashContainerStore, HashItemProps, Zone, ZoneContainers, ZoneItemProps,
};
use framework::continuum::WorldEvent;
use framework::ecumene::Entity;
use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::{EngineCommand, ResolvedItemName};
use common::uo_engine::item_props::ItemProps;
use common::uo_engine::serial_alloc::SerialAllocator;
use common::uo_engine::stackable::is_stackable;
use crate::commands::DemoCommand;
use crate::controller_registry;
use crate::equipment_calc::{compute_armor_profile, compute_backpack_gold, compute_mobile_weight, compute_skill_bonuses};
use crate::spawn_points;

// ── DemoZone type alias ───────────────────────────────────────────────────

/// Type alias for the demo server's zone with item properties.
pub(crate) type DemoZone = Zone<DemoEntity, HashContainerStore, HashItemProps<ItemProps>>;

// ── ZoneTickState ─────────────────────────────────────────────────────────

/// Per-zone tick state for ship sailing and door auto-close.
///
/// A serial only ever lives in one zone's `item_props`, and each zone
/// schedules its own next wake.  Keeping this state per-zone (rather than as
/// flat fields on the handler) prevents one zone's `tick` from wiping another
/// zone's open doors / sailing ships — the worker ticks every zone with the
/// same `&mut handler`, so shared sets would be cross-contaminated.
#[derive(Default)]
struct ZoneTickState {
    /// Serials of ships under way (carry `META_SAIL_HEADING`).
    active_ships: std::collections::HashSet<u32>,
    /// Serials of open doors (carry `META_DOOR_CLOSE_AT`).
    open_doors: std::collections::HashSet<u32>,
    /// Next ship-sailing tick for this zone.
    next_sail_tick: Option<tokio::time::Instant>,
    /// Next door auto-close tick for this zone.
    next_door_tick: Option<tokio::time::Instant>,
}

impl ZoneTickState {
    /// Whether this state carries no pending work and can be dropped from the
    /// per-zone map (idle zones must not accumulate).
    fn is_idle(&self) -> bool {
        self.active_ships.is_empty()
            && self.open_doors.is_empty()
            && self.next_sail_tick.is_none()
            && self.next_door_tick.is_none()
    }
}

// ── DemoHandler ───────────────────────────────────────────────────────────

/// `CommandHandler` that delegates shared commands to [`BaseHandler`](common::uo_engine::base_handler::BaseHandler)
/// and handles demo-server–specific broadcast commands.
pub(crate) struct DemoHandler {
    pub(crate) base: common::uo_engine::base_handler::BaseHandler,
    /// Monster spawn-point manager (ticked each zone tick).
    spawn_mgr: spawn_points::SpawnManager,
    /// Base directory for Lua scripts, used to resolve `"lua:..."` controller
    /// ids when spawning monsters.
    scripts_dir: std::path::PathBuf,
    /// Per-zone door/ship tick state, keyed by `map_id`.
    ///
    /// The worker ticks every zone with the same `&mut handler`, so this
    /// state must be per-zone to avoid one zone's tick wiping another zone's
    /// open doors / sailing ships.  Created lazily and pruned when idle.
    zone_state: std::collections::HashMap<u8, ZoneTickState>,
    /// Counts zones whose active-ship / open-door sets need rebuilding.
    ///
    /// Incremented by one each time a `RestoreSnapshot` is observed (a
    /// restore repopulates `zone.item_props` directly, bypassing the
    /// `SetItemProps` intercept that normally maintains the sets).
    /// Decremented by one each tick after `rebuild_active_sets` runs,
    /// so every pending zone gets exactly one rescan even when several
    /// zones are restored in a single startup (`--load`).
    sets_need_rescan: u8,
    /// Resource-node depletion / regeneration state (mining, lumberjacking, …).
    ///
    /// World-level state owned by the worker; the session asks it to harvest
    /// via [`DemoCommand::TryHarvestResource`].  Only partially-used nodes are
    /// tracked; recovered nodes are pruned (see [`crate::resource_nodes`]).
    resource_nodes: crate::resource_nodes::NodeMap,
    /// Next instant at which a resource node needs servicing (regen sweep), or
    /// `None` when no nodes are being tracked.
    next_resource_tick: Option<tokio::time::Instant>,
}

impl DemoHandler {
    pub(crate) fn new(
        event_rx: tokio::sync::mpsc::UnboundedReceiver<WorldEvent>,
        serial_alloc: Arc<SerialAllocator>,
        spawn_mgr: spawn_points::SpawnManager,
        scripts_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            base: common::uo_engine::base_handler::BaseHandler::new(event_rx, serial_alloc),
            spawn_mgr,
            scripts_dir,
            zone_state: std::collections::HashMap::new(),
            sets_need_rescan: 0,
            resource_nodes: crate::resource_nodes::NodeMap::new(),
            next_resource_tick: None,
        }
    }

    /// Populate the equipment reverse index from all mobiles in a zone.
    ///
    /// Must be called after directly populating a zone via `zone.spawn()`
    /// (e.g. during startup from `.uolog` data).
    pub(crate) fn index_zone_equipment(&mut self, zone: &DemoZone) {
        self.base.index_zone_equipment(zone);
    }
}

// ── intercept_engine_command ──────────────────────────────────────────────

/// Intercept item-property and demo-specific engine commands.
///
/// `EngineHandler` is generic over `P: ZoneItemProps` and cannot access the
/// concrete `HashItemProps<ItemProps>` type.  This function handles the
/// commands that require knowledge of the concrete zone type: item props
/// get/set, weight computation, and armor queries.
///
/// Returns `None` if the command was consumed, or `Some(cmd)` to let
/// the engine handle it.
fn intercept_engine_command(
    engine_cmd: EngineCommand,
    zone: &mut DemoZone,
    zst: &mut ZoneTickState,
    sets_need_rescan: &mut u8,
) -> Option<EngineCommand> {
    use crate::game_session::shipping::{META_SAIL_HEADING, META_SAIL_LAST_MOVE, SAIL_TICK_MS};
    use crate::doors::META_DOOR_CLOSE_AT;

    match engine_cmd {
        EngineCommand::GetItemProps { serial, reply } => {
            let _ = reply.send(zone.item_props.get(serial).cloned());
            None
        }
        EngineCommand::SetItemProps { serial, props } => {
            match props {
                Some(p) => {
                    // Keep the active-ship / open-door sets in sync with the
                    // meta written by the session side (sailing, doors), so
                    // the periodic ticks iterate only active objects.  Also
                    // arm the matching `next_*_tick` so the worker wakes for
                    // the auto-close / sail move even in an otherwise idle
                    // world (`next_tick_at` is recomputed after each command).
                    if p.meta.contains_key(META_SAIL_HEADING) {
                        zst.active_ships.insert(serial);
                        // Next move is scheduled `SAIL_TICK_MS` after the last
                        // recorded move (or now, if this is a fresh launch).
                        let last_move = p.meta.get(META_SAIL_LAST_MOVE).and_then(|m| match m {
                            common::uo_engine::item_props::MetaValue::Int(v) => Some(*v),
                            _ => None,
                        });
                        let due_ms = match last_move {
                            Some(lm) => lm + SAIL_TICK_MS,
                            None => sail_clock_now_ms(),
                        };
                        let inst = sail_instant_from_clock_ms(due_ms);
                        zst.next_sail_tick =
                            Some(zst.next_sail_tick.map_or(inst, |e| e.min(inst)));
                    } else {
                        zst.active_ships.remove(&serial);
                    }
                    if let Some(common::uo_engine::item_props::MetaValue::Int(close_at)) =
                        p.meta.get(META_DOOR_CLOSE_AT)
                    {
                        zst.open_doors.insert(serial);
                        let inst = door_instant_from_clock_ms(*close_at);
                        zst.next_door_tick =
                            Some(zst.next_door_tick.map_or(inst, |e| e.min(inst)));
                    } else {
                        zst.open_doors.remove(&serial);
                    }
                    zone.item_props.insert(serial, p);
                }
                None => {
                    zst.active_ships.remove(&serial);
                    zst.open_doors.remove(&serial);
                    zone.item_props.remove(serial);
                }
            }
            None
        }
        EngineCommand::ResolveItemName { serial, reply } => {
            let _ = reply.send(resolve_item_name(zone, serial));
            None
        }
        EngineCommand::ComputeWeight { serial, held_item, reply } => {
            let _ = reply.send(compute_mobile_weight(zone, serial, held_item));
            None
        }
        EngineCommand::QueryEquipmentArmor { serial, reply } => {
            let _ = reply.send(compute_armor_profile(zone, serial));
            None
        }
        EngineCommand::QuerySkills { serial, reply } => {
            let skills = zone.store.get(serial)
                .and_then(|e| e.mobile())
                .map(|m| m.skills.clone());
            let _ = reply.send(skills);
            None
        }
        EngineCommand::SetSkillLock { serial, skill_id, lock, reply } => {
            let updated = zone.store.get_mut(serial)
                .and_then(|e| e.mobile_mut())
                .and_then(|m| m.skills.get_mut(&skill_id))
                .map(|sv| { sv.lock = lock; *sv });
            let _ = reply.send(updated);
            None
        }
        EngineCommand::QuerySkillBonuses { serial, reply } => {
            let _ = reply.send(compute_skill_bonuses(zone, serial));
            None
        }
        EngineCommand::CountGold { serial, held_item, reply } => {
            let _ = reply.send(compute_backpack_gold(zone, serial, held_item));
            None
        }
        other => {
            // A snapshot restore repopulates `zone.item_props` directly inside
            // the base handler (bypassing the `SetItemProps` intercept above),
            // so the active-ship / open-door sets must be rebuilt afterwards.
            // Flag it here and let the base handler consume the command.
            if matches!(other, EngineCommand::RestoreSnapshot { .. }) {
                *sets_need_rescan = sets_need_rescan.saturating_add(1);
            }
            Some(other)
        }
    }
}

// ── Item name resolution ──────────────────────────────────────────────────

/// Locate an item by serial across all storage tiers, returning its
/// `(graphic, color, amount)` if it exists.
///
/// Search order mirrors `item_ops::find_item_info`:
/// 1. Top-level entities (`zone.store`).
/// 2. Equipped items on mobiles (`MobileData::items`, amount is always 1).
/// 3. Items inside containers (`zone.containers`).
fn locate_item_graphic(zone: &DemoZone, serial: u32) -> Option<(u16, u16, u16)> {
    // 1. Top-level entity.
    if let Some(DemoEntity::Item { graphic, color, amount, .. }) = zone.store.get(serial) {
        return Some((*graphic, *color, *amount));
    }

    // 2. Equipped items — scan mobiles' equipment lists.
    for (_, entity) in zone.store.iter() {
        if let DemoEntity::Mobile(m) = entity {
            if let Some(eq) = m.items.iter().find(|i| i.serial == serial) {
                return Some((eq.graphic, eq.color.unwrap_or(0), 1));
            }
        }
    }

    // 3. Items inside containers.
    if let Some(cs) = zone.containers.find_container_of_item(serial) {
        if let Some(container) = zone.containers.get(cs) {
            if let Some(item) = container.find_item(serial) {
                return Some((item.graphic, item.color, item.amount));
            }
        }
    }

    None
}

/// Expand UO tiledata plural markers in an item name against a stack `amount`.
///
/// `tiledata.mul` names embed inline pluralisation hints delimited by `%`.
/// The token *between* a pair of `%` is chosen by count:
///
/// - `%s%`            → `"s"` when `amount != 1`, otherwise empty
///   (`"bandage%s%"` → `"bandages"` / `"bandage"`).
/// - `%suffix%`       → `"suffix"` when plural, empty when singular
///   (a bare suffix to append, e.g. `%es%`).
/// - `%singular/plural%` → the left side when `amount == 1`, the right side
///   otherwise (`"piece%s/ces%"`-style alternations also work: the `/`
///   splits the two count forms).
///
/// Names without any `%…%` marker are returned unchanged.  An unterminated
/// trailing `%` (malformed data) is dropped.
fn expand_plural_markers(name: &str, amount: u16) -> String {
    if !name.contains('%') {
        return name.to_string();
    }

    let plural = amount != 1;
    let mut out = String::with_capacity(name.len());
    let mut rest = name;

    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('%') {
            Some(close) => {
                let token = &after[..close];
                let chosen = match token.split_once('/') {
                    // `%singular/plural%`
                    Some((singular, plural_form)) => {
                        if plural { plural_form } else { singular }
                    }
                    // `%suffix%` (incl. `%s%`): suffix only when plural.
                    None => {
                        if plural { token } else { "" }
                    }
                };
                out.push_str(chosen);
                rest = &after[close + 1..];
            }
            // Unterminated marker — drop the trailing `%` and stop.
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Resolve an item's display name through the full chain:
///
/// 1. `ItemProps::name` — explicit per-instance name (treated as a proper
///    noun: `explicit_name = true`, never gets a stack-count prefix).  This
///    is also where rare per-instance variants (e.g. a "Vanquishing" weapon
///    or a `+25` accuracy item) carry their bespoke name.
/// 2. Poison bottle — named by its per-instance level from `ItemProps.meta`
///    (`poison_level`); all four levels share one graphic/hue.
/// 3. `lookup_potion(graphic, color)` — our other potion bottles, named by
///    their `(graphic, color)` pair.  Distinguishes e.g. mana/strength (same
///    graphic, different hue).  Foreign bottles whose pair is not in the
///    table fall through to the default tiers below.
/// 4. `tiledata.mul` name via `StaticDataProvider` (only when `--data`).
/// 5. The demo-server's hardcoded [`crate::constants::item_names`] table
///    (keyed by graphic).
/// 6. `[item 0x{graphic:04X}]` hex fallback.
///
/// After the base name is resolved, a weapon with remaining poison charges
/// (`poison_charges` meta > 0) gets a trailing ` (poisoned)`.
///
/// Returns `None` only when `serial` does not refer to any item.
fn resolve_item_name(zone: &DemoZone, serial: u32) -> Option<ResolvedItemName> {
    let (graphic, color, amount) = locate_item_graphic(zone, serial)?;

    let stackable = is_stackable(graphic, zone.static_data().map(|a| a.as_ref()));

    // Resolve the base name + whether it is an explicit (proper-noun) name.
    let (mut base_name, explicit_name) =
        if let Some(name) = zone.item_props.get(serial).and_then(|p| p.name_owned()) {
            // 1. Explicit per-instance name.
            (name, true)
        } else if crate::potions::is_poison_graphic(graphic) {
            // 2. Poison bottle — named by its per-instance level (from meta),
            //    since all levels share one graphic/hue.
            let level = zone
                .item_props
                .get(serial)
                .and_then(|p| p.get_meta_int(crate::game_session::poison::META_POISON_LEVEL))
                .map(|v| v.clamp(1, 4) as u8)
                .unwrap_or(1);
            (crate::potions::poison_name(level).to_string(), true)
        } else if let Some(def) = crate::potions::lookup_potion(graphic, color) {
            // 3. Known potion identified by its (graphic, color) pair.
            (def.name.to_string(), true)
        } else {
            // 4. tiledata name (trim NUL / whitespace padding from the
            //    FixedString), then expand any UO plural markers (`%s%`,
            //    `%singular/plural%`, …) against the stack amount.
            let tiledata_name = zone
                .static_data()
                .and_then(|sd| sd.static_tile_def(graphic))
                .map(|def| def.name.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string())
                .filter(|s| !s.is_empty())
                .map(|name| expand_plural_markers(&name, amount));

            // 5. Hardcoded table.
            let base_name = tiledata_name
                .or_else(|| crate::constants::item_names::name_for_graphic(graphic).map(str::to_string))
                // 6. Hex fallback.
                .unwrap_or_else(|| format!("[item 0x{:04X}]", graphic));
            (base_name, false)
        };

    // Poisoned weapons (those with remaining poison charges) get a
    // " (poisoned)" suffix.  Poison *bottles* carry `poison_level` but no
    // `poison_charges`, so they are unaffected.
    let poisoned = zone
        .item_props
        .get(serial)
        .and_then(|p| p.get_meta_int(crate::game_session::poison::META_POISON_CHARGES))
        .is_some_and(|charges| charges > 0);
    if poisoned {
        base_name.push_str(" (poisoned)");
    }

    Some(ResolvedItemName {
        base_name,
        graphic,
        amount,
        stackable,
        explicit_name,
    })
}

// ── CommandHandler impl ──────────────────────────────────────────────────
impl CommandHandler<DemoEntity, HashContainerStore, HashItemProps<ItemProps>> for DemoHandler {
    type Command = DemoCommand;

    fn handle(
        &mut self,
        zone: &mut DemoZone,
        cmd: DemoCommand,
        event_tx: &tokio::sync::mpsc::UnboundedSender<framework::continuum::WorldEvent>,
    ) {
        match cmd {
            DemoCommand::Base(base_cmd) => {
                // Split borrows: the intercept closure needs this zone's
                // tick state while `handle_base_command` borrows `self.base`.
                let zst = self.zone_state.entry(zone.map_id).or_default();
                let sets_need_rescan = &mut self.sets_need_rescan;
                self.base.handle_base_command(
                    zone,
                    base_cmd,
                    event_tx,
                    &mut |engine_cmd, zone| {
                        intercept_engine_command(
                            engine_cmd, zone, zst, sets_need_rescan,
                        )
                    },
                );
            }
            DemoCommand::BroadcastSound { sound_id, x, y, z } => {
                let _ = event_tx.send(WorldEvent::SoundPlayed {
                    map_id: zone.map_id,
                    sound_id,
                    x,
                    y,
                    z,
                });
            }
            DemoCommand::BroadcastEffect {
                direction_type, source_serial, target_serial, graphic,
                x, y, z, target_x, target_y, target_z,
                speed, duration, fixed_direction, explode,
            } => {
                let _ = event_tx.send(WorldEvent::EffectPlayed {
                    map_id: zone.map_id,
                    direction_type, source_serial, target_serial, graphic,
                    x, y, z, target_x, target_y, target_z,
                    speed, duration, fixed_direction, explode,
                });
            }
            DemoCommand::BroadcastAnimation {
                serial, action, frame_count, repeat_count,
                reverse, repeat, frame_delay, x, y,
            } => {
                let _ = event_tx.send(WorldEvent::AnimationPlayed {
                    map_id: zone.map_id,
                    serial, action, frame_count, repeat_count,
                    reverse, repeat, frame_delay, x, y,
                });
            }
            DemoCommand::BroadcastSpeech {
                serial, graphic, speech_type, color, font,
                name, message, x, y,
            } => {
                let _ = event_tx.send(WorldEvent::Speech {
                    map_id: zone.map_id,
                    serial, graphic, speech_type, color, font,
                    name, message, x, y,
                });
            }
            DemoCommand::TryHarvestResource { x, y, z, graphic, kind, source, want, reply } => {
                let result = handle_harvest_request(
                    zone, &mut self.resource_nodes, x, y, z, graphic, kind, source, want,
                );
                let _ = reply.send(result);
            }
            DemoCommand::AttachControllerPersist { serial, controller, controller_id } => {
                // 1. Attach the controller (same as BaseCommand::AttachController).
                let zst = self.zone_state.entry(zone.map_id).or_default();
                let sets_need_rescan = &mut self.sets_need_rescan;
                self.base.handle_base_command(
                    zone,
                    common::uo_engine::base_handler::BaseCommand::AttachController { serial, controller },
                    event_tx,
                    &mut |engine_cmd, zone| {
                        intercept_engine_command(
                            engine_cmd, zone, zst, sets_need_rescan,
                        )
                    },
                );
                // 2. Persist the controller ID in item_props.meta.
                use common::uo_engine::item_props::MetaValue;
                let mut props = zone.item_props.get(serial).cloned().unwrap_or_default();
                props.meta.insert(
                    "controller".to_string(),
                    MetaValue::Str(controller_id),
                );
                zone.item_props.insert(serial, props);
            }
        }
    }

    fn tick(
        &mut self,
        zone: &mut DemoZone,
        event_tx: &tokio::sync::mpsc::UnboundedSender<framework::continuum::WorldEvent>,
    ) {
        self.base.tick(zone, event_tx);

        // Whether this zone is waiting for a `RestoreSnapshot` to land before
        // spawn-point bootstrap should run.  Passed to the spawn manager so
        // it can delay bootstrap until the zone is fully populated.
        let restore_pending = self.sets_need_rescan > 0;

        // Drive monster spawn points for this zone.  Split borrows of the
        // base handler: the spawn manager needs the serial allocator (read)
        // and, via the attach callback, the controller host (write).
        let serial_alloc = self.base.engine.serial_alloc.clone();
        let scripts_dir = self.scripts_dir.clone();
        let zone_map_id = zone.map_id;
        let host = &mut self.base.host;
        self.spawn_mgr.tick(
            zone,
            event_tx,
            &serial_alloc,
            restore_pending,
            |serial, controller_id| {
                // Resolve the persistent controller id through the registry,
                // so spawners can transparently use Rust or Lua AI.
                match controller_registry::create_controller(controller_id, &scripts_dir) {
                    Ok(controller) => host.attach(serial, controller, zone_map_id),
                    Err(e) => log::error!(
                        "[spawn] failed to create controller {:?} for 0x{:08X}: {}",
                        controller_id, serial, e,
                    ),
                }
            },
        );

        // Rebuild equipment index for any newly spawned monsters (cheap; only
        // adds missing entries).
        self.base.index_zone_equipment(zone);

        // Per-zone door/ship tick state.  Created lazily; pruned at the end of
        // this tick if it ends up idle, so zones without doors/ships don't
        // accumulate.  Borrowing it here (after all `self.base` / `self.base`-
        // index borrows above are done) keeps the borrow checker happy.
        let zst = self.zone_state.entry(zone.map_id).or_default();

        // Rebuild the active-ship / open-door sets after a snapshot restore
        // (a restore repopulates `zone.item_props` directly, bypassing the
        // `SetItemProps` intercept that normally maintains the sets).
        // Also re-adopt any monsters that were restored from the snapshot so
        // the spawn manager knows about them and does not double-spawn.
        // The counter tracks how many zones still need a rescan; each tick
        // handles one zone so that multi-zone `--load` restores work correctly.
        // `rebuild_active_sets` now only clears/repopulates *this* zone's sets,
        // so it can no longer wipe another zone's open doors / sailing ships.
        if self.sets_need_rescan > 0 {
            rebuild_active_sets(zone, &mut zst.active_ships, &mut zst.open_doors);
            self.spawn_mgr.adopt_zone(zone);
            self.sets_need_rescan -= 1;
        }

        // Drive ship sailing.
        zst.next_sail_tick = tick_sailing_ships(zone, event_tx, &mut zst.active_ships);

        // Drive door auto-close.
        zst.next_door_tick = tick_auto_close_doors(zone, event_tx, &mut zst.open_doors);

        // Drop this zone's tick state if it carries no pending work, so idle
        // zones (e.g. spawn-only worlds with no doors/ships) don't accumulate.
        if zst.is_idle() {
            self.zone_state.remove(&zone.map_id);
        }

        // Prune fully-recovered resource nodes and schedule the next sweep.
        // (Node state is computed lazily on harvest; this sweep only reclaims
        // memory for nodes nobody is actively harvesting, mirroring the
        // door/ship ticks.  An empty map costs nothing.)
        self.next_resource_tick = self
            .resource_nodes
            .sweep()
            .map(|ms| resource_instant_from_clock_ms(ms));
    }

    fn next_tick_at(&mut self) -> Option<tokio::time::Instant> {
        // A pending snapshot rescan must run on the very next tick so that
        // doors / ships are rebuilt even in an otherwise idle world (e.g.
        // immediately after a CLI `--load` with no connected players).
        if self.sets_need_rescan > 0 || self.spawn_mgr.has_pending_adopt() {
            return Some(tokio::time::Instant::now());
        }
        let base = self.base.next_tick_at();
        let spawn = self.spawn_mgr.next_tick_at();
        let res = self.next_resource_tick;
        // Per-zone sail / door wakes across every tracked zone.
        let zone_ticks = self
            .zone_state
            .values()
            .flat_map(|z| [z.next_sail_tick, z.next_door_tick]);
        [base, spawn, res]
            .into_iter()
            .chain(zone_ticks)
            .flatten()
            .min()
    }

    fn post_command(&mut self) {
        self.base.flush_events();
    }

    fn on_entity_leaving_zone(
        &mut self,
        serial: u32,
    ) -> Option<Box<dyn std::any::Any + Send>> {
        self.base.detach_controller_for_transfer(serial)
    }

    fn on_entity_entering_zone(
        &mut self,
        serial: u32,
        controller_state: Option<Box<dyn std::any::Any + Send>>,
        to_map: u8,
    ) {
        self.base.attach_controller_from_transfer(serial, controller_state, to_map);
    }
}

// ── Ship sailing tick ──────────────────────────────────────────────────────

use crate::game_session::shipping::{META_SAIL_HEADING, META_SAIL_LAST_MOVE, SAIL_TICK_MS};

/// Advance all currently-sailing ships by one tile (if enough time has
/// passed since their last move).
///
/// Called from `DemoHandler::tick()`.  Returns the next instant at which
/// a sailing ship needs another movement tick, or `None` if no ships are
/// sailing.
fn tick_sailing_ships(
    zone: &mut DemoZone,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    active_ships: &mut std::collections::HashSet<u32>,
) -> Option<tokio::time::Instant> {
    use common::uo_engine::item_props::MetaValue;

    if active_ships.is_empty() {
        return None;
    }

    // Use a single monotonic clock for both the per-ship movement gate and
    // the next-tick scheduling, so the two can't drift apart.  We measure
    // elapsed milliseconds from a process-wide start instant (stored in a
    // `OnceLock`) — this is immune to wall-clock jumps (NTP, DST) that made
    // the old `SystemTime::now()` gate jitter against the tokio timer.
    let now_ms = sail_clock_now_ms();

    // Collect sailing ships that are due: serials + their desired heading.
    // We iterate only the tracked active-ship set (not every item in the
    // zone) and collect first because we'll mutate the zone during movement.
    // We also track the earliest "next due" time across *all* sailing ships
    // (whether due this tick or not) so the worker is woken at the right
    // instant for the next move.  Serials whose ship has vanished or lost its
    // heading meta are pruned from the set.
    let mut sailing: Vec<(u32, String)> = Vec::new();
    let mut stale: Vec<u32> = Vec::new();
    let mut earliest_next_due_ms: Option<i64> = None;
    for &serial in active_ships.iter() {
        let heading_str = zone
            .item_props
            .get(serial)
            .and_then(|p| p.get_meta_str(META_SAIL_HEADING).map(str::to_string));
        let is_multi = zone.store.get(serial).map(|e| e.is_multi()).unwrap_or(false);
        match (heading_str, is_multi) {
            (Some(heading_str), true) => {
                // `last_move` is the *scheduled* time of the previous move
                // (not the wall-clock instant it actually ran), so the cadence
                // stays exactly `SAIL_TICK_MS` regardless of how late the
                // worker woke up.
                let last_move = zone
                    .item_props
                    .get(serial)
                    .and_then(|p| p.get_meta_int(META_SAIL_LAST_MOVE))
                    .unwrap_or(0);
                let next_due = last_move + SAIL_TICK_MS;
                if now_ms >= next_due {
                    sailing.push((serial, heading_str));
                } else {
                    earliest_next_due_ms = Some(
                        earliest_next_due_ms.map_or(next_due, |e| e.min(next_due)),
                    );
                }
            }
            // No heading meta or no longer a multi — drop from the set.
            _ => stale.push(serial),
        }
    }
    for serial in stale {
        active_ships.remove(&serial);
    }

    for (serial, heading_str) in &sailing {
        let heading = match crate::ships::ShipHeading::from_keyword(heading_str) {
            Some(h) => h,
            None => continue,
        };
        let (dx, dy) = heading.delta();

        match common::uo_engine::handler::ship_ops::handle_move_ship(
            zone, event_tx, *serial, dx, dy,
        ) {
            Ok(()) => {
                // Advance the scheduled time by one tick.  Anchor to the
                // previous scheduled time (`last_move + SAIL_TICK_MS`) rather
                // than `now_ms`, so accumulated lateness does not stretch the
                // interval.  If the ship has fallen more than one tick behind
                // (e.g. after a long stall), snap forward to `now_ms` to avoid
                // a burst of catch-up moves.
                if let Some(props) = zone.item_props.get_mut(*serial) {
                    let last_move = props.get_meta_int(META_SAIL_LAST_MOVE).unwrap_or(0);
                    let scheduled = last_move + SAIL_TICK_MS;
                    let new_last = if now_ms - scheduled >= SAIL_TICK_MS {
                        now_ms
                    } else {
                        scheduled
                    };
                    props.meta.insert(
                        META_SAIL_LAST_MOVE.to_string(),
                        MetaValue::Int(new_last),
                    );
                    let next_due = new_last + SAIL_TICK_MS;
                    earliest_next_due_ms = Some(
                        earliest_next_due_ms.map_or(next_due, |e| e.min(next_due)),
                    );
                }
            }
            Err(_reason) => {
                // Ship is blocked — stop sailing and drop it from the set.
                if let Some(props) = zone.item_props.get_mut(*serial) {
                    props.meta.remove(META_SAIL_HEADING);
                    props.meta.remove(META_SAIL_LAST_MOVE);
                }
                active_ships.remove(serial);
            }
        }
    }

    // Schedule the next wake at the earliest ship's due time, converting the
    // monotonic-ms target back into a `tokio::time::Instant`.
    let next_due_ms = earliest_next_due_ms?;
    let delay_ms = (next_due_ms - now_ms).max(0) as u64;
    Some(tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms))
}

/// Monotonic milliseconds since the first call (process start).
///
/// Used by the sailing scheduler so the movement gate and the tokio wake
/// timer share one clock that can never jump backwards.
fn sail_clock_now_ms() -> i64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_millis() as i64
}

/// Convert a [`sail_clock_now_ms`] timestamp into a tokio
/// [`Instant`](tokio::time::Instant) for scheduling the next sail tick.
fn sail_instant_from_clock_ms(target_ms: i64) -> tokio::time::Instant {
    let now_ms = sail_clock_now_ms();
    let delay_ms = (target_ms - now_ms).max(0) as u64;
    tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms)
}

// ── Door auto-close tick ─────────────────────────────────────────────────────

/// Monotonic milliseconds since process start, shared by the door open
/// (session side) and the auto-close scheduler (this handler).
///
/// A single monotonic clock guarantees the close timestamp written when a
/// door opens and the gate checked here can never drift against each other.
pub(crate) fn door_clock_now_ms() -> i64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_millis() as i64
}

/// Convert a [`door_clock_now_ms`] timestamp into a tokio
/// [`Instant`](tokio::time::Instant) for scheduling the next auto-close tick.
fn door_instant_from_clock_ms(target_ms: i64) -> tokio::time::Instant {
    let now_ms = door_clock_now_ms();
    let delay_ms = (target_ms - now_ms).max(0) as u64;
    tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms)
}

/// Automatically close doors whose auto-close time has arrived.
///
/// Mirrors [`tick_sailing_ships`]: per-door state lives in
/// `zone.item_props.meta` under [`crate::doors::META_DOOR_CLOSE_AT`].  A door
/// is opened by the session (which sets the timestamp); this scan closes it
/// once the delay has elapsed.
///
/// If a mobile is standing on the door's *closed* tile when the timer fires,
/// closing is deferred and re-checked every [`crate::doors::DOOR_RETRY_CLOSE_MS`]
/// until the doorway is clear (the door cannot shut on someone blocking it).
///
/// Returns the earliest instant at which another door needs servicing, or
/// `None` when no doors are open.
fn tick_auto_close_doors(
    zone: &mut DemoZone,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    open_doors: &mut std::collections::HashSet<u32>,
) -> Option<tokio::time::Instant> {
    use common::uo_engine::item_props::MetaValue;
    use framework::ecumene::TileRect;
    use u_core::Pos3D;

    if open_doors.is_empty() {
        return None;
    }

    let now_ms = door_clock_now_ms();

    // Collect doors whose close time has arrived (we mutate the zone below, so
    // gather serials first).  We iterate only the tracked open-door set, not
    // every item in the zone.  Track the earliest pending close for scheduling
    // and prune entries that have lost their close-at meta.
    let mut due: Vec<u32> = Vec::new();
    let mut stale: Vec<u32> = Vec::new();
    let mut earliest_next_ms: Option<i64> = None;
    for &serial in open_doors.iter() {
        match zone
            .item_props
            .get(serial)
            .and_then(|p| p.get_meta_int(crate::doors::META_DOOR_CLOSE_AT))
        {
            Some(close_at) => {
                if now_ms >= close_at {
                    due.push(serial);
                } else {
                    earliest_next_ms =
                        Some(earliest_next_ms.map_or(close_at, |e| e.min(close_at)));
                }
            }
            // No close-at meta any more — drop from the set.
            None => stale.push(serial),
        }
    }
    for serial in stale {
        open_doors.remove(&serial);
    }

    for serial in due {
        // The door must still exist and currently be an open door item.
        let (graphic, x, y, z, color, amount, is_container, hidden, facing) =
            match zone.store.get(serial).and_then(|e| e.item()) {
                Some(i) => (
                    i.graphic, i.x, i.y, i.z, i.color, i.amount,
                    i.is_container, i.hidden, i.facing,
                ),
                None => {
                    // Door vanished — drop the stale schedule entry.
                    if let Some(p) = zone.item_props.get_mut(serial) {
                        p.meta.remove(crate::doors::META_DOOR_CLOSE_AT);
                    }
                    open_doors.remove(&serial);
                    continue;
                }
            };

        let (closed_graphic, dx, dy) = crate::doors::close_target(graphic);
        if closed_graphic == graphic {
            // Already closed (e.g. the player closed it manually); clear.
            if let Some(p) = zone.item_props.get_mut(serial) {
                p.meta.remove(crate::doors::META_DOOR_CLOSE_AT);
            }
            open_doors.remove(&serial);
            continue;
        }

        let new_x = (x as i32 + dx as i32) as u16;
        let new_y = (y as i32 + dy as i32) as u16;

        // Defer if a mobile is standing on the closed tile.  Re-check on a
        // short cadence so the door shuts promptly once the blocker moves off,
        // instead of waiting a full auto-close interval.  The door stays in
        // `open_doors` (its close-at meta is just pushed forward).
        let rect = TileRect { x_min: new_x, y_min: new_y, x_max: new_x, y_max: new_y };
        let blocked = zone
            .query_area(&rect)
            .iter()
            .any(|e| e.is_mobile());
        if blocked {
            let next = now_ms + crate::doors::DOOR_RETRY_CLOSE_MS;
            if let Some(p) = zone.item_props.get_mut(serial) {
                p.meta.insert(
                    crate::doors::META_DOOR_CLOSE_AT.to_string(),
                    MetaValue::Int(next),
                );
            }
            earliest_next_ms = Some(earliest_next_ms.map_or(next, |e| e.min(next)));
            continue;
        }

        // Apply the close: update the entity and notify observers.
        let updated = DemoEntity::Item {
            serial,
            graphic: closed_graphic,
            color,
            amount,
            x: new_x,
            y: new_y,
            z,
            is_container,
            hidden,
            facing,
        };
        let snap = Entity::snapshot(&updated);
        zone.update(serial, updated);
        let _ = event_tx.send(WorldEvent::EntityUpdated {
            map_id: zone.map_id,
            serial,
            pos: Pos3D { x: new_x, y: new_y, z },
            entity: snap,
        });
        let _ = event_tx.send(WorldEvent::SoundPlayed {
            map_id: zone.map_id,
            sound_id: crate::constants::sound::DOOR_CLOSE,
            x: new_x,
            y: new_y,
            z: z as i16,
        });

        // Done — remove the schedule entry and drop from the set.
        if let Some(p) = zone.item_props.get_mut(serial) {
            p.meta.remove(crate::doors::META_DOOR_CLOSE_AT);
        }
        open_doors.remove(&serial);
    }

    let next_ms = earliest_next_ms?;
    let delay_ms = (next_ms - now_ms).max(0) as u64;
    Some(tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms))
}

// ── Resource-node harvesting ─────────────────────────────────────────────────

/// Validate a harvest source against authoritative world state and, if valid,
/// run the resource-node policy (depletion + regeneration / maturation).
///
/// Runs in the worker because only the zone has the static map data and the
/// entity store needed to validate the source the client targeted.
fn handle_harvest_request(
    zone: &mut DemoZone,
    nodes: &mut crate::resource_nodes::NodeMap,
    x: u16,
    y: u16,
    z: i8,
    graphic: u16,
    kind: crate::gathering::GatherKind,
    source: crate::commands::GatherSource,
    want: u16,
) -> crate::commands::HarvestReply {
    use crate::commands::{GatherSource, HarvestReply};
    use crate::resource_nodes::HarvestOutcome;

    // 1. Server-side validation of the targeted source, plus the canonical
    //    `(x, y, z, graphic)` to key the node by (never trust the client's
    //    reported values blindly).
    let key = match source {
        GatherSource::StaticTile => match validate_static_source(zone, x, y, z, graphic) {
            Some(k) => k,
            None => return HarvestReply::Invalid,
        },
        GatherSource::ItemNode { serial } => match validate_item_node(zone, serial, kind) {
            Some(k) => k,
            None => return HarvestReply::Invalid,
        },
    };

    // 2. Apply the node policy (consumption + lazy regeneration).
    match nodes.harvest(key, want) {
        HarvestOutcome::Yield(d) => HarvestReply::Yield {
            graphic: d.graphic,
            color: d.color,
            amount: d.amount,
            name: d.name,
        },
        HarvestOutcome::Depleted => HarvestReply::Depleted,
        HarvestOutcome::NotReady => HarvestReply::Nothing,
    }
}

/// Validate that a static tile with `graphic` really exists at/near
/// `(x, y, z)` in the loaded map data, returning the node key keyed on the
/// *actual* static's z when matched.
///
/// When no static data is loaded (server started without `--data`), there is
/// nothing to validate against, so the client's report is trusted (keeps the
/// demo usable on minimal data sets).
fn validate_static_source(
    zone: &DemoZone,
    x: u16,
    y: u16,
    z: i8,
    graphic: u16,
) -> Option<crate::resource_nodes::NodeKey> {
    use crate::resource_nodes::NodeKey;

    let world = zone.map_id;

    let Some(sd) = zone.static_data() else {
        // No map data — trust the client's report.
        return Some(NodeKey { world, x, y, z, graphic });
    };

    // A static with the reported graphic must exist at (x, y).  Match the z
    // with a small tolerance, since clients commonly report a surface z rather
    // than the exact static z.
    const Z_TOLERANCE: i32 = 2;
    let statics = sd.statics_at(world, x, y)?;
    let matched = statics
        .iter()
        .find(|s| s.tile_id == graphic && (s.z as i32 - z as i32).abs() <= Z_TOLERANCE)?;

    Some(NodeKey { world, x, y, z: matched.z, graphic })
}

/// Validate that `serial` is a live resource-node item entity for `kind`,
/// returning a node key built from its *actual* coordinates and graphic.
fn validate_item_node(
    zone: &DemoZone,
    serial: u32,
    kind: crate::gathering::GatherKind,
) -> Option<crate::resource_nodes::NodeKey> {
    use crate::resource_nodes::NodeKey;

    // Must carry the matching gather-resource meta.
    let ok = zone
        .item_props
        .get(serial)
        .and_then(|p| p.get_meta_str(crate::gathering::META_GATHER_RESOURCE))
        .is_some_and(|v| v == kind.as_str());
    if !ok {
        return None;
    }

    // Must still exist as an item entity; key by its own coordinates/graphic.
    let item = zone.store.get(serial).and_then(|e| e.item())?;
    Some(NodeKey {
        world: zone.map_id,
        x: item.x,
        y: item.y,
        z: item.z,
        graphic: item.graphic,
    })
}

/// Convert a [`crate::resource_nodes::node_clock_now_ms`] timestamp into a
/// tokio [`Instant`](tokio::time::Instant) for scheduling.
fn resource_instant_from_clock_ms(target_ms: i64) -> tokio::time::Instant {
    let now_ms = crate::resource_nodes::node_clock_now_ms();
    let delay_ms = (target_ms - now_ms).max(0) as u64;
    tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms)
}

/// Rebuild the active-ship / open-door sets from the zone's item props.
///
/// Used after a snapshot restore, which repopulates `zone.item_props`
/// directly (bypassing the `SetItemProps` intercept that normally keeps the
/// sets in sync).  This is the only full scan of `item_props` for these two
/// subsystems and runs once per restore, not per tick.
fn rebuild_active_sets(
    zone: &DemoZone,
    active_ships: &mut std::collections::HashSet<u32>,
    open_doors: &mut std::collections::HashSet<u32>,
) {
    active_ships.clear();
    open_doors.clear();
    for (&serial, props) in zone.item_props.iter() {
        if props.meta.contains_key(META_SAIL_HEADING) {
            active_ships.insert(serial);
        }
        if props.meta.contains_key(crate::doors::META_DOOR_CLOSE_AT) {
            open_doors.insert(serial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::expand_plural_markers;

    #[test]
    fn plain_name_unchanged() {
        assert_eq!(expand_plural_markers("iron ingot", 5), "iron ingot");
        assert_eq!(expand_plural_markers("iron ingot", 1), "iron ingot");
    }

    #[test]
    fn s_marker_appends_for_plural() {
        assert_eq!(expand_plural_markers("bandage%s%", 49), "bandages");
        assert_eq!(expand_plural_markers("bandage%s%", 1), "bandage");
        assert_eq!(expand_plural_markers("bloody bandage%s%", 3), "bloody bandages");
        assert_eq!(expand_plural_markers("bloody bandage%s%", 1), "bloody bandage");
    }

    #[test]
    fn suffix_marker_appends_arbitrary_suffix() {
        assert_eq!(expand_plural_markers("torch%es%", 2), "torches");
        assert_eq!(expand_plural_markers("torch%es%", 1), "torch");
    }

    #[test]
    fn singular_slash_plural_marker() {
        assert_eq!(expand_plural_markers("%loaf/loaves% of bread", 1), "loaf of bread");
        assert_eq!(expand_plural_markers("%loaf/loaves% of bread", 4), "loaves of bread");
    }

    #[test]
    fn multiple_markers() {
        assert_eq!(expand_plural_markers("rock%s% and stone%s%", 2), "rocks and stones");
        assert_eq!(expand_plural_markers("rock%s% and stone%s%", 1), "rock and stone");
    }

    #[test]
    fn unterminated_marker_dropped() {
        assert_eq!(expand_plural_markers("broken%", 3), "broken");
    }
}
