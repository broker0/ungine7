-- lib.lua: Reusable helper functions for demo-server Lua scripts.
--
-- Usage: dofile("scripts/lib.lua")
--
-- Note: this file is loaded *before* constants/, so it uses
-- numeric literals directly.  When the ANIM/SOUND/EFFECT constants are
-- available (i.e. after scene.lua loads), prefer using named constants
-- in your scripts instead of these numbers.

-- ── Geometry ──────────────────────────────────────────────────────────────

--- Chebyshev (king-move) distance between two tile positions.
function chebyshev(x1, y1, x2, y2)
    return math.max(math.abs(x1 - x2), math.abs(y1 - y2))
end

-- ── Timer-based scheduler ────────────────────────────────────────────────
--
-- Non-blocking task scheduler for controller scripts.  Instead of calling
-- sleep() (which blocks the entire event loop), schedule callbacks:
--
--   local sched = Scheduler.new()
--   sched:after(2.0, function() ... end)
--
--   while true do
--       local ev = wait_event(sched:next_timeout(60000))
--       sched:tick()
--       ...
--   end

Scheduler = {}
Scheduler.__index = Scheduler

function Scheduler.new()
    return setmetatable({ tasks = {} }, Scheduler)
end

--- Schedule `callback` to run after `delay_sec` seconds.
--- Returns a key that can be passed to :cancel().
function Scheduler:after(delay_sec, callback)
    local key = {}  -- unique table reference as key
    self.tasks[key] = { deadline = clock() + delay_sec, fn = callback }
    return key
end

--- Cancel a previously scheduled task.
function Scheduler:cancel(key)
    self.tasks[key] = nil
end

--- Execute all tasks whose deadline has passed.  Call once per loop iteration.
function Scheduler:tick()
    local now = clock()
    local expired = {}
    for key, task in pairs(self.tasks) do
        if now >= task.deadline then
            expired[#expired + 1] = { key = key, fn = task.fn }
        end
    end
    for _, e in ipairs(expired) do
        self.tasks[e.key] = nil
        e.fn()
    end
end

--- Shortest time (ms) until the next task fires, or `default_ms`.
--- Use as the timeout for wait_event().
function Scheduler:next_timeout(default_ms)
    local now = clock()
    local best = default_ms
    for _, task in pairs(self.tasks) do
        local remaining = (task.deadline - now) * 1000
        if remaining < best then best = math.max(remaining, 1) end
    end
    return best
end

-- ── Movement ──────────────────────────────────────────────────────────────

--- Walk `steps` tiles in `dir` (0-7) with delay between each step.
function walk(w, serial, dir, steps, delay)
    delay = delay or 200
    for i = 1, steps do
        w:step(serial, dir)
        sleep(delay)
    end
end

--- Run `steps` tiles in `dir` (0-7) with delay between each step.
function run(w, serial, dir, steps, delay)
    delay = delay or 100
    walk(w, serial, dir + 128, steps, delay)
end

-- ── Speech ────────────────────────────────────────────────────────────────

--- Say a message, optionally wait `pause` ms after.
function say(w, serial, msg, pause)
    w:say(serial, msg)
    if pause then sleep(pause) end
end

-- ── Spellcasting ──────────────────────────────────────────────────────────

--- Generic cast: animation + sound + effect on the caster.
--- `opts` can override direction_type, speed, duration, fixed_direction.
function cast(w, serial, sound, effect_graphic, opts)
    local me = w:get_entity(serial)
    if not me then return end
    w:animate(serial, 0x10, 7)  -- ANIM.CAST
    if sound then
        w:play_sound(sound, me.x, me.y, me.z)
    end
    if effect_graphic then
        w:effect({
            direction_type = opts and opts.direction_type or 3,
            source_serial = serial,
            graphic = effect_graphic,
            x = me.x, y = me.y, z = me.z,
            speed = opts and opts.speed or 10,
            duration = opts and opts.duration or 30,
            fixed_direction = opts and opts.fixed_direction or false,
        })
    end
end

--- Lightning bolt on target.
function lightning(w, serial)
    -- SOUND.LIGHTNING (0x29), lightning effect type
    cast(w, serial, 0x29, 0, { direction_type = 1, speed = 0, duration = 0 })
end

--- Flame strike on target.
function flamestrike(w, serial)
    -- SOUND.FLAMESTRIKE (0x208), EFFECT.FLAMESTRIKE (0x3709)
    cast(w, serial, 0x208, 0x3709)
end

--- Heal effect on target.
function heal(w, serial)
    -- SOUND.HEAL (0x202), EFFECT.HEAL (0x375A)
    cast(w, serial, 0x202, 0x375A, { speed = 7, duration = 16 })
end

-- ── Teleport ──────────────────────────────────────────────────────────────

--- Teleport with visual effects at source and destination.
function teleport(w, serial, x, y, z)
    local me = w:get_entity(serial)
    if not me then return end
    -- source effect: ANIM.MOUNTED_ATTACK (0x1A), EFFECT.TELEPORT (0x3728)
    w:animate(serial, 0x1A, 20)
    w:effect({
        direction_type = 2, graphic = 0x3728,
        x = me.x, y = me.y, z = me.z,
        speed = 10, duration = 10,
    })
    w:teleport(serial, x, y, z)
    -- dest effect: SOUND.TELEPORT (0x1FE), EFFECT.TELEPORT (0x3728)
    w:play_sound(0x1FE, x, y, z)
    w:effect({
        direction_type = 2, graphic = 0x3728,
        x = x, y = y, z = z,
        speed = 10, duration = 10,
    })
end

-- ── Emotes / Animations ──────────────────────────────────────────────────

--- Play a bow animation.
function bow(w, serial)
    w:animate(serial, 0x20, 5)  -- ANIM.BOW
end

--- Play a salute animation.
function salute(w, serial)
    w:animate(serial, 0x21, 5)  -- ANIM.SALUTE
end
