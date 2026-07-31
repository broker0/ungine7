-- mob_controller.lua: Unified NPC controller adapter.
--
-- Wraps the differences between async mode (runtime.rs) and controller
-- mode (lua_controller.rs) behind a single API.  Write your AI logic
-- once, prototype it in async mode (hot-reload, spawn on the fly, see
-- all WorldEvents), then switch to controller mode by changing one line.
--
-- ╔═══════════════════════════════════════════════════════════════════╗
-- ║  Async mode (prototyping):                                      ║
-- ║    dofile("scripts/mob_controller.lua")                         ║
-- ║    local mob = MobController.spawn(World(0), { ... })           ║
-- ║                                                                 ║
-- ║  Controller mode (production):                                  ║
-- ║    dofile("scripts/mob_controller.lua")                         ║
-- ║    local mob = MobController.attach(World())                    ║
-- ║                                                                 ║
-- ║  After that, the API is IDENTICAL.                              ║
-- ╚═══════════════════════════════════════════════════════════════════╝
--
-- ## API Reference
--
-- ### Construction
--
--   MobController.spawn(w, params)   -- async mode: spawn NPC + wrap
--   MobController.attach(w)          -- controller mode: wrap existing
--
-- ### Identity
--
--   mob.serial          -- serial number of the controlled entity
--   mob.mode            -- "async" or "controller"
--
-- ### World queries
--
--   mob:me()                            -> entity table or nil
--   mob:get_entity(serial)              -> entity table or nil
--   mob:query_area(x1, y1, x2, y2)     -> table of entities
--   mob:has_los(x1, y1, z1, x2, y2, z2) -> boolean
--   mob:test_step(x, y, z, dir)        -> new_z or nil
--   mob:resolve_z(x, y, z_hint, dir)   -> z or nil
--
-- ### Movement
--
--   mob:step(dir)               -> pos table or nil
--   mob:face(dir)               -> boolean
--   mob:teleport(x, y, z)      -> boolean
--
-- ### Combat
--
--   mob:deal_damage(serial, amount)      -> { new_hits, killed } or nil
--   mob:heal_entity(serial, amount)      -> new_hits or nil
--   mob:modify_mana(serial, delta)       -> new_mana or nil
--   mob:modify_stamina(serial, delta)    -> new_stamina or nil
--
-- ### Presentation
--
--   mob:say(msg, opts)                       -- speech
--   mob:animate(action, frame_count, opts)   -- animation
--   mob:play_sound(id, x, y, z)             -- sound effect
--   mob:effect(params)                       -- graphical effect
--
-- ### Lifecycle
--
--   mob:kill()                 -- kill with corpse + remove
--   mob:remove()               -- remove entity from world
--
-- ### Events
--
--   mob:poll_event()           -> event table or nil
--   mob:wait_event(timeout_ms) -> event table or nil
--
--   Events are normalized to controller format:
--     { type="damage_received", amount=N, source_serial=0 }
--     { type="moved", direction=D, x=X, y=Y, z=Z }
--     { type="timer_fired", timer_id=N }
--     { type="killed" }
--

dofile("scripts/scene/helpers.lua")
dofile("scripts/constants/animations.lua")
dofile("scripts/constants/sounds.lua")
dofile("scripts/constants/effects.lua")
dofile("scripts/constants/music.lua")
dofile("scripts/constants/combat.lua")

MobController = {}
MobController.__index = MobController

-- ═════════════════════════════════════════════════════════════════════
-- Construction
-- ═════════════════════════════════════════════════════════════════════

--- Spawn a new NPC in async mode and wrap it.
--- @param w       World object from World(map_id)
--- @param params  spawn_npc parameters (graphic, x, y, z, name, ...)
--- @return MobController
function MobController.spawn(w, params)
    local serial = w:spawn_npc(params)
    log(string.format("[mob_ctrl] spawned 0x%08X (%s)", serial, params.name or "NPC"))
    return MobController._create(w, serial, "async")
end

--- Wrap an existing entity in controller mode.
--- @param w  World object from World() (no map_id in controller mode)
--- @return MobController
function MobController.attach(w)
    local serial = w:serial()
    log(string.format("[mob_ctrl] attached to 0x%08X", serial))
    return MobController._create(w, serial, "controller")
end

--- Internal constructor.
function MobController._create(w, serial, mode)
    local self = setmetatable({}, MobController)
    self.w = w
    self.serial = serial
    self.mode = mode
    return self
end

-- ═════════════════════════════════════════════════════════════════════
-- World queries (identical in both modes)
-- ═════════════════════════════════════════════════════════════════════

