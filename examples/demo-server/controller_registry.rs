//! Controller registry: creation, persistence, and built-in controller types.
//!
//! Each controller type has a **persistent ID** string in the format
//! `"type"` or `"type:params"`.  This module provides:
//!
//! - [`create_controller`] — instantiate a controller from its persistent ID
//! - [`controller_id`] — build a persistent ID string
//! - [`WanderController`] — simple NPC AI that wanders randomly

use std::path::Path;
use std::time::Duration;

use rand::Rng;

use framework::anima::{ControlContext, EntityController};
use framework::ecumene::TileRect;
use u_core::{Facing, Heading};

use common::uo_engine::controller::{EntityEvent, DemoControllerDef};

// ── Controller creation from persistent ID ────────────────────────────────

/// Create a controller from its persistent ID string.
///
/// Format: `"type"` or `"type:params"`
///
/// Known types:
/// - `"wander"` / `"wander:5"` — WanderController with interval in seconds
///   (default 3s when params is empty)
/// - `"pet"` — PetController for a tamed animal (follow/stay via item_props meta)
/// - `"spawner"` — SpawnerController: UI for an admin spawner object (see
///   [`crate::spawner_object`])
/// - `"teleporter"` — TeleporterController: moves mobiles that step onto the
///   item to a destination (intra- or cross-world); see [`TeleporterController`]
/// - `"monster:aggro=10,leash=20,dmg=8-18,swing=2500"` — MonsterController
///   (aggressive melee AI).  All params optional; defaults applied when omitted.
/// - `"lua:script.lua"` — LuaController loaded from `{scripts_dir}/script.lua`
///
/// `scripts_dir` is the base directory for script files (e.g. `"scripts"`).
///
/// Returns an error string describing what went wrong.
pub fn create_controller(
    id: &str,
    scripts_dir: &Path,
) -> Result<Box<dyn EntityController<DemoControllerDef>>, String> {
    let (kind, params) = id.split_once(':').unwrap_or((id, ""));
    match kind {
        "wander" => {
            // Parse params: "3" (interval only) or "3,r=10" (interval + radius).
            let (secs, radius) = parse_wander_params(params)?;
            if let Some(r) = radius {
                Ok(Box::new(WanderController::with_radius(Duration::from_secs(secs), r)))
            } else {
                Ok(Box::new(WanderController::new(Duration::from_secs(secs))))
            }
        }
        "pet" => Ok(Box::new(PetController::new())),
        "spawner" => Ok(Box::new(crate::spawner_object::SpawnerController::new())),
        "teleporter" => Ok(Box::new(TeleporterController::new())),
        "monster" => {
            let cfg = MonsterCfg::from_params(params)
                .map_err(|e| format!("invalid monster params {:?}: {}", params, e))?;
            Ok(Box::new(MonsterController::new(cfg)))
        }
        #[cfg(feature = "lua")]
        "lua" => {
            if params.is_empty() {
                return Err("lua controller requires a script path".into());
            }
            let path = scripts_dir.join(params);
            crate::lua_script::LuaController::from_file(&path, Some(scripts_dir))
                .map(|c| Box::new(c) as _)
                .map_err(|e| format!("failed to load lua controller {:?}: {}", params, e))
        }
        _ => Err(format!("unknown controller type: {:?}", kind)),
    }
}

/// Build the persistent ID string for a controller.
///
/// Used when attaching controllers to store in `item_props.meta["controller"]`.
///
/// ```text
/// controller_id("wander", "3")  -> "wander:3"
/// controller_id("wander", "")   -> "wander"
/// controller_id("lua", "ai.lua") -> "lua:ai.lua"
/// ```
pub fn controller_id(kind: &str, params: &str) -> String {
    if params.is_empty() {
        kind.to_string()
    } else {
        format!("{}:{}", kind, params)
    }
}

