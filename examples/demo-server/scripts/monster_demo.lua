-- monster_demo.lua: Aggressive monster AI demo (async mode).
--
-- Spawns a daemon that:
--   * Scans for players in aggro range
--   * Checks line of sight before engaging
--   * Chases the target until out of leash range
--   * Casts spells at range (energy bolt, lightning)
--   * Attacks physically in melee
--   * Reacts to damage (aggro switch)
--   * Heals itself when low on HP
--   * Returns to spawn point when idle
--
-- Usage:
--   .lua scripts/monster_demo.lua
--
-- To adapt for controller mode, change the construction line:
--   local mob = MobController.attach(World())
-- and remove the spawn_npc call.

dofile("scripts/mob_controller.lua")
dofile("scripts/scene/outfit.lua")

-- ═════════════════════════════════════════════════════════════════════
-- Configuration
-- ═════════════════════════════════════════════════════════════════════

local SPAWN_X, SPAWN_Y, SPAWN_Z = 1438, 1696, 0
local BODY_GRAPHIC = 0x000A  -- Daemon

local AGGRO_RANGE    = 10   -- detection range (Chebyshev)
local CHASE_RANGE    = 16   -- max chase distance from spawn
local MELEE_RANGE    = 1    -- melee attack range
local SPELL_RANGE    = 8    -- max spell casting range
local LEASH_RANGE    = 20   -- max distance from spawn before reset

local MELEE_DAMAGE_MIN = 8
local MELEE_DAMAGE_MAX = 18
local MELEE_COOLDOWN   = 2000  -- ms between melee attacks

local HEAL_THRESHOLD   = 0.4  -- heal when below 40% HP
local HEAL_COOLDOWN    = 5000  -- ms between heals

local STEP_DELAY       = 400  -- ms between movement steps
local IDLE_WANDER_DELAY = 3000 -- ms between idle wander steps

-- ── Spell table ─────────────────────────────────────────────────────
-- Spell definitions are loaded from the Rust SpellDef table via
-- w:get_spell(id).  We only store AI-specific fields here (cooldown,
-- range, priority order).  The actual mana cost, damage, sound,
-- effects are all in Rust — single source of truth.

local SPELL_IDS = {
    { id = 42, range = SPELL_RANGE, cooldown = 4000, last_used = 0 },  -- Energy Bolt
    { id = 30, range = SPELL_RANGE, cooldown = 3000, last_used = 0 },  -- Lightning
    { id = 5,  range = SPELL_RANGE, cooldown = 2000, last_used = 0 },  -- Magic Arrow
}

-- ═════════════════════════════════════════════════════════════════════
-- Spawn the monster
-- ═════════════════════════════════════════════════════════════════════

local w = World(0)
local mob = MobController.spawn(w, {
    graphic = BODY_GRAPHIC,
    x = SPAWN_X,
    y = SPAWN_Y,
    z = SPAWN_Z,
    name = "Daemon Lord",
    color = 0,
    notoriety = 6,  -- Murderer (red)
    hits = 250,
    hits_max = 250,
})

-- Set initial mana (use the entity serial)
mob:modify_mana(mob.serial, 100)  -- will be clamped to max

-- Register cleanup to remove the monster on script stop/reload.
register_cleanup(function()
    mob:remove()
end)

log(string.format("Daemon Lord spawned at (%d, %d, %d)", SPAWN_X, SPAWN_Y, SPAWN_Z))

-- ═════════════════════════════════════════════════════════════════════
-- State
-- ═════════════════════════════════════════════════════════════════════

local state = "idle"        -- idle, chase, combat, return
local target_serial = nil
local last_melee_time = 0
local last_heal_time = 0
local tick_count = 0

-- Pseudo-clock: we track time via sleep() calls. Not precise, but
-- sufficient for AI timing.
local clock = 0

local function now()
    return clock
end

-- ═════════════════════════════════════════════════════════════════════
-- AI Helpers
-- ═════════════════════════════════════════════════════════════════════