--- Get info about the controlled entity.
function MobController:me()
    return self.w:get_entity(self.serial)
end

--- Get info about any entity.
function MobController:get_entity(serial)
    return self.w:get_entity(serial)
end

--- Query all entities in a rectangle.
function MobController:query_area(x1, y1, x2, y2)
    return self.w:query_area(x1, y1, x2, y2)
end

--- Check line of sight between two 3D points.
function MobController:has_los(x1, y1, z1, x2, y2, z2)
    return self.w:has_los(x1, y1, z1, x2, y2, z2)
end

--- Test if a step is passable. Returns new Z or nil.
function MobController:test_step(x, y, z, dir)
    return self.w:test_step(x, y, z, dir)
end

--- Resolve standing height at a tile.
function MobController:resolve_z(x, y, z_hint, dir)
    return self.w:resolve_z(x, y, z_hint, dir)
end

-- ═════════════════════════════════════════════════════════════════════
-- Movement (adapted signatures)
-- ═════════════════════════════════════════════════════════════════════

--- Move one tile in a direction (0-7, or +128 for running).
function MobController:step(dir)
    if self.mode == "controller" then
        return self.w:step(dir)
    else
        return self.w:step(self.serial, dir)
    end
end

--- Turn to face a direction (0-7) without moving.
function MobController:face(dir)
    if self.mode == "controller" then
        return self.w:face(dir)
    else
        -- In async mode: update_entity with new direction.
        self.w:update_entity(self.serial, { direction = dir })
        return true
    end
end

--- Teleport to absolute coordinates.
function MobController:teleport(x, y, z)
    if self.mode == "controller" then
        return self.w:teleport(x, y, z)
    else
        return self.w:teleport(self.serial, x, y, z)
    end
end

-- ═════════════════════════════════════════════════════════════════════
-- Combat (identical in both modes — both now have these methods)
-- ═════════════════════════════════════════════════════════════════════

--- Deal damage to a target. Returns { new_hits, killed } or nil.
function MobController:deal_damage(target_serial, amount)
    return self.w:deal_damage(target_serial, amount, self.serial)
end

--- Heal a target. Returns new HP or nil.
function MobController:heal_entity(target_serial, amount)
    return self.w:heal_entity(target_serial, amount)
end

--- Modify mana (delta can be negative). Returns new mana or nil.
function MobController:modify_mana(target_serial, delta)
    return self.w:modify_mana(target_serial, delta)
end

--- Modify stamina (delta can be negative). Returns new stamina or nil.
function MobController:modify_stamina(target_serial, delta)
    return self.w:modify_stamina(target_serial, delta)
end

-- ═════════════════════════════════════════════════════════════════════
-- Presentation (adapted signatures)
-- ═════════════════════════════════════════════════════════════════════

--- Make the controlled entity speak.
function MobController:say(msg, opts)
    if self.mode == "controller" then
        return self.w:say(msg, opts)
    else
        return self.w:say(self.serial, msg, opts)
    end
end

--- Play character animation on the controlled entity.
function MobController:animate(action, frame_count, opts)
    return self.w:animate(self.serial, action, frame_count, opts)
end

--- Play sound at specific coordinates.
--- If no coordinates given, plays at entity's position.
function MobController:play_sound(id, x, y, z)
    if x then
        self.w:play_sound(id, x, y, z)
    else
        local me = self:me()
        if me then
            self.w:play_sound(id, me.x, me.y, me.z)
        end
    end
end

--- Spawn a graphical effect.
function MobController:effect(params)
    return self.w:effect(params)
end

-- ═════════════════════════════════════════════════════════════════════
-- Lifecycle
-- ═════════════════════════════════════════════════════════════════════

--- Kill the controlled entity (death animation + corpse).
function MobController:kill()
    if self.mode == "async" then
        self.w:kill_mobile(self.serial)
    end
    -- In controller mode: not yet supported (requires framework change).
    -- The entity can be removed after death via mob:remove().
end

--- Remove the controlled entity from the world.
function MobController:remove()
    if self.mode == "async" then
        self.w:remove_entity(self.serial)
    end
    -- In controller mode: not yet supported.
end

-- ═════════════════════════════════════════════════════════════════════
-- Events
-- ═════════════════════════════════════════════════════════════════════
--
-- In controller mode, events come pre-filtered for "our" entity:
--   { type="damage_received", source_serial=N, amount=N }
--   { type="spell_hit", source_serial=N, spell_id=N }
--   { type="moved", direction=N, x=N, y=N, z=N }
--   { type="timer_fired", timer_id=N }
--
-- In async mode, we receive all WorldEvents and filter/transform them
-- into the same controller-format events.