/// Parse wander controller params string.
///
/// Accepted formats:
/// - `""` → interval=3, radius=None
/// - `"5"` → interval=5, radius=None
/// - `"5,r=10"` → interval=5, radius=Some(10)
/// - `"3,r=4"` → interval=3, radius=Some(4)
fn parse_wander_params(params: &str) -> Result<(u64, Option<u16>), String> {
    if params.is_empty() {
        return Ok((3, None));
    }

    let mut secs: u64 = 3;
    let mut radius: Option<u16> = None;

    for part in params.split(',') {
        let part = part.trim();
        if let Some(r_val) = part.strip_prefix("r=") {
            radius = Some(
                r_val.parse().map_err(|e| format!("invalid wander radius {:?}: {}", r_val, e))?
            );
        } else if !part.is_empty() {
            secs = part.parse().map_err(|e| format!("invalid wander interval {:?}: {}", part, e))?;
        }
    }

    Ok((secs, radius))
}

// ── WanderController ──────────────────────────────────────────────────────

/// Simple NPC AI: wander in a random direction every few seconds.
///
/// If `max_radius` is set, the NPC will not wander further than that many
/// tiles (Chebyshev distance) from its home position.  The home position
/// is captured automatically on the first tick.
pub struct WanderController {
    wander_interval: Duration,
    timer_scheduled: bool,
    /// Home position — captured on the first tick.  The NPC tries to
    /// stay within `max_radius` Chebyshev tiles of this point.
    home: Option<(u16, u16)>,
    /// Maximum Chebyshev distance from home.  `None` = unlimited.
    max_radius: Option<u16>,
}

impl WanderController {
    pub fn new(interval: Duration) -> Self {
        Self {
            wander_interval: interval,
            timer_scheduled: false,
            home: None,
            max_radius: None,
        }
    }

    /// Create a wander controller that stays within `radius` tiles of its
    /// spawn point.
    pub fn with_radius(interval: Duration, radius: u16) -> Self {
        Self {
            wander_interval: interval,
            timer_scheduled: false,
            home: None,
            max_radius: Some(radius),
        }
    }
}

impl WanderController {
    /// Max random jitter added/subtracted from the wander interval (±20 %).
    const JITTER_FRAC: u64 = 5; // denominator: interval / 5 = 20 %

    /// Compute the next one-shot delay: base interval ± random jitter.
    fn next_delay(&self) -> Duration {
        let base_ms = self.wander_interval.as_millis() as u64;
        let jitter_range = base_ms / Self::JITTER_FRAC; // 20 % of interval
        // Offset in [-jitter_range, +jitter_range]
        let offset = rand::rng().random_range(0..=jitter_range * 2) as i64
            - jitter_range as i64;
        let ms = (base_ms as i64 + offset).max(100) as u64;
        Duration::from_millis(ms)
    }

    /// Schedule the next one-shot wander timer.
    fn schedule_next(&self, ctx: &mut ControlContext) {
        use framework::anima::TaskAction;
        let map_id = ctx.map_id();
        ctx.scheduler.schedule(
            self.next_delay(),
            TaskAction::FireTimer {
                entity_serial: ctx.entity_serial,
                timer_id: 1, // "wander"
            },
            Some(map_id),
        );
    }
}

impl EntityController<DemoControllerDef> for WanderController {
    fn tick(&mut self, ctx: &mut ControlContext, _dt: Duration) {
        // Capture the home position on the first tick.
        if self.home.is_none() {
            if let Some(me) = ctx.me() {
                self.home = Some((me.pos.x, me.pos.y));
            }
        }

        if !self.timer_scheduled {
            // First fire gets a fully random delay in [0, interval) so that
            // NPCs spawned at the same time don't walk in lockstep.
            use framework::anima::TaskAction;
            let initial_delay = Duration::from_millis(
                rand::rng().random_range(0..self.wander_interval.as_millis() as u64),
            );
            let map_id = ctx.map_id();
            ctx.scheduler.schedule(
                initial_delay,
                TaskAction::FireTimer {
                    entity_serial: ctx.entity_serial,
                    timer_id: 1,
                },
                Some(map_id),
            );
            self.timer_scheduled = true;
        }
    }

