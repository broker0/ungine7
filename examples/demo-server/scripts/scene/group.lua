-- scene/group.lua: ActorGroup class — a named collection of actors.
--
-- Depends on: scene/actor.lua
-- Loaded automatically by scene.lua.

-- ═══════════════════════════════════════════════════════════════════════
-- ActorGroup class
-- ═══════════════════════════════════════════════════════════════════════

ActorGroup = {}
ActorGroup.__index = ActorGroup

function ActorGroup.new(scene, name, actors)
    local self = setmetatable({}, ActorGroup)
    self._scene = scene
    self._name = name
    self._actors = actors or {}
    return self
end

--- Add an actor to the group.
function ActorGroup:add(actor)
    table.insert(self._actors, actor)
end

--- Return the list of actors.
function ActorGroup:actors()
    return self._actors
end

--- Return the number of actors in the group.
function ActorGroup:count()
    return #self._actors
end

--- Make all actors say the same message (sequentially, with tiny stagger).
function ActorGroup:say(msg, opts)
    for _, a in ipairs(self._actors) do
        a:say(msg, opts)
    end
end

--- Make all actors walk to the same target point (in parallel).
--- Accepts absolute: walk_to(x, y, opts)
--- Or relative:      walk_to({rx, ry}, opts)
function ActorGroup:walk_to(tx_or_rel, ty_or_opts, opts)
    local fns = {}
    for _, a in ipairs(self._actors) do
        table.insert(fns, function()
            a:walk_to(tx_or_rel, ty_or_opts, opts)
        end)
    end
    self._scene:parallel(table.unpack(fns))
end

--- Make all actors run to the same target point (in parallel).
--- Accepts absolute: run_to(x, y, opts)
--- Or relative:      run_to({rx, ry}, opts)
function ActorGroup:run_to(tx_or_rel, ty_or_opts, opts)
    local fns = {}
    for _, a in ipairs(self._actors) do
        table.insert(fns, function()
            a:run_to(tx_or_rel, ty_or_opts, opts)
        end)
    end
    self._scene:parallel(table.unpack(fns))
end

--- Play an animation on all actors.
function ActorGroup:animate(action, frame_count, opts)
    for _, a in ipairs(self._actors) do
        a:animate(action, frame_count, opts)
    end
end

--- Remove all actors from the world.
function ActorGroup:remove()
    for _, a in ipairs(self._actors) do
        a:remove()
    end
end
