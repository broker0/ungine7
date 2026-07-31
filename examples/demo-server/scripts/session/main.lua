-- session/main.lua — Session game logic entry point.
--
-- Load with:  .slua scripts/session/main.lua
--
-- This is the main script for lua-session mode. It loads all modules
-- and runs the event loop that dispatches packets and world events
-- to the appropriate handlers.
--
-- Currently implements:
--   * Spell casting with reagent/scroll support
--   * Bandage healing with timed delay
--   * Spell fizzle on damage
--   * Stat regeneration (HP / mana / stamina)
--   * Meditation (skill 46) — boosted mana regen, interrupted by actions/damage
--
-- To add new systems, create a new module in scripts/session/ and:
--   1. dofile() it below
--   2. Add handlers to dispatch_event() in handlers.lua
--   3. Add timer ticks to the main loop below (if needed)
--
-- Available globals: engine, session, broadcast, log, sleep, poll_event, wait_event

-- ══════════════════════════════════════════════════════════════════════════
-- Module loading (order matters — later modules depend on earlier ones)
-- ══════════════════════════════════════════════════════════════════════════

dofile("scripts/constants/animations.lua")  -- ANIM table
dofile("scripts/constants/sounds.lua")     -- SOUND table
dofile("scripts/constants/effects.lua")    -- EFFECT table
dofile("scripts/constants/layers.lua")     -- LAYER table
dofile("scripts/constants/hues.lua")       -- HUE table
dofile("scripts/constants/combat.lua")     -- COMBAT, DAMAGE, EYE_HEIGHT
dofile("scripts/constants/regen.lua")      -- REGEN table
dofile("scripts/constants/items.lua")      -- REAGENT, BANDAGE_GRAPHIC, BANDAGE
dofile("scripts/constants/spells.lua")     -- spell IDs, cursor bases
dofile("scripts/session/helpers.lua")      -- utility functions, reagent system
dofile("scripts/session/spells.lua")       -- spell definitions + begin/complete cast
dofile("scripts/session/bandage.lua")      -- bandage healing system
dofile("scripts/session/regen.lua")        -- stat regeneration + meditation
dofile("scripts/session/handlers.lua")     -- packet & event dispatch

-- ══════════════════════════════════════════════════════════════════════════
-- Session state (shared across modules via globals)
-- ══════════════════════════════════════════════════════════════════════════

pending_spell   = nil   -- { spell_def, caster_serial, cursor_id }
pending_bandage = nil   -- { healer_serial, bandage_item_serial, cursor_id }
active_cast     = nil   -- { spell_def, caster_serial, target_serial, delay_ms }
active_bandage  = nil   -- { healer_serial, target_serial, bandage_item_serial, delay_ms }
regen_timer_ms  = REGEN.TICK_MS  -- countdown to next regen tick

-- ══════════════════════════════════════════════════════════════════════════
-- Main event loop
-- ══════════════════════════════════════════════════════════════════════════

log("session loaded — spells | bandages | fizzle-on-damage | regen | meditation")

while true do
    -- Determine wait time: nearest active timer or default poll interval.
    local wait_ms = 50

    if active_cast then
        wait_ms = math.min(wait_ms, active_cast.delay_ms)
    end
    if active_bandage then
        wait_ms = math.min(wait_ms, active_bandage.delay_ms)
    end
    wait_ms = math.min(wait_ms, regen_timer_ms)

    -- Wait for events or timeout.
    -- wait_event returns (event_or_nil, actual_elapsed_ms).
    local ev, elapsed = wait_event(math.max(wait_ms, 1))
    if not elapsed or elapsed < 1 then elapsed = 1 end

    -- Tick down active timers.
    if active_cast then
        active_cast.delay_ms = active_cast.delay_ms - elapsed
        if active_cast.delay_ms <= 0 then
            local ac = active_cast
            active_cast = nil
            complete_cast(ac.spell_def, ac.caster_serial, ac.target_serial, ac.scroll_item_serial)
        end
    end

    if active_bandage then
        active_bandage.delay_ms = active_bandage.delay_ms - elapsed
        if active_bandage.delay_ms <= 0 then
            local ab = active_bandage
            active_bandage = nil
            complete_bandage(ab.healer_serial, ab.target_serial, ab.bandage_item_serial)
        end
    end

    -- Regen tick.
    regen_timer_ms = regen_timer_ms - elapsed
    if regen_timer_ms <= 0 then
        regen_tick()
        regen_timer_ms = REGEN.TICK_MS
    end

    -- Process events.
    if ev then
        dispatch_event(ev)

        -- Drain buffered events.
        while true do
            local extra = poll_event()
            if not extra then break end
            dispatch_event(extra)
        end
    end
end
