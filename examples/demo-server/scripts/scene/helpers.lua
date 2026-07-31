-- scene/helpers.lua: Direction, geometry, and pathfinding utilities.
--
-- These are internal helpers used by Actor and Scene.
-- Loaded automatically by scene.lua.

-- ── Direction helpers ───────────────────────────────────────────────────

DIR_NAMES = {
    n = 0, north = 0,
    ne = 1, northeast = 1,
    e = 2, east = 2,
    se = 3, southeast = 3,
    s = 4, south = 4,
    sw = 5, southwest = 5,
    w = 6, west = 6,
    nw = 7, northwest = 7,
}

--- Resolve a direction value (number 0-7 or string name) to a number.
function resolve_dir(d)
    if type(d) == "number" then return d % 8 end
    if type(d) == "string" then
        return DIR_NAMES[d:lower()] or 0
    end
    return 0
end

-- ── Geometry helpers ────────────────────────────────────────────────────

--- Chebyshev distance between two points.
function distance(x1, y1, x2, y2)
    return math.max(math.abs(x2 - x1), math.abs(y2 - y1))
end

--- Compute direction from (x1,y1) to (x2,y2) as a UO direction (0-7).
function direction_to(x1, y1, x2, y2)
    local dx = x2 - x1
    local dy = y2 - y1
    if dx == 0 and dy == 0 then return nil end

    -- Normalize to -1/0/+1
    local sx = (dx > 0) and 1 or (dx < 0) and -1 or 0
    local sy = (dy > 0) and 1 or (dy < 0) and -1 or 0

    -- UO direction mapping: N=0, NE=1, E=2, SE=3, S=4, SW=5, W=6, NW=7
    --   dx: -1=W, 0=center, +1=E
    --   dy: -1=N, 0=center, +1=S
    local dir_map = {
        [-1] = { [-1] = 7, [0] = 0, [1] = 1 },  -- dy=-1 (north-ish)
        [0]  = { [-1] = 6, [0] = 0, [1] = 2 },  -- dy=0 (east/west)
        [1]  = { [-1] = 5, [0] = 4, [1] = 3 },  -- dy=+1 (south-ish)
    }
    return dir_map[sy][sx]
end

-- ── Simple pathfinding ──────────────────────────────────────────────────

--- Very simple pathfinding: tries to walk straight, and if blocked,
--- tries adjacent directions to get around obstacles. Not a full A*,
--- but enough to walk around a tree or fence.
---
--- Returns the direction to step in, or nil if completely stuck.
function find_step_direction(w, x, y, z, target_x, target_y)
    local desired = direction_to(x, y, target_x, target_y)
    if desired == nil then return nil end

    -- Try desired direction first
    local new_z = w:test_step(x, y, z, desired)
    if new_z then return desired end

    -- Try the two adjacent directions (clockwise and counter-clockwise)
    local cw = (desired + 1) % 8
    local ccw = (desired + 7) % 8
    if w:test_step(x, y, z, cw) then return cw end
    if w:test_step(x, y, z, ccw) then return ccw end

    -- Try wider angles
    local cw2 = (desired + 2) % 8
    local ccw2 = (desired + 6) % 8
    if w:test_step(x, y, z, cw2) then return cw2 end
    if w:test_step(x, y, z, ccw2) then return ccw2 end

    return nil  -- stuck
end
