-- scene/scene.lua: Scene class — the orchestrator.
--
-- Depends on: scene/helpers.lua, scene/actor.lua, scene/group.lua, scene/props.lua
-- Loaded automatically by scene.lua.

-- ═══════════════════════════════════════════════════════════════════════
-- Scene class
-- ═══════════════════════════════════════════════════════════════════════

Scene = {}
Scene.__index = Scene

--- Create a new Scene bound to a World context.
---
--- Usage:
---   local scene = Scene(w)                        -- no anchor
---   local scene = Scene(w, { anchor = {x, y} })   -- static anchor
---   local scene = Scene(w, { anchor = {x, y}, z = 5 })
---
function Scene.new(w, opts)
    local self = setmetatable({}, Scene)
    self._w = w
    self._actors = {}      -- all actors created/adopted by this scene
    self._groups = {}      -- named groups
    self._spawned = {}     -- serials of actors spawned by this scene (for cleanup)
    self._initial_light = nil  -- saved for cleanup

    -- Anchor point for relative coordinates.
    self._anchor_x = 0
    self._anchor_y = 0
    self._anchor_z = 0
    self._has_anchor = false

    -- Attached entity: scene follows this entity's position.
    self._attached_serial = nil
    self._attached_offset_x = 0   -- anchor offset from attached entity
    self._attached_offset_y = 0

    if opts and opts.anchor then
        self:set_anchor(opts.anchor[1], opts.anchor[2], opts.anchor.z or opts.anchor[3])
    end

    -- Register for automatic cleanup when the script finishes or errors.
    -- This is a graceful best-effort mechanism (Phase 2).  The Rust-side
    -- serial-range cleanup (Phase 1) is the authoritative safety net.
    if register_cleanup then
        register_cleanup(function()
            self:cleanup()
        end)
    end

    return self
end

-- Allow calling Scene(w) as constructor.
setmetatable(Scene, { __call = function(cls, ...) return cls.new(...) end })

-- ── Anchor / coordinate system ──────────────────────────────────────────

--- Set a static anchor point. All relative coordinates are offsets from this.
--- If a z is provided, it becomes the default z for spawn_actor/spawn_prop.
function Scene:set_anchor(x, y, z)
    self._anchor_x = x or 0
    self._anchor_y = y or 0
    self._anchor_z = z or 0
    self._has_anchor = true
    return self
end

--- Return the current anchor position (x, y, z).
function Scene:anchor()
    if self._attached_serial then
        self:_refresh_anchor()
    end
    return self._anchor_x, self._anchor_y, self._anchor_z
end

--- Convert relative coordinates to absolute.
--- Accepts: (rx, ry [, rz])  or  ({rx, ry [, rz]})
--- If no anchor is set, returns coordinates unchanged.
function Scene:abs(rx_or_tbl, ry, rz)
    local rx, ry_v, rz_v
    if type(rx_or_tbl) == "table" then
        rx = rx_or_tbl[1] or rx_or_tbl.x or 0
        ry_v = rx_or_tbl[2] or rx_or_tbl.y or 0
        rz_v = rx_or_tbl[3] or rx_or_tbl.z
    else
        rx = rx_or_tbl or 0
        ry_v = ry or 0
        rz_v = rz
    end
    if self._attached_serial then
        self:_refresh_anchor()
    end
    return self._anchor_x + rx,
           self._anchor_y + ry_v,
           (rz_v or 0) + self._anchor_z
end

--- Internal: resolve x, y, z from a params table.
--- Supports both absolute (x=, y=, z=) and relative (at={rx, ry, rz}).
--- If `at` is present and scene has an anchor, coordinates are relative.
--- Returns absolute x, y, z.
function Scene:_resolve_pos(params)
    if params.at then
        local at = params.at
        local rx = at[1] or at.x or 0
        local ry = at[2] or at.y or 0
        local rz = at[3] or at.z or 0
        return self:abs(rx, ry, rz)
    end
    -- Absolute coordinates — use as-is.
    return params.x or 0, params.y or 0, params.z or 0