--- Check if we should heal ourselves.
local function try_self_heal()
    local me = mob:me()
    if not me then return false end
    if me.hits / me.hits_max > HEAL_THRESHOLD then return false end
    if now() - last_heal_time < HEAL_COOLDOWN then return false end

    -- Use the Rust Heal spell (id=4) which checks mana internally.
    local heal_spell = w:get_spell(4)
    if not heal_spell then return false end
    if me.mana < heal_spell.mana then return false end

    w:cast_spell(mob.serial, mob.serial, 4)
    last_heal_time = now()
    log(string.format("cast Heal on self (HP: %d/%d)", me.hits, me.hits_max))
    return true
end

--- Try to cast a spell at the target.
local function try_cast_spell(target)
    local me = mob:me()
    if not me then return false end

    local dist = distance(me.x, me.y, target.x, target.y)

    for _, entry in ipairs(SPELL_IDS) do
        local spell = w:get_spell(entry.id)
        if spell and dist <= entry.range
            and me.mana >= spell.mana
            and now() - entry.last_used >= entry.cooldown
        then
            -- LOS check (eye height +14)
            if not mob:has_los(me.x, me.y, me.z + 14, target.x, target.y, target.z + 14) then
                return false
            end

            -- Cast through the unified Rust magic system.
            -- This handles: mana, LOS, animation, spell words, effects, damage.
            w:cast_spell(mob.serial, target.serial, entry.id)

            entry.last_used = now()
            log(string.format("%s -> 0x%08X", spell.name, target.serial))
            return true
        end
    end

    return false
end

--- Perform a melee attack.
local function try_melee_attack(target)
    if now() - last_melee_time < MELEE_COOLDOWN then return false end

    local me = mob:me()
    if not me then return false end

    local dist = distance(me.x, me.y, target.x, target.y)
    if dist > MELEE_RANGE then return false end

    -- Face the target
    mob:face_towards(target.serial)

    -- Attack animation (use slash)
    mob:animate(ANIM.SLASH_1H, 7)

    local dmg = math.random(MELEE_DAMAGE_MIN, MELEE_DAMAGE_MAX)
    local result = mob:deal_damage(target.serial, dmg)

    mob:play_sound(SOUND.SWORD_HIT, target.x, target.y, target.z)

    last_melee_time = now()

    if result then
        log(string.format("melee -> 0x%08X for %d damage (HP: %d)",
            target.serial, dmg, result.new_hits))
        if result.killed then
            log(string.format("killed target 0x%08X!", target.serial))
        end
    end

    return true
end

--- Find the best target among nearby entities.
local function find_target()
    local me = mob:me()
    if not me then return nil end

    local nearby = mob:find_nearby(AGGRO_RANGE, function(e)
        return MobController.is_player(e)
           and (e.hits or 0) > 0
    end)

    -- Pick the closest visible player.
    local best = nil
    local best_dist = 999
    for _, e in ipairs(nearby) do
        local d = distance(me.x, me.y, e.x, e.y)
        if d < best_dist then
            -- LOS check
            if mob:has_los(me.x, me.y, me.z + 14, e.x, e.y, e.z + 14) then
                best = e
                best_dist = d
            end
        end
    end

    return best
end

--- Check if target is still valid.
local function is_target_valid(serial)
    local t = mob:get_entity(serial)
    if not t then return false end
    if not t.is_mobile then return false end
    if (t.hits or 0) <= 0 then return false end

    local me = mob:me()
    if not me then return false end

    -- Check leash range (from spawn)
    local dist_from_spawn = distance(SPAWN_X, SPAWN_Y, me.x, me.y)
    if dist_from_spawn > LEASH_RANGE then return false end

    -- Check chase range (from target)
    local dist_to_target = distance(me.x, me.y, t.x, t.y)
    if dist_to_target > CHASE_RANGE then return false end

    return true
end