--- Transform a raw WorldEvent into a controller-format event,
--- or return nil if it's not relevant to this entity.
function MobController:_transform_event(ev)
    if not ev then return nil end

    -- Damage dealt to US -> damage_received
    if ev.type == "damage_dealt" and ev.serial == self.serial then
        return {
            type = "damage_received",
            source_serial = ev.source_serial or 0,
            amount = ev.amount,
        }
    end

    -- Our entity moved
    if ev.type == "entity_moved" and ev.serial == self.serial then
        return {
            type = "moved",
            direction = ev.direction or 0,
            x = ev.new_x,
            y = ev.new_y,
            z = ev.new_z,
        }
    end

    -- Our entity was killed
    if ev.type == "mobile_killed" and ev.serial == self.serial then
        return { type = "killed" }
    end

    -- Healed
    if ev.type == "mobile_healed" and ev.serial == self.serial then
        return {
            type = "healed",
            amount = ev.amount,
            new_hits = ev.new_hits,
        }
    end

    return nil  -- not relevant
end

--- Non-blocking event poll. Returns a controller-format event or nil.
function MobController:poll_event()
    if self.mode == "controller" then
        return poll_event()
    end

    -- Async: drain WorldEvents, filter for our entity.
    while true do
        local ev = poll_event()
        if ev == nil then return nil end
        local transformed = self:_transform_event(ev)
        if transformed then return transformed end
    end
end

--- Wait for an event with timeout. Returns a controller-format event or nil.
function MobController:wait_event(timeout_ms)
    if self.mode == "controller" then
        return wait_event(timeout_ms)
    end

    -- Async: poll with deadline.
    -- Note: in async mode, wait_event() blocks on the broadcast channel.
    -- We call it in a loop, transforming events until we find a relevant one
    -- or the timeout expires.
    local start_ms = nil  -- we rely on Lua clock or iteration limits

    -- Strategy: call wait_event with short slices to check each event.
    -- If no relevant event within timeout, return nil.
    local remaining = timeout_ms
    while remaining > 0 do
        -- Use a smaller slice to allow checking multiple events.
        local slice = math.min(remaining, 100)
        local ev = wait_event(slice)
        if ev then
            local transformed = self:_transform_event(ev)
            if transformed then return transformed end
        end
        remaining = remaining - slice
    end
    return nil
end

-- ═════════════════════════════════════════════════════════════════════
-- High-level helpers
-- ═════════════════════════════════════════════════════════════════════

--- Find nearby mobiles within a range.
--- Returns a list of entity tables.
--- @param range   Chebyshev distance
--- @param filter  optional function(entity) -> boolean
function MobController:find_nearby(range, filter)
    local me = self:me()
    if not me then return {} end
    local entities = self:query_area(
        me.x - range, me.y - range,
        me.x + range, me.y + range
    )
    local result = {}
    for _, e in ipairs(entities) do
        if e.is_mobile and e.serial ~= self.serial then
            if not filter or filter(e) then
                table.insert(result, e)
            end
        end
    end
    return result
end

--- Check if a target is visible (LOS) from our position.
--- Adds eye-height offset (+14 for humanoids).
function MobController:can_see(target_serial)
    local me = self:me()
    local target = self:get_entity(target_serial)
    if not me or not target then return false end
    return self:has_los(
        me.x, me.y, me.z + 14,
        target.x, target.y, target.z + 14
    )
end

--- Walk towards a target position using simple pathfinding.
--- Returns true if a step was taken, false if stuck.
function MobController:walk_towards(target_x, target_y)
    local me = self:me()
    if not me then return false end
    local dir = find_step_direction(self.w, me.x, me.y, me.z, target_x, target_y)
    if dir then
        return self:step(dir) ~= nil
    end
    return false
end

--- Compute Chebyshev distance to another entity.
function MobController:distance_to(serial)
    local me = self:me()
    local target = self:get_entity(serial)
    if not me or not target then return 999 end
    return distance(me.x, me.y, target.x, target.y)
end

--- Face towards a target entity.
function MobController:face_towards(serial)
    local me = self:me()
    local target = self:get_entity(serial)
    if not me or not target then return false end
    local dir = direction_to(me.x, me.y, target.x, target.y)
    if dir then
        return self:face(dir)
    end
    return false
end

--- Is the given serial likely a player (not a Lua-spawned NPC)?
--- Convention: Lua-spawned mobiles use serials >= 0x3F000000.
function MobController.is_player(entity)
    return entity.is_mobile and entity.serial < 0x3F000000
end
