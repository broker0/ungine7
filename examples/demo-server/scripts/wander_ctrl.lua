-- wander_ctrl.lua: Example controller-mode script.
--
-- This script runs as a coroutine inside the controller framework.
-- Unlike the async wander.lua, it has synchronous access to the game
-- world through ControlContext — no RPC, no channels.
--
-- Usage:
--   demo-server --log world.uolog \
--     --lua-controller 0x00000001:scripts/wander_ctrl.lua
--
-- ======================= Controller API =======================
--
-- World() -> world object
--   Creates a context bound to the controller's entity.
--   No map_id needed — it comes from the controller framework.
--
--   w:serial()                         -> number
--     The serial of the controlled entity.
--
--   w:map_id()                         -> number
--     The map ID of the current zone.
--
--   w:get_entity(serial)               -> table | nil
--     Returns: { serial, x, y, z, graphic, is_mobile, is_multi }
--
--   w:step(direction)                  -> table | nil
--     Move the controlled entity one tile.
--     direction: 0=N, 1=NE, 2=E, ... 7=NW
--     Returns { x, y, z } on success, nil if blocked.
--
--   w:teleport(x, y, z)               -> boolean
--     Teleport the controlled entity (no passability check).
--
--   w:query_area(x1, y1, x2, y2)      -> table of entity tables
--     All entities in the rectangle.
--
--   w:test_step(x, y, z, direction)    -> number | nil
--     Test if a step is passable. Returns new Z or nil.
--
--   w:resolve_z(x, y, z_hint, dir)    -> number | nil
--     Resolve standing height at a tile.
--
--   w:play_sound(sound_id, x, y, z)
--     Play a sound effect at coordinates.
--
--   w:effect(params_table)
--     Spawn a graphical effect (same params as async API).
--
--   w:animate(serial, action, frame_count [, opts])
--     Play character animation.
--
--   w:say(message [, opts])
--     Make the controlled entity speak.
--     opts: { speech_type, color, font, name }
--
-- Global functions:
--   sleep(ms)                  Yield the coroutine for ms milliseconds.
--   log(msg)                   Print to server log.
--   poll_event()        -> table | nil   Non-blocking controller event.
--   wait_event(ms)      -> table | nil   Yield until event or timeout.
--
-- Controller events (from poll_event / wait_event):
--   { type="moved",        direction, x, y, z }
--   { type="timer_fired",  timer_id }
--
-- Key differences from async mode:
--   - World() takes no map_id
--   - step/teleport operate on the controlled entity (no serial arg)
--   - say() doesn't need a serial — uses the controlled entity
--   - sleep() yields the coroutine instead of async-awaiting
--   - Events are controller events (not broadcast WorldEvents)
--
-- =============================================================

local w = World()
local SERIAL = w:serial()

log(string.format("controller started for entity 0x%08X", SERIAL))

-- Look up our entity.
local me = w:get_entity(SERIAL)
if me then
    log(string.format("  at (%d,%d,%d) graphic=0x%04X",
        me.x, me.y, me.z, me.graphic))
else
    log("  entity not found (will retry)")
end

while true do
    -- Pick a random direction (0=N, 1=NE, 2=E, ... 7=NW)
    local dir = math.random(0, 7)
    local result = w:step(dir)

    if result then
        log(string.format("stepped to (%d,%d,%d)", result.x, result.y, result.z))
    end

    -- Check for controller events (timer_fired, moved, etc.)
    local event = poll_event()
    while event do
        log(string.format("event: %s", event.type))
        event = poll_event()
    end

    sleep(3000)
end