    fn on_event(&mut self, ctx: &mut ControlContext, event: EntityEvent) {
        if let EntityEvent::TimerFired { timer_id: 1 } = event {
            // Schedule the *next* step first (one-shot with jitter).
            self.schedule_next(ctx);

            let dir_idx = rand::rng().random_range(0u8..8);
            if let Some(heading) = Heading::from_raw(dir_idx) {
                // If a radius is configured, check whether the step would
                // take the NPC beyond its home leash.
                if let (Some(radius), Some((hx, hy))) = (self.max_radius, self.home) {
                    if let Some(me) = ctx.me() {
                        let (dx, dy) = heading.delta();
                        let nx = me.pos.x as i32 + dx;
                        let ny = me.pos.y as i32 + dy;
                        let dist_x = (nx - hx as i32).unsigned_abs() as u16;
                        let dist_y = (ny - hy as i32).unsigned_abs() as u16;
                        if dist_x.max(dist_y) > radius {
                            // Would leave the leash — skip this step.
                            return;
                        }
                    }
                }
                let _ = ctx.step(Facing::from_heading(heading));
            }
        }
    }

    fn name(&self) -> &str {
        "wander"
    }

    fn next_tick_at(&self) -> Option<tokio::time::Instant> {
        if !self.timer_scheduled {
            // Need the first tick to register the repeating scheduler timer.
            Some(tokio::time::Instant::now())
        } else {
            None // scheduler handles wakeups from here on
        }
    }
}

// ── PetController ──────────────────────────────────────────────────────────

/// AI for a tamed pet: follow the owner or stay, driven by the pet's
/// `item_props` meta (`pet_owner` / `pet_command`).
///
/// The session writes the command into meta (e.g. `"follow"` / `"stay"`);
/// this controller polls it on a repeating timer and steps toward the owner
/// when following.  Because all state lives in meta, it survives snapshot
/// save/load and needs no engine type changes.
pub struct PetController {
    timer_scheduled: bool,
}

impl PetController {
    pub fn new() -> Self {
        Self { timer_scheduled: false }
    }
}

impl EntityController<DemoControllerDef> for PetController {
    fn tick(&mut self, ctx: &mut ControlContext, _dt: Duration) {
        if !self.timer_scheduled {
            use framework::anima::TaskAction;
            let interval = Duration::from_millis(crate::taming::FOLLOW_INTERVAL_MS);
            let map_id = ctx.map_id();
            ctx.scheduler.schedule_repeating(
                interval,
                interval,
                TaskAction::FireTimer {
                    entity_serial: ctx.entity_serial,
                    timer_id: 1, // "follow tick"
                },
                Some(map_id),
            );
            self.timer_scheduled = true;
        }
    }

    fn on_event(&mut self, ctx: &mut ControlContext, event: EntityEvent) {
        let EntityEvent::TimerFired { timer_id: 1 } = event else { return };

        // Read ownership + command from this pet's item_props meta.
        let (owner_serial, command) = match read_pet_meta(ctx) {
            Some(v) => v,
            None => return, // not (or no longer) a pet — do nothing
        };

        // Only "follow" causes movement; "stay" (or anything else) stands still.
        if command != crate::taming::CMD_FOLLOW {
            return;
        }

        // Locate self and owner (same map only).
        let Some(me) = ctx.me() else { return };
        let Some(owner) = ctx.get_entity(owner_serial) else { return };

        let dx = owner.pos.x as i32 - me.pos.x as i32;
        let dy = owner.pos.y as i32 - me.pos.y as i32;

        // Close enough — stop following.
        if dx.abs() <= crate::taming::FOLLOW_DISTANCE && dy.abs() <= crate::taming::FOLLOW_DISTANCE {
            return;
        }

        // Greedy one-tile step toward the owner.
        if let Some(heading) = Heading::from_delta(dx, dy) {
            let _ = ctx.step(Facing::from_heading(heading));
        }
    }

    fn name(&self) -> &str {
        "pet"
    }

    fn next_tick_at(&self) -> Option<tokio::time::Instant> {
        if !self.timer_scheduled {
            Some(tokio::time::Instant::now())
        } else {
            None
        }
    }
}

