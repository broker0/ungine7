-- monster_ctrl.lua: Aggressive monster AI (controller mode).
--
-- Attach to any mobile via spawn_monster.lua or w:attach_controller().
-- The monster will:
--   * Scan for players in aggro range
--   * Chase the target
--   * Attack in melee range
--   * React to damage (aggro switch)
--   * Return to spawn point when idle
--   * Die properly with corpse + loot when killed by players
--
-- Configuration is passed via item_props.meta on the entity:
--   meta["aggro_range"]  — detection range (default: 10)
--   meta["leash_range"]  — max distance from spawn before reset (default: 20)
--   meta["melee_damage"] — "min,max" damage string (default: "5,15")
--   meta["swing_delay"]  — ms between attacks (default: 2500)
--
-- Usage (controller mode):
--   Attached by spawn_monster.lua via w:attach_controller(serial, "monster_ctrl.lua")

dofile("scripts/mob_controller.lua")

-- ═════════════════════════════════════════════════════════════════════
-- Read configuration from item_props meta (set by spawner)
-- ═════════════════════════════════════════════════════════════════════

local w = World()
local mob = MobController.attach(w)

local me_initial = mob:me()
if not me_initial then
    log("[monster_ctrl] ERROR: entity not found, aborting")
    return
end

local SPAWN_X, SPAWN_Y = me_initial.x, me_initial.y

-- Read config from item_props.meta (set by spawn_monster.lua).
local props = w:get_item_props(mob.serial)
local meta = props and props.meta or {}

local AGGRO_RANGE    = tonumber(meta.aggro_range) or 10
local LEASH_RANGE    = tonumber(meta.leash_range) or 20
local SWING_DELAY    = tonumber(meta.swing_delay) or 2500
local STEP_DELAY     = 400

-- Parse "min,max" damage string.
local dmg_str = meta.melee_damage or "5,15"
local DAMAGE_MIN, DAMAGE_MAX = dmg_str:match("(%d+),(%d+)")
DAMAGE_MIN = tonumber(DAMAGE_MIN) or 5
DAMAGE_MAX = tonumber(DAMAGE_MAX) or 15

-- Subscribe to world events so we can see other entity movements.
w:subscribe_world_events(AGGRO_RANGE + 5)

log(string.format("[monster_ctrl] 0x%08X alive at (%d,%d) aggro=%d leash=%d dmg=%d-%d",
    mob.serial, SPAWN_X, SPAWN_Y, AGGRO_RANGE, LEASH_RANGE, DAMAGE_MIN, DAMAGE_MAX))

-- ═════════════════════════════════════════════════════════════════════
-- State
-- ═════════════════════════════════════════════════════════════════════

local state = "idle"         -- idle, chase, combat, return
local target_serial = nil
local last_swing_time = 0

-- ═════════════════════════════════════════════════════════════════════
-- AI Helpers
-- ═════════════════════════════════════════════════════════════════════

--- Find the closest visible player within aggro range.
local function find_target()
    local nearby = mob:find_nearby(AGGRO_RANGE, function(e)
        -- Only attack non-monster entities (notoriety 1-4).
        -- Monsters are typically Enemy (5) or Murderer (6).
        return e.is_mobile
           and e.notoriety ~= nil
           and e.notoriety <= 4
           and (e.hits or 0) > 0
    end)

    local me = mob:me()
    if not me then return nil end

    local best, best_dist = nil, 999
    for _, e in ipairs(nearby) do
        local d = distance(me.x, me.y, e.x, e.y)
        if d < best_dist then
            if mob:has_los(me.x, me.y, me.z + 14, e.x, e.y, e.z + 14) then
                best = e
                best_dist = d
            end
        end
    end
    return best
end

--- Check if current target is still valid.
local function is_target_valid()
    if not target_serial then return false end
    local t = mob:get_entity(target_serial)
    if not t or not t.is_mobile or (t.hits or 0) <= 0 then return false end

    local me = mob:me()
    if not me then return false end

    -- Check leash range from spawn.
    if distance(SPAWN_X, SPAWN_Y, me.x, me.y) > LEASH_RANGE then return false end
    return true
