-- spawn_travel_stone.lua: Spawn travel stones at every destination.
--
-- Usage:  .lua scripts/spawn_travel_stone.lua
--         (or called from init.lua via dofile)
--
-- Each stone is marked as persistent so it survives script reload/stop.
-- The controller script path is stored in item_props.meta, so stones
-- are restored automatically after .save / .load.

dofile("scripts/travel_dests.lua")

-- Cache one World handle per map id (default 0).  Each destination's stone
-- is spawned into its own world (`dest.map`), so cross-world destinations
-- get a physical stone in the right facet.  Spawning into a world that does
-- not exist yet auto-creates the zone (worker `ensure_zone`).
local worlds = {}
local function world_for(map)
    map = map or 0
    if not worlds[map] then
        worlds[map] = World(map)
    end
    return worlds[map]
end

for _, dest in ipairs(DESTINATIONS) do
    local dest_map = dest.map or 0
    local w = world_for(dest_map)
    local serial = w:spawn_item({
        graphic = 0x0EDC,
        color = 0x04AA,
        x = dest.x,
        y = dest.y,
        z = dest.z,
    })
    w:attach_controller(serial, "travel_stone.lua")
    w:persist(serial)
    log(string.format("Travel stone %s at (%d,%d,%d) map=%d serial=0x%08X",
        dest.name, dest.x, dest.y, dest.z, dest_map, serial))
end

log(string.format("Spawned %d travel stones", #DESTINATIONS))