/// Read `(owner_serial, command)` from the entity's `item_props` meta.
///
/// Returns `None` if the entity has no `pet_owner` meta (not a pet).
fn read_pet_meta(ctx: &ControlContext) -> Option<(u32, String)> {
    use common::uo_engine::item_props::ItemProps;

    let boxed = ctx.get_item_props_any(ctx.entity_serial)?;
    let props = boxed.downcast::<ItemProps>().ok()?;

    let owner = props.get_meta_int(crate::taming::META_PET_OWNER)? as u32;
    let command = props
        .get_meta_str(crate::taming::META_PET_COMMAND)
        .unwrap_or(crate::taming::CMD_FOLLOW)
        .to_string();

    Some((owner, command))
}

// ── MonsterController ──────────────────────────────────────────────────────

use crate::constants::{anim, melee, sound, speech_type};

/// Configuration for [`MonsterController`], read from the spawn template
/// or decoded from a persistent ID (`"monster:aggro=10,leash=20,..."`).
#[derive(Debug, Clone, Copy)]
pub struct MonsterCfg {
    /// Detection radius (Chebyshev tiles) for acquiring a target.
    pub aggro_range: u16,
    /// Maximum distance from the spawn point before the monster disengages
    /// and walks back home.
    pub leash_range: u16,
    /// Minimum melee damage per swing.
    pub damage_min: u16,
    /// Maximum melee damage per swing.
    pub damage_max: u16,
    /// Milliseconds between successful melee swings.
    pub swing_delay_ms: u64,
}

impl Default for MonsterCfg {
    fn default() -> Self {
        Self {
            aggro_range: 10,
            leash_range: 20,
            damage_min: 5,
            damage_max: 15,
            swing_delay_ms: 2500,
        }
    }
}

impl MonsterCfg {
    /// Parse a params string of the form `aggro=10,leash=20,dmg=8-18,swing=2500`.
    ///
    /// All keys are optional; unspecified fields keep their defaults.
    pub fn from_params(params: &str) -> Result<Self, String> {
        let mut cfg = MonsterCfg::default();
        if params.is_empty() {
            return Ok(cfg);
        }
        for kv in params.split(',') {
            let kv = kv.trim();
            if kv.is_empty() {
                continue;
            }
            let (key, value) = kv
                .split_once('=')
                .ok_or_else(|| format!("expected key=value, got {:?}", kv))?;
            match key.trim() {
                "aggro" => {
                    cfg.aggro_range = value.trim().parse()
                        .map_err(|e| format!("bad aggro {:?}: {}", value, e))?;
                }
                "leash" => {
                    cfg.leash_range = value.trim().parse()
                        .map_err(|e| format!("bad leash {:?}: {}", value, e))?;
                }
                "swing" => {
                    cfg.swing_delay_ms = value.trim().parse()
                        .map_err(|e| format!("bad swing {:?}: {}", value, e))?;
                }
                "dmg" => {
                    let (lo, hi) = value.trim()
                        .split_once('-')
                        .ok_or_else(|| format!("dmg expects min-max, got {:?}", value))?;
                    cfg.damage_min = lo.trim().parse()
                        .map_err(|e| format!("bad dmg min {:?}: {}", lo, e))?;
                    cfg.damage_max = hi.trim().parse()
                        .map_err(|e| format!("bad dmg max {:?}: {}", hi, e))?;
                }
                other => return Err(format!("unknown monster param {:?}", other)),
            }
        }
        if cfg.damage_max < cfg.damage_min {
            cfg.damage_max = cfg.damage_min;
        }
        Ok(cfg)
    }

    /// Build the persistent params string (without the `"monster:"` prefix).
    ///
    /// Round-trips with [`from_params`](Self::from_params).
    pub fn to_params(&self) -> String {
        format!(
            "aggro={},leash={},dmg={}-{},swing={}",
            self.aggro_range, self.leash_range,
            self.damage_min, self.damage_max, self.swing_delay_ms,
        )
    }

    /// Build the full persistent controller ID (`"monster:..."`).
    pub fn controller_id(&self) -> String {
        format!("monster:{}", self.to_params())
    }
}

/// AI state machine for [`MonsterController`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiState {
    /// No target: wander near the spawn point and scan for prey.
    Idle,
    /// Have a target: chase and attack it.
    Combat,
    /// Lost / killed the target: walk back to the spawn point.
    Return,
}

