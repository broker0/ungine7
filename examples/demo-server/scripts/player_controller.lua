---@access safe
-- Player Controller (controller-session mode)
--
-- Runs inside the worker tick as a LuaController attached to the player
-- entity.  Receives player input via poll_command()/wait_command() and
-- game events via poll_event()/wait_event().
--
-- All world operations (deal_damage, play_sound, animate, etc.) execute
-- synchronously with zero RPC overhead.
--
-- This is a minimal melee combat PoC.  Spell casting, skills, bandages,
-- and regen are left as TODOs.

dofile("scripts/constants/animations.lua")  -- ANIM table
dofile("scripts/constants/sounds.lua")      -- SOUND table
dofile("scripts/constants/layers.lua")      -- LAYER table

local w = World()
local me = w:serial()

-- ── Constants ────────────────────────────────────────────────────────────

local SWING_DELAY_MS    = 2500   -- time between melee swings
local MELEE_RANGE       = 2      -- Chebyshev tiles
local EYE_HEIGHT        = 15     -- LOS offset (z)

-- Damage
local DMG_MIN           = 3
local DMG_MAX           = 12

-- Regen
local REGEN_TICK_MS     = 2000
local HP_PER_TICK       = 1
local MANA_PER_TICK     = 1
local STAMINA_PER_TICK  = 2

-- ── State ────────────────────────────────────────────────────────────────

local targets = {}           -- set of target serials: { [serial] = true }
local primary_target = nil   -- current primary attack target serial
local war_mode = false       -- peace/war mode

local next_swing_at = 0      -- clock() time of next allowed swing
local next_regen_at = 0      -- clock() time of next regen tick

-- ── Helpers ──────────────────────────────────────────────────────────────

local function chebyshev(x1, y1, x2, y2)
    local dx = math.abs(x1 - x2)
    local dy = math.abs(y1 - y2)
    return math.max(dx, dy)
end

local function random_range(lo, hi)
    return math.random(lo, hi)
end

local function entity_is_mounted(entity)
    if not entity or not entity.items then return false end
    for _, eq in ipairs(entity.items) do
        if eq.layer == LAYER.MOUNT then return true end
    end
    return false
end

--- Resolve animation ID accounting for mount state.
--- Returns the resolved action or nil if the animation should be skipped.
local function resolve_animation(action, mounted)
    if not mounted then return action end
    -- Melee attacks → mounted attack
    if action == ANIM.SLASH_1H  or action == ANIM.PIERCE_1H
    or action == ANIM.SWING_2H  or action == ANIM.SLASH_2H
    or action == ANIM.PIERCE_2H
    or action == ANIM.SHOOT_BOW or action == ANIM.SHOOT_XBOW then
        return ANIM.MOUNTED_ATTACK
    end
    -- Get hit → mounted get hit
    if action == ANIM.GET_HIT then
        return ANIM.MOUNTED_GET_HIT
    end
    -- Cast → mounted cast
    if action == ANIM.CAST_DIRECTED then return ANIM.MOUNTED_CAST_DIRECTED end
    if action == ANIM.CAST_AREA    then return ANIM.MOUNTED_CAST_AREA end
    -- Emotes / eat — no mounted variant → skip
    if action == ANIM.BOW or action == ANIM.SALUTE or action == ANIM.EAT then
        return nil
    end
    return action
end

--- Pick a random hurt (pain) sound appropriate for the character's body.
local FEMALE_HUMAN_GFX = 0x0191
local MALE_HURT_POOL   = { 0x0154, 0x0155, 0x0156, 0x0157, 0x0158 }
local FEMALE_HURT_POOL = { 0x014B, 0x014C, 0x014D, 0x014E, 0x014F }

