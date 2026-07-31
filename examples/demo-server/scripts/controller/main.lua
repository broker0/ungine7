---@access safe
-- controller/main.lua — Player controller entry point (controller-session mode).
--
-- Runs inside the worker tick as a LuaController attached to the player
-- entity.  Receives player input via poll_command()/wait_command() and
-- game events via poll_event()/wait_event().
--
-- All world operations (deal_damage, play_sound, animate, etc.) execute
-- synchronously with zero RPC overhead.
--
-- To add new systems, create a new module in scripts/controller/ and:
--   1. dofile() it below
--   2. Add handlers to handle_command() / handle_event() in handlers.lua
--   3. Add timer ticks to the main loop below (if needed)
--
-- Available globals: World, log, clock, sleep, poll_command, wait_command,
--                    poll_event, wait_event

-- ══════════════════════════════════════════════════════════════════════════
-- Core globals (available to all modules)
-- ══════════════════════════════════════════════════════════════════════════

w  = World()
me = w:serial()

-- ══════════════════════════════════════════════════════════════════════════
-- Module loading (order matters — later modules depend on earlier ones)
-- ══════════════════════════════════════════════════════════════════════════

dofile("scripts/constants/animations.lua")  -- ANIM table
dofile("scripts/constants/sounds.lua")     -- SOUND table
dofile("scripts/constants/effects.lua")    -- EFFECT table
dofile("scripts/constants/hues.lua")       -- HUE table
dofile("scripts/constants/layers.lua")     -- LAYER table
dofile("scripts/constants/items.lua")      -- REAGENT, BANDAGE tables
dofile("scripts/constants/spells.lua")     -- SPELL_CURSOR_BASE, spell ID constants
dofile("scripts/constants/combat.lua")     -- COMBAT, DAMAGE, EYE_HEIGHT
dofile("scripts/constants/regen.lua")      -- REGEN table
dofile("scripts/controller/helpers.lua")   -- utility functions
dofile("scripts/controller/combat.lua")    -- target management, melee swing
dofile("scripts/controller/regen.lua")     -- stat regeneration
dofile("scripts/controller/spells.lua")    -- spell casting (two-phase)
dofile("scripts/controller/handlers.lua")  -- command & event dispatch

-- ══════════════════════════════════════════════════════════════════════════
-- Main loop
-- ══════════════════════════════════════════════════════════════════════════

log("player controller started for 0x" .. string.format("%08X", me))

next_regen_at = clock() + REGEN.TICK_MS / 1000.0

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

    -- Check if an active spell cast has completed.
    check_active_cast()

    -- Calculate sleep until next interesting time
    local next_time = next_regen_at
    if has_targets() and next_swing_at < next_time then
        next_time = next_swing_at
    end
    local cast_time = next_cast_time()
    if cast_time and cast_time < next_time then
        next_time = cast_time
    end
    local remaining_ms = math.max(1, math.floor((next_time - clock()) * 1000))
    local wait_ms = math.min(remaining_ms, 100)

    -- Yield to let other controllers run.
    sleep(wait_ms)
end