/// Aggressive melee monster AI, ported from `scripts/monster_ctrl.lua`.
///
/// Behaviour:
/// - scans for visible non-monster mobiles within `aggro_range` (LOS-checked)
/// - chases the closest target and attacks in melee range
/// - switches aggro to whoever damages it
/// - disengages and returns home when the target leaves `leash_range`
///
/// Driven by one-shot scheduler timers (`STEP_TICK` ± jitter) that re-schedule
/// after each step, plus instant reaction
/// to [`EntityEvent::DamageReceived`].  No async loop — pure event/tick model.
pub struct MonsterController {
    cfg: MonsterCfg,
    state: AiState,
    target: Option<u32>,
    /// Spawn anchor — learned from the entity's position on first tick.
    home: Option<(u16, u16)>,
    /// Wall-clock (epoch ms) of the last successful swing.
    last_swing_ms: u64,
    /// True once the repeating step timer + world-event subscription are set.
    initialised: bool,
}

impl MonsterController {
    /// Tick interval driving chase / scan / attack cadence.
    const STEP_TICK: Duration = Duration::from_millis(500);
    /// Max jitter ±20 % of STEP_TICK (i.e. ±100 ms for 500 ms tick).
    const JITTER_MS: u64 = 100;
    const TIMER_ID: u64 = 1;

    pub fn new(cfg: MonsterCfg) -> Self {
        Self {
            cfg,
            state: AiState::Idle,
            target: None,
            home: None,
            last_swing_ms: 0,
            initialised: false,
        }
    }

    /// Schedule the next AI step as a one-shot timer with small jitter
    /// so that monsters spawned together don't walk in lockstep.
    fn schedule_next_step(&self, ctx: &mut ControlContext) {
        use framework::anima::TaskAction;
        let base_ms = Self::STEP_TICK.as_millis() as u64;
        let offset = rand::rng().random_range(0..=Self::JITTER_MS * 2) as i64
            - Self::JITTER_MS as i64;
        let ms = (base_ms as i64 + offset).max(100) as u64;
        let map_id = ctx.map_id();
        ctx.scheduler.schedule(
            Duration::from_millis(ms),
            TaskAction::FireTimer {
                entity_serial: ctx.entity_serial,
                timer_id: Self::TIMER_ID,
            },
            Some(map_id),
        );
    }

    /// Current wall-clock in epoch milliseconds.
    fn now_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Chebyshev distance between two points.
    fn dist(ax: u16, ay: u16, bx: u16, by: u16) -> u16 {
        let dx = (ax as i32 - bx as i32).unsigned_abs();
        let dy = (ay as i32 - by as i32).unsigned_abs();
        dx.max(dy) as u16
    }

    /// Whether `noto` marks an attackable, non-monster mobile (1–4).
    fn is_prey_noto(noto: Option<u8>) -> bool {
        matches!(noto, Some(n) if (1..=4).contains(&n))
    }

