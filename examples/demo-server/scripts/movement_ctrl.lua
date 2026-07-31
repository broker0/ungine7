-- movement_ctrl.lua: Controller script that replicates the movement benchmark
-- behaviour (bench-movement) for an NPC entity.
--
-- The NPC mirrors what every bench client does in its steady-state loop:
--   * Walk in one direction for RUN_LEN_MIN..RUN_LEN_MAX steps.
--   * Then either:
--       - Pick a new random direction immediately (50 %)  OR
--       - Pause for PAUSE_MIN_MS..PAUSE_MAX_MS milliseconds (50 %).
--   * On a blocked step (step() returns nil) treat it like MoveReject:
--       change direction immediately and reset the step series.
--
-- Usage (attach to an existing entity):
--   demo-server --log world.uolog \
--     --lua-controller 0x00000001:scripts/movement_ctrl.lua
--
-- Configuration constants below can be overridden before dofile() via
-- the global table MOVEMENT_CFG:
--   MOVEMENT_CFG = {
--       run_len_min   = 3,
--       run_len_max   = 20,
--       pause_min_ms  = 2000,
--       pause_max_ms  = 5000,
--       step_delay_ms = 100,   -- matches --move-interval 100
--       run_flag      = true,  -- OR 128 bit on direction (running)
--   }
--
-- ======================= Controller API used =======================
--
--   World()              -> world object (controller mode, no map_id)
--   w:serial()           -> number
--   w:get_entity(serial) -> table | nil  { serial, x, y, z, ... }
--   w:step(direction)    -> table | nil  { x, y, z }  nil = blocked
--
-- Global:
--   sleep(ms)            Yield for ms milliseconds.
--   log(msg)             Print to server log.
--   poll_event()         Non-blocking controller event.
--   wait_event(ms)       Yield until event or timeout.
--
-- =================================================================

-- ── Configuration ────────────────────────────────────────────────────────

local cfg = MOVEMENT_CFG or {}

-- Direction 0-7 (N=0,NE=1,E=2,SE=3,S=4,SW=5,W=6,NW=7).
-- OR 128 to the direction byte to signal running (same as real UO client).
local RUN_FLAG      = cfg.run_flag     ~= false and 128 or 0  -- running by default

-- Number of steps per "series" before direction change / pause decision.
local RUN_LEN_MIN   = cfg.run_len_min  or 3
local RUN_LEN_MAX   = cfg.run_len_max  or 20

-- Pause duration range in milliseconds (only used on the 50 % pause branch).
local PAUSE_MIN_MS  = cfg.pause_min_ms or 2000
local PAUSE_MAX_MS  = cfg.pause_max_ms or 5000

-- Delay between individual move attempts (mirrors --move-interval).
local STEP_DELAY_MS = cfg.step_delay_ms or 100

-- ── Helpers ──────────────────────────────────────────────────────────────

local function rand_direction()
    -- Returns a random direction 0-7 with the run flag applied.
    return math.random(0, 7) + RUN_FLAG
end

local function rand_run_length()
    -- Guard against swapped min/max.
    local lo = math.min(RUN_LEN_MIN, RUN_LEN_MAX)
    local hi = math.max(RUN_LEN_MIN, RUN_LEN_MAX)
    return math.random(lo, hi)
end

local function rand_pause_ms()
    local lo = math.min(PAUSE_MIN_MS, PAUSE_MAX_MS)
    local hi = math.max(PAUSE_MIN_MS, PAUSE_MAX_MS)
    -- math.random requires integers
    return math.random(lo, hi)
end

-- ── Initialise ───────────────────────────────────────────────────────────

local w      = World()
local SERIAL = w:serial()

-- log(string.format("[movement_ctrl] controller started for entity 0x%08X", SERIAL))
-- log(string.format("[movement_ctrl] config: run_len=%d..%d  pause=%d..%dms  step_delay=%dms  run_flag=%d",
--    RUN_LEN_MIN, RUN_LEN_MAX, PAUSE_MIN_MS, PAUSE_MAX_MS, STEP_DELAY_MS, RUN_FLAG))

local me = w:get_entity(SERIAL)
if me then
    -- log(string.format("[movement_ctrl]   at (%d,%d,%d) graphic=0x%04X", me.x, me.y, me.z, me.graphic))
else
    log("[movement_ctrl]   entity not found in world yet — will continue anyway")
end

-- ── State ────────────────────────────────────────────────────────────────

local direction      = rand_direction()
local steps_remaining = rand_run_length()
local pausing        = false   -- true while we are in a pause interval
local pause_left_ms  = 0       -- remaining pause time (decremented by STEP_DELAY_MS)

-- ── Main loop ────────────────────────────────────────────────────────────
--
-- We drive the loop at STEP_DELAY_MS intervals (like --move-interval on the
-- benchmark client).  Each iteration:
--   1. Drain controller events (moved, timer_fired, …).
--   2. If pausing: tick down the pause counter and skip movement.
--   3. Otherwise: attempt a step.
--      * Success → decrement steps_remaining.
--        When steps_remaining reaches 0 → series end decision.
--      * Blocked  → treat like MoveReject: new direction, new series.

while true do
    -- 1. Drain pending controller events (non-blocking).
    local ev = poll_event()
    while ev do
        if ev.type == "moved" then
            -- Acknowledge server-driven moves (should not happen for pure
            -- controller-driven NPCs, but handle gracefully).
        elseif ev.type == "timer_fired" then
            -- No timers registered; ignore.
        end
        ev = poll_event()
    end

    -- 2. Pause phase.
    if pausing then
        pause_left_ms = pause_left_ms - STEP_DELAY_MS
        if pause_left_ms <= 0 then
            pausing    = false
            direction  = rand_direction()
            steps_remaining = rand_run_length()
            -- log(string.format("[movement_ctrl] 0x%08X  pause ended → dir=%d steps=%d", SERIAL, direction, steps_remaining))
        end
        sleep(STEP_DELAY_MS)
        goto continue
    end

    -- 3. Attempt a step.
    local result = w:step(direction)

    if result then
        -- Step succeeded.
        steps_remaining = steps_remaining - 1

        if steps_remaining <= 0 then
            -- End of series: 50 % chance to pause, 50 % new direction.
            if math.random(0, 1) == 0 then
                -- Pause branch.
                local pause_ms = rand_pause_ms()
                pausing       = true
                pause_left_ms = pause_ms
                -- log(string.format("[movement_ctrl] 0x%08X  series end → pausing %dms", SERIAL, pause_ms))
            else
                -- Immediate direction change.
                direction = rand_direction()
                steps_remaining = rand_run_length()
                -- log(string.format("[movement_ctrl] 0x%08X  series end → new dir=%d steps=%d", SERIAL, direction, steps_remaining))
            end
        end
    else
        -- Step blocked (MoveReject equivalent): pick a new direction and
        -- reset the series immediately.
        direction = rand_direction()
        steps_remaining = rand_run_length()
        -- log(string.format("[movement_ctrl] 0x%08X  blocked → new dir=%d steps=%d", SERIAL, direction, steps_remaining))
    end

    sleep(STEP_DELAY_MS)

    ::continue::
end