end

--- Attempt a melee attack on the target.
local function try_melee(target)
    local now = clock() * 1000  -- clock() returns seconds
    if now - last_swing_time < SWING_DELAY then return false end

    local me = mob:me()
    if not me then return false end

    local dist = distance(me.x, me.y, target.x, target.y)
    if dist > COMBAT.MELEE_RANGE then return false end

    -- Face the target.
    mob:face_towards(target.serial)

    -- Attack animation.
    mob:animate(ANIM.SLASH_1H, 7)

    -- Deal damage.
    local dmg = math.random(DAMAGE_MIN, DAMAGE_MAX)
    local result = mob:deal_damage(target.serial, dmg)

    -- Hit sound.
    mob:play_sound(SOUND.SWORD_HIT, target.x, target.y, target.z)

    last_swing_time = now

    if result and result.killed then
        log(string.format("[monster_ctrl] killed 0x%08X", target.serial))
        target_serial = nil
        state = "return"
    end

    return true
end

-- ═════════════════════════════════════════════════════════════════════
-- Process incoming events (damage → aggro switch)
-- ═════════════════════════════════════════════════════════════════════

local function process_events()
    while true do
        -- Check entity events first (damage to us).
        local ev = mob:poll_event()
        if not ev then break end

        if ev.type == "damage_received" and ev.source_serial and ev.source_serial ~= 0 then
            local attacker = mob:get_entity(ev.source_serial)
            if attacker and attacker.is_mobile and attacker.notoriety and attacker.notoriety <= 4 then
                target_serial = ev.source_serial
                state = "combat"
            end
        end

        if ev.type == "killed" then
            -- We've been killed — entity is already being removed.
            log(string.format("[monster_ctrl] 0x%08X was killed", mob.serial))
            return true  -- signal to exit main loop
        end
    end
    return false  -- not dead
end

-- ═════════════════════════════════════════════════════════════════════
-- Main AI Loop
-- ═════════════════════════════════════════════════════════════════════

while true do
    -- Process events. Exit if we were killed.
    if process_events() then break end

    -- Check if we're still alive.
    local me = mob:me()
    if not me or me.hits <= 0 then break end

    -- ── State machine ────────────────────────────────────────────

    if state == "idle" then
        -- Scan for targets.
        local target = find_target()
        if target then
            target_serial = target.serial
            state = "chase"
            mob:say("* growls *", { speech_type = 2, color = 0x0021 })
        else
            -- Wander near spawn.
            local dist_from_spawn = distance(SPAWN_X, SPAWN_Y, me.x, me.y)
            if dist_from_spawn > 5 then
                mob:walk_towards(SPAWN_X, SPAWN_Y)
            else
                mob:step(math.random(0, 7))
            end
            -- Use wait_event so we wake up instantly on damage.
            mob:wait_event(3000)
        end

    elseif state == "chase" or state == "combat" then
        if not is_target_valid() then
            target_serial = nil
            state = "return"
            goto continue
        end

        local target = mob:get_entity(target_serial)
        if not target then
            state = "return"
            goto continue
        end

        local dist = distance(me.x, me.y, target.x, target.y)
        state = "combat"

        -- Priority: melee if in range, otherwise chase.
        if dist <= COMBAT.MELEE_RANGE then
            if not try_melee(target) then
                -- Swing on cooldown, wait a bit but stay responsive.
                mob:wait_event(200)
            else
                -- After a successful hit, wait for swing delay but
                -- stay responsive to events (damage, target death).
                mob:wait_event(SWING_DELAY)
            end
        else
            -- Chase towards target.
            mob:walk_towards(target.x, target.y)
            sleep(STEP_DELAY)
        end

    elseif state == "return" then
        local dist = distance(SPAWN_X, SPAWN_Y, me.x, me.y)
        if dist <= 2 then
            state = "idle"
        else
            mob:walk_towards(SPAWN_X, SPAWN_Y)
            sleep(STEP_DELAY)
        end
    end

    ::continue::
end

log(string.format("[monster_ctrl] 0x%08X loop ended", mob.serial))