    /// Find the closest visible prey within aggro range.  Returns its serial.
    fn find_target(&self, ctx: &ControlContext) -> Option<u32> {
        let me = ctx.me()?;
        let rect = TileRect::from_view(me.pos.x, me.pos.y, self.cfg.aggro_range);
        let mut best: Option<(u16, u32)> = None;
        for e in ctx.query_area(&rect) {
            if e.serial == ctx.entity_serial || !e.is_mobile {
                continue;
            }
            if !Self::is_prey_noto(e.notoriety) {
                continue;
            }
            if e.hits.unwrap_or(0) == 0 {
                continue;
            }
            let d = Self::dist(me.pos.x, me.pos.y, e.pos.x, e.pos.y);
            if d > self.cfg.aggro_range {
                continue;
            }
            // LOS check (eye height +14, as in monster_ctrl.lua).
            if !ctx.has_los(
                me.pos.x, me.pos.y, me.pos.z as i16 + 14,
                e.pos.x, e.pos.y, e.pos.z as i16 + 14,
            ) {
                continue;
            }
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, e.serial));
            }
        }
        best.map(|(_, s)| s)
    }

    /// Is the current target still alive, present, and within leash range?
    fn target_valid(&self, ctx: &ControlContext) -> bool {
        let Some(ts) = self.target else { return false };
        let Some(t) = ctx.get_entity(ts) else { return false };
        if !t.is_mobile || t.hits.unwrap_or(0) == 0 {
            return false;
        }
        let Some(me) = ctx.me() else { return false };
        let Some((hx, hy)) = self.home else { return true };
        Self::dist(hx, hy, me.pos.x, me.pos.y) <= self.cfg.leash_range
    }

    /// Step one tile toward `(tx, ty)`, routing around simple obstacles.
    fn walk_towards(&self, ctx: &mut ControlContext, tx: u16, ty: u16) {
        let Some(me) = ctx.me() else { return };
        let dx = tx as i32 - me.pos.x as i32;
        let dy = ty as i32 - me.pos.y as i32;
        let Some(desired) = Heading::from_delta(dx, dy) else { return };

        // Try the desired heading first, then nearby angles (±1, ±2) to get
        // around obstacles — mirrors find_step_direction() in helpers.lua.
        let desired_raw = u8::from(desired);
        let candidates = [
            desired_raw,
            (desired_raw + 1) % 8,
            (desired_raw + 7) % 8,
            (desired_raw + 2) % 8,
            (desired_raw + 6) % 8,
        ];
        for raw in candidates {
            let Some(h) = Heading::from_raw(raw) else { continue };
            if ctx.test_step(me.pos.x, me.pos.y, me.pos.z, h).is_some() {
                let _ = ctx.step(Facing::from_heading(h));
                return;
            }
        }
    }

    /// Attempt a melee swing on `target_serial`.  Returns `true` if a swing
    /// landed (damage dealt), `false` if out of range or on cooldown.
    fn try_melee(&mut self, ctx: &mut ControlContext, target_serial: u32) -> bool {
        let now = Self::now_ms();
        if now.saturating_sub(self.last_swing_ms) < self.cfg.swing_delay_ms {
            return false;
        }
        let Some(me) = ctx.me() else { return false };
        let Some(t) = ctx.get_entity(target_serial) else { return false };

        let d = Self::dist(me.pos.x, me.pos.y, t.pos.x, t.pos.y);
        if d > melee::MELEE_RANGE_1H {
            return false;
        }

        // Face the target.
        let fdx = t.pos.x as i32 - me.pos.x as i32;
        let fdy = t.pos.y as i32 - me.pos.y as i32;
        if let Some(h) = Heading::from_delta(fdx, fdy) {
            let _ = ctx.set_direction(Facing::from_heading(h));
        }

        // Attack animation + hit sound.
        ctx.animate(ctx.entity_serial, anim::SLASH_1H, 5, 1, false, false, 0, me.pos.x, me.pos.y);

        let dmg = rand::rng().random_range(self.cfg.damage_min..=self.cfg.damage_max);
        let killed = ctx.deal_damage(target_serial, dmg).map(|(_, k)| k).unwrap_or(false);

        ctx.play_sound(sound::SWORD_1, t.pos.x, t.pos.y, t.pos.z as i16);

        self.last_swing_ms = now;

        if killed {
            self.target = None;
            self.state = AiState::Return;
        }
        true
    }

    /// Run one step of the AI state machine.  Called from the repeating timer.
    fn ai_step(&mut self, ctx: &mut ControlContext) {
        // Confirm we are still alive and present.
        let Some(me) = ctx.me() else { return };
        if me.hits.unwrap_or(0) == 0 {
            return;
        }

        match self.state {
            AiState::Idle => {
                if let Some(t) = self.find_target(ctx) {
                    self.target = Some(t);
                    self.state = AiState::Combat;
                    ctx.say(
                        ctx.entity_serial, me.graphic, speech_type::EMOTE, 0x0021, 3,
                        me.name.clone().unwrap_or_default(),
                        "* growls *".to_string(), me.pos.x, me.pos.y,
                    );
                } else if let Some((hx, hy)) = self.home {
                    // Wander near home.
                    if Self::dist(hx, hy, me.pos.x, me.pos.y) > 5 {
                        self.walk_towards(ctx, hx, hy);
                    } else {
                        let dir = rand::rng().random_range(0u8..8);
                        if let Some(h) = Heading::from_raw(dir) {
                            let _ = ctx.step(Facing::from_heading(h));
                        }
                    }
                }
            }
            AiState::Combat => {
                if !self.target_valid(ctx) {
                    self.target = None;
                    self.state = AiState::Return;
                    return;
                }
                let target_serial = self.target.unwrap();
                let Some(t) = ctx.get_entity(target_serial) else {
                    self.target = None;
                    self.state = AiState::Return;
                    return;
                };
                let d = Self::dist(me.pos.x, me.pos.y, t.pos.x, t.pos.y);
                if d <= melee::MELEE_RANGE_1H {
                    self.try_melee(ctx, target_serial);
                } else {
                    self.walk_towards(ctx, t.pos.x, t.pos.y);
                }
            }
            AiState::Return => {
                match self.home {
                    Some((hx, hy)) if Self::dist(hx, hy, me.pos.x, me.pos.y) > 2 => {
                        self.walk_towards(ctx, hx, hy);
                    }
                    _ => self.state = AiState::Idle,
                }
            }
        }
    }
}