-- ═════════════════════════════════════════════════════════════════════
-- Main AI Loop
-- ═════════════════════════════════════════════════════════════════════

while true do
    tick_count = tick_count + 1

    -- Process events (damage received -> aggro switch)
    while true do
        local ev = mob:poll_event()
        if not ev then break end
        if ev.type == "damage_received" and ev.source_serial and ev.source_serial ~= 0 then
            -- Switch target to attacker
            local attacker = mob:get_entity(ev.source_serial)
            if attacker and attacker.is_mobile and MobController.is_player(attacker) then
                target_serial = ev.source_serial
                state = "combat"
                log(string.format("aggro switch -> 0x%08X (took %d damage)",
                    ev.source_serial, ev.amount))
            end
        end
    end

    -- Priority: self-heal
    if try_self_heal() then
        clock = clock + 1500
        sleep(1500)
        goto continue
    end

    -- ── State machine ────────────────────────────────────────────────

    if state == "idle" then
        -- Scan for targets
        local target = find_target()
        if target then
            target_serial = target.serial
            state = "chase"
            mob:say("* growls menacingly *", { speech_type = 2, color = 0x0021 })
            log(string.format("spotted player 0x%08X, engaging!", target.serial))
        else
            -- Wander randomly near spawn
            local me = mob:me()
            if me then
                local dist_from_spawn = distance(SPAWN_X, SPAWN_Y, me.x, me.y)
                if dist_from_spawn > 5 then
                    -- Walk back towards spawn
                    mob:walk_towards(SPAWN_X, SPAWN_Y)
                else
                    mob:step(math.random(0, 7))
                end
            end
            clock = clock + IDLE_WANDER_DELAY
            sleep(IDLE_WANDER_DELAY)
        end

    elseif state == "chase" or state == "combat" then
        -- Validate target
        if not is_target_valid(target_serial) then
            target_serial = nil
            state = "return"
            log("target lost, returning to spawn")
            goto continue
        end

        local target = mob:get_entity(target_serial)
        local me = mob:me()
        if not target or not me then
            state = "return"
            goto continue
        end

        local dist = distance(me.x, me.y, target.x, target.y)
        state = "combat"

        -- Decision priority:
        -- 1. Melee if in range
        -- 2. Cast spell if in spell range
        -- 3. Chase (move closer)

        if dist <= MELEE_RANGE then
            if try_melee_attack(target) then
                clock = clock + MELEE_COOLDOWN
                sleep(MELEE_COOLDOWN)
            else
                -- Melee on cooldown, try spell
                if not try_cast_spell(target) then
                    clock = clock + 500
                    sleep(500)
                else
                    clock = clock + 1500
                    sleep(1500)
                end
            end
        elseif dist <= SPELL_RANGE then
            if try_cast_spell(target) then
                clock = clock + 1500
                sleep(1500)
            else
                -- Can't cast, move closer
                mob:walk_towards(target.x, target.y)
                clock = clock + STEP_DELAY
                sleep(STEP_DELAY)
            end
        else
            -- Too far for spells, chase
            mob:walk_towards(target.x, target.y)
            clock = clock + STEP_DELAY
            sleep(STEP_DELAY)
        end

    elseif state == "return" then
        -- Walk back to spawn point
        local me = mob:me()
        if me then
            local dist = distance(SPAWN_X, SPAWN_Y, me.x, me.y)
            if dist <= 2 then
                state = "idle"
                log("returned to spawn, resuming idle")
            else
                mob:walk_towards(SPAWN_X, SPAWN_Y)
                clock = clock + STEP_DELAY
                sleep(STEP_DELAY)
            end
        else
            state = "idle"
        end
    end

    -- Default tick delay (for states that didn't sleep explicitly)
    if state == "idle" then
        -- Already slept above
    elseif state == "chase" or state == "combat" then
        -- Already slept above
    else
        clock = clock + 200
        sleep(200)
    end

    ::continue::
end
