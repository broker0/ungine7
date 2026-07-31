-- scene/actor.lua: Actor class — a mobile entity in a scene.
--
-- Depends on: scene/helpers.lua (resolve_dir, direction_to, find_step_direction)
-- Depends on: lib.lua (teleport, lightning, flamestrike, heal, cast)
-- Loaded automatically by scene.lua.

-- ═══════════════════════════════════════════════════════════════════════
-- Actor class
-- ═══════════════════════════════════════════════════════════════════════

Actor = {}
Actor.__index = Actor

--- Create an Actor wrapping an existing entity serial.
function Actor.new(scene, serial, spawned)
    local self = setmetatable({}, Actor)
    self._scene = scene
    self._w = scene._w
    self._serial = serial
    self._spawned = spawned or false  -- true if scene spawned this actor
    return self
end

--- Return the actor's serial.
function Actor:serial()
    return self._serial
end

--- Refresh and return the actor's current entity data.
function Actor:entity()
    return self._w:get_entity(self._serial)
end

--- Return the actor's current position as x, y, z.
function Actor:pos()
    local e = self:entity()
    if e then return e.x, e.y, e.z end
    return 0, 0, 0
end

-- ── Speech & Emotes ─────────────────────────────────────────────────────

--- Make the actor say a message. opts.pause = ms to wait after.
--- opts.color, opts.font, opts.speech_type — forwarded to w:say().
function Actor:say(msg, opts)
    opts = opts or {}
    self._w:say(self._serial, msg, {
        speech_type = opts.speech_type,
        color = opts.color,
        font = opts.font,
        name = opts.name,
    })
    if opts.pause then sleep(opts.pause) end
end

--- Emote (speech_type = 2).
function Actor:emote(msg, opts)
    opts = opts or {}
    opts.speech_type = 2
    self:say(msg, opts)
end

--- Play a bow animation.
function Actor:bow()
    self._w:animate(self._serial, ANIM.BOW, 5)
end

--- Play a salute animation.
function Actor:salute()
    self._w:animate(self._serial, ANIM.SALUTE, 5)
end

--- Play a custom animation.
function Actor:animate(action, frame_count, opts)
    self._w:animate(self._serial, action, frame_count or 7, opts)
end

-- ── Movement ────────────────────────────────────────────────────────────

--- Walk one step in a direction.
function Actor:step(dir)
    return self._w:step(self._serial, resolve_dir(dir))
end

--- Run one step (direction + 0x80).
function Actor:run_step(dir)
    return self._w:step(self._serial, resolve_dir(dir) + 128)
end

--- Turn to face a direction without moving.
function Actor:face(dir)
    self._w:step(self._serial, resolve_dir(dir))
end

--- Turn to face a point.
--- Accepts absolute: face_towards(x, y)
--- Or relative:      face_towards({rx, ry})
function Actor:face_towards(tx_or_rel, ty)
    local tx, ty_v
    if type(tx_or_rel) == "table" then
        tx, ty_v = self._scene:abs(tx_or_rel[1], tx_or_rel[2])
    else
        tx = tx_or_rel
        ty_v = ty
    end
    local x, y, _ = self:pos()
    local d = direction_to(x, y, tx, ty_v)
    if d then self:face(d) end
end

--- Walk to a target point. Uses simple pathfinding to avoid obstacles.
--- Accepts absolute coordinates: walk_to(x, y, opts)
--- Or relative coordinates:      walk_to({rx, ry}, opts)
--- opts.speed = ms per step (default 200).
--- opts.max_steps = safety limit (default 200).
--- Returns true if reached, false if stuck/gave up.
function Actor:walk_to(tx_or_rel, ty_or_opts, opts)
    local tx, ty
    if type(tx_or_rel) == "table" then
        -- Relative: walk_to({rx, ry}, opts)
        tx, ty = self._scene:abs(tx_or_rel[1], tx_or_rel[2])
        opts = ty_or_opts
    else
        tx = tx_or_rel
        ty = ty_or_opts
    end
    opts = opts or {}
    local speed = opts.speed or 200
    local max_steps = opts.max_steps or 200

    for i = 1, max_steps do
        local x, y, z = self:pos()
        if x == tx and y == ty then return true end

        local dir = find_step_direction(self._w, x, y, z, tx, ty)
        if not dir then return false end  -- stuck

        self._w:step(self._serial, dir)
        sleep(speed)
    end
    return false  -- max steps exceeded
end

--- Run to a target point.
--- Accepts absolute coordinates: run_to(x, y, opts)
--- Or relative coordinates:      run_to({rx, ry}, opts)
--- opts.speed = ms per step (default 100).
function Actor:run_to(tx_or_rel, ty_or_opts, opts)
    local tx, ty
    if type(tx_or_rel) == "table" then
        tx, ty = self._scene:abs(tx_or_rel[1], tx_or_rel[2])
        opts = ty_or_opts
    else
        tx = tx_or_rel
        ty = ty_or_opts
    end
    opts = opts or {}
    local speed = opts.speed or 100
    local max_steps = opts.max_steps or 200

    for i = 1, max_steps do
        local x, y, z = self:pos()
        if x == tx and y == ty then return true end

        local dir = find_step_direction(self._w, x, y, z, tx, ty)
        if not dir then return false end

        self._w:step(self._serial, dir + 128)
        sleep(speed)
    end
    return false