impl EntityController<DemoControllerDef> for MonsterController {
    fn tick(&mut self, ctx: &mut ControlContext, _dt: Duration) {
        if !self.initialised {
            use framework::anima::TaskAction;
            // Learn the spawn anchor from the current position.
            if let Some(me) = ctx.me() {
                self.home = Some((me.pos.x, me.pos.y));
            }
            // See nearby entity movement so combat stays responsive.
            ctx.subscribe_world_events(self.cfg.aggro_range + 5);
            // Random initial delay in [0, STEP_TICK) to break lockstep.
            let initial_ms = rand::rng().random_range(
                0..Self::STEP_TICK.as_millis() as u64,
            );
            let map_id = ctx.map_id();
            ctx.scheduler.schedule(
                Duration::from_millis(initial_ms),
                TaskAction::FireTimer {
                    entity_serial: ctx.entity_serial,
                    timer_id: Self::TIMER_ID,
                },
                Some(map_id),
            );
            self.initialised = true;
        }
    }

    fn on_event(&mut self, ctx: &mut ControlContext, event: EntityEvent) {
        match event {
            EntityEvent::TimerFired { timer_id: Self::TIMER_ID } => {
                // Schedule the *next* step first (one-shot with jitter).
                self.schedule_next_step(ctx);
                self.ai_step(ctx);
            }
            EntityEvent::DamageReceived { source_serial, .. } => {
                if source_serial == 0 {
                    return;
                }
                // Aggro-switch to the attacker if it is valid prey.
                if let Some(attacker) = ctx.get_entity(source_serial) {
                    if attacker.is_mobile && Self::is_prey_noto(attacker.notoriety) {
                        self.target = Some(source_serial);
                        self.state = AiState::Combat;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "monster"
    }

    fn next_tick_at(&self) -> Option<tokio::time::Instant> {
        if !self.initialised {
            // First tick must run to register the repeating timer + subscription.
            Some(tokio::time::Instant::now())
        } else {
            None // scheduler drives subsequent wakeups
        }
    }
}

// ── TeleporterController ────────────────────────────────────────────────────

use crate::teleporters::{self, TeleportDest, TeleportFilter};

/// Per-object teleporter controller.
///
/// Attached to a world item carrying `teleport_*` meta keys (see
/// [`crate::teleporters`]).  Reacts to the engine's
/// [`EntityEvent::SteppedOnBy`] — emitted by `process_step_on_triggers`
/// after *any* mobile completes a step or teleport onto the item's tile —
/// and moves the stepping mobile to the destination:
///
/// - **Same world** (or `teleport_map` omitted): an intra-zone
///   [`ControlContext::teleport_other`], which works for any mobile.
/// - **Different world**, player mobile: delegated to the player's session
///   via [`ControlContext::send_cross_world_teleport`] (controllers cannot
///   perform a worker-level cross-map transfer themselves).
/// - **Different world**, NPC / pet: currently a no-op (TODO — needs a
///   worker-side `transfer_entity` path for non-player mobiles).
///
/// A [`TeleportFilter`] read from `teleport_filter` meta decides which
/// mobiles are transported (default: players only).
///
/// All destination state lives in `item_props.meta`, so the controller is
/// stateless and survives snapshot save/load (re-attached via
/// `SnapshotRestored`).
pub struct TeleporterController;

impl TeleporterController {
    pub fn new() -> Self {
        Self
    }

    /// Read this teleporter's destination + filter from its own item_props.
    fn read_config(ctx: &ControlContext, current_world: u8) -> Option<(TeleportDest, TeleportFilter)> {
        use common::uo_engine::item_props::ItemProps;
        let boxed = ctx.get_item_props_any(ctx.entity_serial)?;
        let props = boxed.downcast::<ItemProps>().ok()?;
        let dest = teleporters::dest_from_props(&props, current_world)?;
        let filter = teleporters::filter_from_props(&props);
        Some((dest, filter))
    }

    /// Whether a given mobile passes the teleporter's filter.
    fn passes_filter(ctx: &ControlContext, mobile: &framework::anima::EntityInfo, filter: TeleportFilter) -> bool {
        match filter {
            TeleportFilter::All => true,
            TeleportFilter::Players => mobile.is_player,
            TeleportFilter::NoPets => {
                if mobile.is_player {
                    return true;
                }
                // Non-players pass only if they are not a tamed pet
                // (pets carry a `pet_owner` meta key).
                !Self::is_pet(ctx, mobile.serial)
            }
        }
    }

    /// Whether the mobile is a tamed pet (has `pet_owner` meta).
    fn is_pet(ctx: &ControlContext, serial: u32) -> bool {
        use common::uo_engine::item_props::ItemProps;
        ctx.get_item_props_any(serial)
            .and_then(|b| b.downcast::<ItemProps>().ok())
            .map(|p| p.get_meta_int(crate::taming::META_PET_OWNER).is_some())
            .unwrap_or(false)
    }
}

impl EntityController<DemoControllerDef> for TeleporterController {
    fn on_event(&mut self, ctx: &mut ControlContext, event: EntityEvent) {
        let EntityEvent::SteppedOnBy { mobile_serial } = event else { return };

        let current_world = ctx.map_id();
        let Some((dest, filter)) = Self::read_config(ctx, current_world) else { return };

        // Resolve the stepping mobile.
        let Some(mobile) = ctx.get_entity(mobile_serial) else { return };
        if !mobile.is_mobile {
            return;
        }

        // Filter: decide whether this mobile should be transported.
        if !Self::passes_filter(ctx, &mobile, filter) {
            return;
        }

        // Self-tile guard: never re-fire onto the same tile.  The engine
        // sends `SteppedOnBy` after teleports too, so without this a
        // teleporter at the destination could loop.
        if dest.world == current_world
            && dest.x == mobile.pos.x
            && dest.y == mobile.pos.y
        {
            return;
        }

        if dest.world == current_world {
            // Intra-zone move — works for any mobile; the player's session
            // receives `EntityMoved { is_teleport }` and re-renders.
            let _ = ctx.teleport_other(mobile_serial, dest.x, dest.y, dest.z);
        } else if mobile.is_player {
            // Cross-world: hand off to the player's session for the atomic
            // transfer (SetMap + inventory + observer re-registration).
            let _ = ctx.send_cross_world_teleport(mobile_serial, dest.world, dest.x, dest.y, dest.z);
        } else {
            // TODO: cross-world transfer for NPCs / pets needs a worker-side
            // `transfer_entity` path; controllers cannot reach it.  For now
            // non-players are not moved across worlds.
        }
    }

    fn name(&self) -> &str {
        "teleporter"
    }

    fn next_tick_at(&self) -> Option<tokio::time::Instant> {
        None // purely event-driven (SteppedOnBy)
    }
}

impl Default for TeleporterController {
    fn default() -> Self {
        Self::new()
    }
}