end

-- ── Entity attachment (scene follows an entity) ─────────────────────────

--- Attach the scene to a world entity (e.g. a player).
--- The entity is teleported to `dest` (absolute coordinates), and the
--- scene's anchor is set to `dest` (or the entity's current position
--- if `dest` is nil).  All subsequent relative coordinates resolve
--- against this anchor.
---
--- Usage:
---   scene:attach(player_serial, { x = 1438, y = 1696, z = 0 })
---   scene:attach(player_serial)  -- anchor at entity's current pos
---
--- `opts.offset` — optional {dx, dy} offset: anchor = entity + offset.
--- This lets you place the "stage center" a few tiles away from where
--- the entity stands.
---
---   scene:attach(serial, { x=1438, y=1696, z=0, offset={2, 3} })
---
function Scene:attach(serial, opts)
    opts = opts or {}
    self._attached_serial = serial

    -- Teleport the entity to the destination if provided.
    if opts.x and opts.y then
        self._w:teleport(serial, opts.x, opts.y, opts.z or 0)
        sleep(200)  -- small delay for the client to process the teleport
    end

    -- Read entity's actual position.
    local e = self._w:get_entity(serial)
    if not e then
        log("scene:attach — entity 0x" .. string.format("%08X", serial) .. " not found")
        return self
    end

    local off_x = opts.offset and opts.offset[1] or 0
    local off_y = opts.offset and opts.offset[2] or 0
    self._attached_offset_x = off_x
    self._attached_offset_y = off_y

    self._anchor_x = e.x + off_x
    self._anchor_y = e.y + off_y
    self._anchor_z = opts.z or e.z or 0
    self._has_anchor = true
    return self
end

--- Detach the scene from the entity.
--- The anchor remains at its last-known position (becomes static).
function Scene:detach()
    if self._attached_serial then
        self:_refresh_anchor()  -- freeze the anchor at current pos
    end
    self._attached_serial = nil
    return self
end

--- Internal: refresh anchor from the attached entity's current position.
function Scene:_refresh_anchor()
    if not self._attached_serial then return end
    local e = self._w:get_entity(self._attached_serial)
    if e then
        self._anchor_x = e.x + self._attached_offset_x
        self._anchor_y = e.y + self._attached_offset_y
        self._anchor_z = e.z
    end
end

-- ── Actor management ────────────────────────────────────────────────────

--- Spawn a new NPC actor.
---
--- Full form:
---   scene:spawn_actor({
---       graphic = 0x0190, color = 0x0481, name = "Guard",
---       at = {0, 0, 0}, direction = "s",
---       outfit = guard_outfit,
---   })
---
--- Compact form with body preset:
---   scene:spawn_actor({ body = BODY.MALE_TAN, name = "Guard",
---       at = {0, 0, 0}, direction = "s", outfit = guard_outfit })
---
---   scene:spawn_actor({ body = BODY.SKELETON, at = {5, 0, 0} })
---   scene:spawn_actor({ body = BODY.DRAGON_RED, at = {10, 0, 0} })
---
--- The `body` preset provides defaults for `graphic`, `color`, and `name`.
--- Explicit params override any body-preset values.
---
--- Relative coordinates:
---   at = {rx, ry, rz}  — position relative to scene anchor.
---   If `at` is provided, it takes precedence over x/y/z.
---
--- Direction can be a string: "n","ne","e","se","s","sw","w","nw".
---
--- If `outfit` is an Outfit object, its items are used as equipment.
--- Returns an Actor object.
function Scene:spawn_actor(params)
    -- Apply body preset as defaults.
    if params.body then
        local b = params.body
        if not params.graphic then params.graphic = b.graphic end
        if not params.color and b.color then params.color = b.color end
        if not params.name and b.name then params.name = b.name end
        params.body = nil
    end

    -- Resolve position (at={} or x/y/z).
    local ax, ay, az = self:_resolve_pos(params)
    params.x = ax
    params.y = ay
    params.z = az

    -- Resolve direction string.
    if type(params.direction) == "string" then
        params.direction = resolve_dir(params.direction)
    end

    -- Convert outfit to items list if provided.
    if params.outfit then
        params.items = params.outfit:items()
    end
    local serial = self._w:spawn_npc(params)
    local actor = Actor.new(self, serial, true)
    table.insert(self._actors, actor)
    table.insert(self._spawned, serial)
    return actor