end

--- Walk a route: list of waypoints.
--- Each waypoint: { x=, y=, [on_arrive=function(actor)] }
---   or relative: { rx, ry, [on_arrive=function(actor)] }
---   or shorthand: { x, y, [on_arrive=] }
--- When using relative, coordinates are resolved against the scene anchor.
--- opts.speed = ms per step (default 200).
--- opts.run = use running animation (default false).
function Actor:walk_route(waypoints, opts)
    opts = opts or {}
    local speed = opts.speed or 200
    local is_run = opts.run or false

    for _, wp in ipairs(waypoints) do
        local target_x, target_y

        if wp.x and wp.y then
            -- Explicit absolute: { x = 100, y = 200 }
            target_x = wp.x
            target_y = wp.y
        elseif wp.at then
            -- Explicit relative: { at = {rx, ry} }
            target_x, target_y = self._scene:abs(wp.at[1], wp.at[2])
        elseif self._scene._has_anchor and wp[1] and wp[2] then
            -- Shorthand: { rx, ry } — relative when anchor is set
            target_x, target_y = self._scene:abs(wp[1], wp[2])
        else
            -- Shorthand fallback (no anchor): { x, y } — absolute
            target_x = wp[1]
            target_y = wp[2]
        end

        if is_run then
            self:run_to(target_x, target_y, { speed = speed })
        else
            self:walk_to(target_x, target_y, { speed = speed })
        end

        -- Execute on_arrive callback at this waypoint
        if wp.on_arrive then
            wp.on_arrive(self)
        end
    end
end

--- Teleport the actor with visual effects (source and destination).
--- Accepts absolute: teleport(x, y, z)
--- Or relative:      teleport({rx, ry, rz})
function Actor:teleport(x_or_rel, y, z)
    local ax, ay, az
    if type(x_or_rel) == "table" then
        ax, ay, az = self._scene:abs(x_or_rel[1], x_or_rel[2], x_or_rel[3])
    else
        ax, ay, az = x_or_rel, y, z
    end
    teleport(self._w, self._serial, ax, ay, az)
end

--- Teleport without visual effects.
--- Accepts absolute: teleport_silent(x, y, z)
--- Or relative:      teleport_silent({rx, ry, rz})
function Actor:teleport_silent(x_or_rel, y, z)
    local ax, ay, az
    if type(x_or_rel) == "table" then
        ax, ay, az = self._scene:abs(x_or_rel[1], x_or_rel[2], x_or_rel[3])
    else
        ax, ay, az = x_or_rel, y, z
    end
    self._w:teleport(self._serial, ax, ay, az)
end

-- ── Effects on actor ────────────────────────────────────────────────────

--- Play a lightning bolt effect on the actor.
function Actor:lightning()
    lightning(self._w, self._serial)
end

--- Play a flamestrike effect on the actor.
function Actor:flamestrike()
    flamestrike(self._w, self._serial)
end

--- Play a heal effect on the actor.
function Actor:heal()
    heal(self._w, self._serial)
end

--- Generic cast: animation + sound + effect on the actor.
function Actor:cast(sound, effect_graphic, opts)
    cast(self._w, self._serial, sound, effect_graphic, opts)
end

--- Play a sound at the actor's location.
function Actor:play_sound(sound_id)
    local x, y, z = self:pos()
    self._w:play_sound(sound_id, x, y, z)
end

--- Play a graphical effect centered on the actor.
function Actor:effect(graphic, opts)
    opts = opts or {}
    local e = self:entity()
    if not e then return end
    self._w:effect({
        direction_type = opts.direction_type or 3,
        source_serial = self._serial,
        graphic = graphic,
        x = e.x, y = e.y, z = e.z,
        speed = opts.speed or 10,
        duration = opts.duration or 30,
        fixed_direction = opts.fixed_direction or false,
    })
end

-- ── Equipment ───────────────────────────────────────────────────────────

--- Apply an Outfit to this actor (replaces all equipment).
--- Sends an update_entity to change appearance immediately.
function Actor:set_outfit(outfit)
    self._w:update_entity(self._serial, {
        items = outfit:items(),
    })
end

--- Update any combination of actor properties on the fly.
--- params: { [graphic], [color], [name], [notoriety], [items], [outfit] }
function Actor:update(params)
    if params.outfit then
        params.items = params.outfit:items()
        params.outfit = nil
    end
    self._w:update_entity(self._serial, params)
end

-- ── Lifecycle ───────────────────────────────────────────────────────────

--- Remove this actor from the world (silently disappears).
function Actor:remove()
    self._w:remove_entity(self._serial)
    self._spawned = false
end

--- Kill this actor: plays death animation, leaves a corpse with
--- equipment, and removes the living mobile.
function Actor:kill()
    self._w:kill_mobile(self._serial)
    self._spawned = false
end