local function random_hurt_sound(graphic)
    local pool = (graphic == FEMALE_HUMAN_GFX) and FEMALE_HURT_POOL or MALE_HURT_POOL
    return pool[math.random(#pool)]
end

local function has_targets()
    return next(targets) ~= nil
end

local function remove_target(serial)
    targets[serial] = nil
    if primary_target == serial then
        primary_target = next(targets)
    end
end

-- ── Combat: try a melee swing ────────────────────────────────────────────

local function try_swing()
    if not primary_target then return end

    local self_info = w:get_entity(me)
    if not self_info then return end

    local target_info = w:get_entity(primary_target)
    if not target_info then
        remove_target(primary_target)
        return
    end

    -- Range check
    local dist = chebyshev(self_info.x, self_info.y, target_info.x, target_info.y)
    if dist > MELEE_RANGE then return end

    -- LOS check
    if not w:has_los(
        self_info.x, self_info.y, self_info.z + EYE_HEIGHT,
        target_info.x, target_info.y, target_info.z + EYE_HEIGHT
    ) then return end

    -- Swing animation + sound
    local self_mounted = entity_is_mounted(self_info)
    local attack_anim = resolve_animation(ANIM.SLASH_1H, self_mounted)
    if attack_anim then
        w:animate(me, attack_anim, 7, { repeat_count = 1 })
    end
    w:play_sound(SOUND.SWING, self_info.x, self_info.y, self_info.z)

    -- Deal damage
    local amount = random_range(DMG_MIN, DMG_MAX)
    local new_hp, killed = w:deal_damage(primary_target, amount)

    -- Hit sound + animation on target
    w:play_sound(SOUND.FIST_HIT, target_info.x, target_info.y, target_info.z)
    local target_mounted = entity_is_mounted(target_info)
    local hit_anim = resolve_animation(ANIM.GET_HIT, target_mounted)
    if hit_anim then
        w:animate(primary_target, hit_anim, 5, { repeat_count = 1 })
    end

    -- Gender-aware hurt sound on the target
    if target_info.graphic then
        local hurt_snd = random_hurt_sound(target_info.graphic)
        w:play_sound(hurt_snd, target_info.x, target_info.y, target_info.z)
    end

    if killed then
        remove_target(primary_target)
    end

    next_swing_at = clock() + SWING_DELAY_MS / 1000.0
end

-- ── Regen tick ───────────────────────────────────────────────────────────

local function regen_tick()
    local info = w:get_entity(me)
    if not info then return end

    if info.hits and info.hits_max and info.hits < info.hits_max then
        w:heal_entity(me, HP_PER_TICK)
    end
    w:modify_mana(me, MANA_PER_TICK)
    w:modify_stamina(me, STAMINA_PER_TICK)

    next_regen_at = clock() + REGEN_TICK_MS / 1000.0
end

-- ── Command handlers ─────────────────────────────────────────────────────

local function handle_command(cmd)
    if not cmd then return end

    if cmd.type == "attack" then
        local t = cmd.target_serial
        if t and t ~= 0 and t ~= me then
            targets[t] = true
            primary_target = t
            -- Try immediate swing if charged
            if clock() >= next_swing_at then
                try_swing()
            end
        end

    elseif cmd.type == "cancel_attack" then
        targets = {}
        primary_target = nil

    elseif cmd.type == "toggle_war_mode" then
        war_mode = cmd.fighting
        if not war_mode then
            targets = {}
            primary_target = nil
        end

    elseif cmd.type == "move" then
        -- Movement is handled by the session (infra) and the engine.
        -- The controller receives it but doesn't need to do anything
        -- for now — the engine already moved us.

    elseif cmd.type == "cast_spell" then
        -- TODO: implement spell casting in controller
        w:send_message(me, "Spell casting not yet implemented in controller mode.", 0x0035)

    elseif cmd.type == "use_skill" then
        -- TODO: implement skill use in controller
        w:send_message(me, "Skills not yet implemented in controller mode.", 0x0035)

    elseif cmd.type == "target_response" then
        -- TODO: handle target cursor responses
        log("target response: serial=" .. tostring(cmd.target_serial))
    end
end

-- ── Event handlers ───────────────────────────────────────────────────────

local function handle_event(ev)
    if not ev then return end

    if ev.type == "damage_received" then
        -- Auto-retaliate
        local src = ev.source_serial
        if src and src ~= 0 and src ~= me then
            local was_empty = not has_targets()
            targets[src] = true
            if not primary_target then
                primary_target = src
            end
        end

    elseif ev.type == "timer_fired" then
        -- Not using scheduler timers yet; using clock() instead.
    end
end

-- ── Main loop ────────────────────────────────────────────────────────────

log("player controller started for 0x" .. string.format("%08X", me))

next_regen_at = clock() + REGEN_TICK_MS / 1000.0

while true do
    -- Poll commands (non-blocking) — drain all pending.
    -- Done first so that state is up-to-date before we act or sleep.
    local cmd = poll_command()
    while cmd do
        handle_command(cmd)
        cmd = poll_command()
    end

    -- Poll events (non-blocking) — drain all pending
    local ev = poll_event()
    while ev do
        handle_event(ev)
        ev = poll_event()
    end

    -- Execute pending actions
    local now = clock()

    if has_targets() and now >= next_swing_at then
        try_swing()
    end

    if now >= next_regen_at then
        regen_tick()
    end

    -- Calculate sleep until next interesting time
    local next_time = next_regen_at
    if has_targets() and next_swing_at < next_time then
        next_time = next_swing_at
    end
    local remaining_ms = math.max(1, math.floor((next_time - clock()) * 1000))
    local wait_ms = math.min(remaining_ms, 100)

    -- Yield to let other controllers run.
    sleep(wait_ms)
end