end

--- Spawn a world item (torch, decoration, sign, etc.).
--- params: { graphic, x, y, z, [color], [amount] }
--- Supports at={rx,ry,rz} for relative coordinates.
--- Returns the item's serial. The item is removed on scene:cleanup().
function Scene:spawn_item(params)
    local ax, ay, az = self:_resolve_pos(params)
    params.x = ax
    params.y = ay
    params.z = az
    local serial = self._w:spawn_item(params)
    table.insert(self._spawned, serial)
    return serial
end

--- Remove a previously spawned item by serial.
function Scene:remove_item(serial)
    self._w:remove_entity(serial)
end

--- Spawn a Prop (stage decoration / world item) with a convenient API.
---
--- Usage:
---   -- From a preset with absolute coordinates:
---   local torch = scene:spawn_prop(PROPS.TORCH, x, y, z)
---   local torch = scene:spawn_prop(PROPS.TORCH, x, y, z, color)
---
---   -- From a preset with relative coordinates (anchor-based):
---   local torch = scene:spawn_prop(PROPS.TORCH, {rx, ry, rz})
---   local torch = scene:spawn_prop(PROPS.TORCH, {rx, ry, rz}, color)
---
---   -- Inline graphic number:
---   local thing = scene:spawn_prop(0x0A12, x, y, z)
---   local thing = scene:spawn_prop(0x0A12, {rx, ry, rz})
---
---   -- Multi-tile preset (parts field):
---   local penta = scene:spawn_prop(PROPS.PENTAGRAM, {6, 1, 0})
---   penta:remove()  -- removes all 9 tiles
---
--- Returns a Prop object. The prop is removed on scene:cleanup().
function Scene:spawn_prop(preset_or_graphic, x_or_at, y_or_color, z, color)
    local params
    if type(preset_or_graphic) == "number" then
        params = { graphic = preset_or_graphic }
    elseif type(preset_or_graphic) == "table" then
        -- Clone the preset so we don't mutate it.
        params = {}
        for k, v in pairs(preset_or_graphic) do
            params[k] = v
        end
    else
        params = {}
    end

    -- Detect relative coordinates: spawn_prop(preset, {rx, ry, rz} [, color])
    if type(x_or_at) == "table" then
        params.at = x_or_at
        if y_or_color then params.color = y_or_color end
    else
        -- Absolute: spawn_prop(preset, x, y, z [, color])
        params.x = x_or_at or params.x or 0
        params.y = y_or_color or params.y or 0
        params.z = z or params.z or 0
        if color then params.color = color end
    end

    local ax, ay, az = self:_resolve_pos(params)

    -- ── Multi-tile prop (preset has `parts`) ────────────────────────
    if params.parts then
        local serials = {}
        local base_color = params.color or 0
        for _, part in ipairs(params.parts) do
            local serial = self._w:spawn_item({
                graphic = part.graphic,
                x       = ax + (part.dx or 0),
                y       = ay + (part.dy or 0),
                z       = az + (part.dz or 0),
                color   = part.color or base_color,
                amount  = part.amount or 0,
            })
            serials[#serials + 1] = serial
            table.insert(self._spawned, serial)
        end
        return Prop.new_multi(self, serials)
    end

    -- ── Single-tile prop (as before) ────────────────────────────────
    params.x = ax
    params.y = ay
    params.z = az

    local serial = self._w:spawn_item(params)
    table.insert(self._spawned, serial)

    local prop = Prop.new(self, serial)
    return prop
end

--- Adopt an existing entity as an Actor.
--- Returns an Actor object (will NOT be removed on cleanup).
function Scene:actor(serial)
    local actor = Actor.new(self, serial, false)
    table.insert(self._actors, actor)
    return actor
end

--- Create a named group of actors.
function Scene:group(name, actors)
    local grp = ActorGroup.new(self, name, actors)
    self._groups[name] = grp
    return grp
end

--- Get a previously created group by name.
function Scene:get_group(name)
    return self._groups[name]
end

-- ── Spawn patterns (batch actor placement) ──────────────────────────────

--- Spawn a line of actors between two points.
---
--- Usage:
---   local guards = scene:spawn_line({
---       count = 5,
---       template = { graphic = 0x0190, outfit = guard_outfit },
---       from = {-8, 0, 0},   -- relative to anchor
---       to   = {-8, 4, 0},
---       names = { "Alric", "Beren", "Cedric", "Drake", "Edwin" },
---       face = "e",
---   })
---
--- Returns an ActorGroup.
function Scene:spawn_line(params)
    local count = params.count or 1
    local tmpl = params.template or {}
    local names = params.names or {}

    -- Resolve endpoints.
    local fx, fy, fz, tx, ty, tz
    local from_p = params.from or {0, 0, 0}
    local to_p = params.to or from_p

    if from_p.x then
        fx, fy, fz = from_p.x, from_p.y, from_p.z or 0
    else
        fx, fy, fz = self:abs(from_p[1], from_p[2], from_p[3])
    end
    if to_p.x then
        tx, ty, tz = to_p.x, to_p.y, to_p.z or 0
    else
        tx, ty, tz = self:abs(to_p[1], to_p[2], to_p[3])
    end

    local actors = {}
    for i = 1, count do
        local t = (count > 1) and ((i - 1) / (count - 1)) or 0
        local x = math.floor(fx + (tx - fx) * t + 0.5)
        local y = math.floor(fy + (ty - fy) * t + 0.5)
        local z = math.floor(fz + (tz - fz) * t + 0.5)

        -- Build spawn params from template.
        local sp = {}
        for k, v in pairs(tmpl) do sp[k] = v end
        sp.x = x
        sp.y = y
        sp.z = z
        sp.name = names[i] or sp.name or ("NPC " .. i)
        if params.face then
            sp.direction = resolve_dir(params.face)
        end

        local actor = self:spawn_actor(sp)
        table.insert(actors, actor)
    end

    local group_name = params.group or ("line_" .. tostring(#self._groups + 1))
    return self:group(group_name, actors)
end

--- Spawn actors in a circle (or arc) around a center point.
---
--- Usage:
---   local cultists = scene:spawn_circle({
---       count = 6,
---       template = { graphic = 0x0190, outfit = mage_outfit },
---       center = {0, 0, 0},       -- relative to anchor
---       radius = 5,
---       face_center = true,        -- all face the center
---       arc = { from = 0, to = 360 },  -- optional: partial arc (degrees)
---   })
---
--- Returns an ActorGroup.
function Scene:spawn_circle(params)
    local count = params.count or 1
    local tmpl = params.template or {}
    local names = params.names or {}
    local radius = params.radius or 3

    -- Resolve center.
    local cx, cy, cz
    local center = params.center or {0, 0, 0}
    if center.x then
        cx, cy, cz = center.x, center.y, center.z or 0
    else
        cx, cy, cz = self:abs(center[1], center[2], center[3])
    end

    -- Arc range in degrees (default full circle).
    local arc_from = 0
    local arc_to = 360
    if params.arc then
        arc_from = params.arc.from or 0
        arc_to = params.arc.to or 360
    end

    -- For a full circle we don't want to double up the last point
    -- (0 and 360 are the same), so spread evenly.
    local arc_span = arc_to - arc_from
    local angle_step = (count > 1)
        and (arc_span / (arc_span >= 360 and count or (count - 1)))
        or 0

    local actors = {}
    for i = 1, count do
        local angle_deg = arc_from + (i - 1) * angle_step
        local angle_rad = math.rad(angle_deg)
        -- UO coordinate system: +x = east, +y = south.
        -- Angle 0 = north (negative y).
        local dx = math.floor(radius * math.sin(angle_rad) + 0.5)
        local dy = math.floor(-radius * math.cos(angle_rad) + 0.5)
        local x = cx + dx
        local y = cy + dy

        local sp = {}
        for k, v in pairs(tmpl) do sp[k] = v end
        sp.x = x
        sp.y = y
        sp.z = cz
        sp.name = names[i] or sp.name or ("NPC " .. i)

        -- Face toward center.
        if params.face_center then
            local d = direction_to(x, y, cx, cy)
            if d then sp.direction = d end
        elseif params.face then
            sp.direction = resolve_dir(params.face)
        end

        local actor = self:spawn_actor(sp)
        table.insert(actors, actor)
    end

    local group_name = params.group or ("circle_" .. tostring(#self._groups + 1))
    return self:group(group_name, actors)
end

--- Spawn actors at explicit positions.
---
--- Usage:
---   local squad = scene:spawn_group({
---       template = { graphic = 0x0190, outfit = guard_outfit },
---       positions = { {0, 0}, {1, 0}, {0, 1}, {1, 1} },
---       names = { "A", "B", "C", "D" },
---       face = "s",
---   })
---
--- Each position is {rx, ry [, rz]} relative to anchor (if set),
--- or {x=, y=, z=} for absolute.
--- Returns an ActorGroup.
function Scene:spawn_group(params)
    local tmpl = params.template or {}
    local names = params.names or {}
    local positions = params.positions or {}

    local actors = {}
    for i, pos in ipairs(positions) do
        local x, y, z
        if pos.x then
            x, y, z = pos.x, pos.y, pos.z or 0
        else
            x, y, z = self:abs(pos[1], pos[2], pos[3])
        end

        local sp = {}
        for k, v in pairs(tmpl) do sp[k] = v end
        sp.x = x
        sp.y = y
        sp.z = z
        sp.name = names[i] or sp.name or ("NPC " .. i)
        if params.face then
            sp.direction = resolve_dir(params.face)
        end

        local actor = self:spawn_actor(sp)
        table.insert(actors, actor)
    end

    local group_name = params.group or ("group_" .. tostring(#self._groups + 1))
    return self:group(group_name, actors)
end

-- ── Timing ──────────────────────────────────────────────────────────────

--- Pause the scene for `ms` milliseconds.
function Scene:wait(ms)
    sleep(ms)
end

--- Wait until two actors are within `range` tiles of each other.
--- opts.timeout = max ms to wait (default 30000).
--- opts.poll = polling interval ms (default 200).
function Scene:wait_until_near(actor1, actor2, range, opts)
    opts = opts or {}
    local timeout = opts.timeout or 30000
    local poll = opts.poll or 200
    local elapsed = 0

    while elapsed < timeout do
        local x1, y1 = actor1:pos()
        local x2, y2 = actor2:pos()
        if distance(x1, y1, x2, y2) <= range then
            return true
        end
        sleep(poll)
        elapsed = elapsed + poll
    end
    return false  -- timed out
end

--- Wait until an actor reaches a specific tile.
--- opts.timeout, opts.poll — same as wait_until_near.
function Scene:wait_until_at(actor, x, y, opts)
    opts = opts or {}
    local timeout = opts.timeout or 30000
    local poll = opts.poll or 200
    local elapsed = 0

    while elapsed < timeout do
        local ax, ay = actor:pos()
        if ax == x and ay == y then return true end
        sleep(poll)
        elapsed = elapsed + poll
    end
    return false
end

-- ── Effects & Sounds ────────────────────────────────────────────────────

--- Play a sound at an actor's position, or at explicit coordinates.
--- source: Actor object OR table { x=, y=, z= }
function Scene:sound(sound_id, source)
    local x, y, z
    if type(source) == "table" and source.pos then
        -- Actor object
        x, y, z = source:pos()
    elseif type(source) == "table" then
        x = source.x or 0
        y = source.y or 0
        z = source.z or 0
    end
    self._w:play_sound(sound_id, x, y, z)
end

--- Play a stationary effect at an actor's position.
function Scene:effect_at(actor_or_pos, graphic, opts)
    opts = opts or {}
    local x, y, z
    if type(actor_or_pos) == "table" and actor_or_pos.pos then
        x, y, z = actor_or_pos:pos()
    else
        x = actor_or_pos.x or 0
        y = actor_or_pos.y or 0
        z = actor_or_pos.z or 0
    end
    self._w:effect({
        direction_type = 2,
        graphic = graphic,
        x = x, y = y, z = z,
        speed = opts.speed or 10,
        duration = opts.duration or 30,
        fixed_direction = opts.fixed_direction,
    })
end

--- Play a projectile effect from source to target.
function Scene:effect_between(source, target, graphic, opts)
    opts = opts or {}
    local sx, sy, sz
    local tx, ty, tz

    if type(source) == "table" and source.pos then
        sx, sy, sz = source:pos()
    else
        sx, sy, sz = source.x or 0, source.y or 0, source.z or 0
    end

    if type(target) == "table" and target.pos then
        tx, ty, tz = target:pos()
    else
        tx, ty, tz = target.x or 0, target.y or 0, target.z or 0
    end

    self._w:effect({
        direction_type = 0,  -- projectile
        source_serial = (type(source) == "table" and source.serial and source:serial()) or 0,
        target_serial = (type(target) == "table" and target.serial and target:serial()) or 0,
        graphic = graphic,
        x = sx, y = sy, z = sz,
        target_x = tx, target_y = ty, target_z = tz,
        speed = opts.speed or 5,
        duration = opts.duration or 15,
        fixed_direction = opts.fixed_direction or true,
        explode = opts.explode or false,
    })
end

--- Preset effects on an actor.
function Scene:lightning(actor)
    actor:lightning()
end

function Scene:flamestrike(actor)
    actor:flamestrike()
end

function Scene:heal(actor)
    actor:heal()
end

-- ── Ambience ────────────────────────────────────────────────────────────

--- Set global light level instantly.
--- level: 0x00 = full day, 0x1F = pitch black.
function Scene:set_light(level)
    self._w:set_light(level)
end

--- Smoothly transition light level from `from` to `to` over `duration_ms`.
--- Steps are sent every `step_ms` (default 100ms).
function Scene:fade_light(from, to, duration_ms, step_ms)
    step_ms = step_ms or 100
    local steps = math.max(1, math.floor(duration_ms / step_ms))
    for i = 0, steps do
        local t = i / steps
        local level = math.floor(from + (to - from) * t + 0.5)
        self._w:set_light(level)
        if i < steps then sleep(step_ms) end
    end
end

--- Set weather.
--- weather_type: 0=rain, 1=storm, 2=snow, 0xFF=none (or string names).
--- num_effects: particle count (default 0x40).
--- temperature: (default 0x10).
function Scene:set_weather(weather_type, num_effects, temperature)
    local wt = weather_type
    if type(wt) == "string" then
        local names = { rain=0, storm=1, snow=2, none=0xFF }
        wt = names[wt:lower()] or 0xFF
    end
    self._w:set_weather(wt, num_effects, temperature)
end

--- Clear weather.
function Scene:clear_weather()
    self._w:set_weather(0xFF, 0, 0)
end

--- Set season.
--- season: 0-4 or string ("spring", "summer", "fall", "winter", "desolation").
--- play_sound: whether to play transition sound (default true).
function Scene:set_season(season, play_sound)
    local s = season
    if type(s) == "string" then
        local names = { spring=0, summer=1, fall=2, winter=3, desolation=4 }
        s = names[s:lower()] or 0
    end
    if play_sound == nil then play_sound = true end
    self._w:set_season(s, play_sound)
end

--- Play background music track.
function Scene:play_music(music_id)
    self._w:play_music(music_id)
end

--- Stop music (silence).
function Scene:stop_music()
    self._w:play_music(MUSIC.STOP)
end

-- ── Parallel execution ──────────────────────────────────────────────────

--- Run multiple functions simultaneously using round-robin scheduling.
---
--- Each function runs one "step" at a time: between each sleep() call
--- the scheduler switches to the next function.  The actual delay is
--- the maximum sleep requested by any function in that round.
---
--- Usage:
---   scene:parallel(
---       function() guard:walk_to({5, 5}) end,
---       function() merchant:walk_to({10, 5}) end
---   )
---
--- Implementation note: Lua coroutines + mlua async methods don't mix
--- (async methods yield internal userdata, not our sleep sentinel).
--- Instead we run each function sequentially in small increments by
--- hooking sleep() to record the requested delay and return immediately.
function Scene:parallel(...)
    local fns = { ... }
    if #fns == 0 then return end
    if #fns == 1 then fns[1](); return end

    local original_sleep = sleep

    -- Sentinel to distinguish our yields from mlua internal ones.
    local SLEEP_TAG = {}

    -- Per-task state
    local tasks = {}
    for i, fn in ipairs(fns) do
        tasks[i] = {
            co = coroutine.create(fn),
            delay = 0,
            done = false,
        }
    end

    local active = #tasks

    while active > 0 do
        local min_delay = math.huge

        for i = 1, #tasks do
            local task = tasks[i]
            if not task.done and task.delay <= 0 then
                -- Run this task until it yields (sleep) or finishes.
                -- Replace sleep with a yielding version.
                local saved = sleep
                sleep = function(ms)
                    coroutine.yield(SLEEP_TAG, ms)
                end

                -- Resume the coroutine.  It may yield with:
                --   SLEEP_TAG, ms  — our sleep
                --   (other)        — mlua internal yield, must re-yield
                local function step(co, ...)
                    if coroutine.status(co) == "dead" then
                        return true -- finished
                    end
                    local results = { coroutine.resume(co, ...) }
                    local ok = results[1]
                    if not ok then
                        -- Error
                        log("parallel task " .. i .. " error: " .. tostring(results[2]))
                        return true -- treat as finished
                    end
                    if coroutine.status(co) == "dead" then
                        return true -- finished normally
                    end
                    -- Check if this is our sleep yield
                    if results[2] == SLEEP_TAG then
                        task.delay = results[3] or 0
                        return false -- paused on sleep
                    end
                    -- mlua internal yield — pass through.
                    return step(co, coroutine.yield(table.unpack(results, 2)))
                end

                local is_done = step(task.co)
                sleep = saved

                if is_done then
                    task.done = true
                    active = active - 1
                end
            end

            if not task.done and task.delay > 0 and task.delay < min_delay then
                min_delay = task.delay
            end
        end

        if active > 0 and min_delay > 0 and min_delay < math.huge then
            -- Sleep for the shortest pending delay
            original_sleep(min_delay)
            -- Subtract elapsed time from all pending delays
            for i = 1, #tasks do
                if not tasks[i].done and tasks[i].delay > 0 then
                    tasks[i].delay = tasks[i].delay - min_delay
                end
            end
        elseif active > 0 then
            -- No delays pending but tasks still active — safety valve
            original_sleep(50)
        end
    end
end

-- ── Cleanup ─────────────────────────────────────────────────────────────

--- Remove all actors spawned by this scene and reset ambience.
function Scene:cleanup()
    for _, serial in ipairs(self._spawned) do
        self._w:remove_entity(serial)
    end
    self._spawned = {}
    self._actors = {}
    self._groups = {}
end
