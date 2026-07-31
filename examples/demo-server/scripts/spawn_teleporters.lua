-- spawn_teleporters.lua: Spawn hidden step-on teleporter items.
--
-- Usage:  .lua scripts/spawn_teleporters.lua
--         (or called from init.lua via dofile)
--
-- Each entry in TELEPORTERS defines a pair of coordinates: the source
-- tile where the invisible item is placed, and the destination where
-- any mobile stepping on it will be teleported.
--
-- The items are hidden (invisible to regular players, semi-transparent
-- for GM+), persistent, and use the teleporter.lua controller.

local w = World(0)

-- ── Teleporter definitions ───────────────────────────────────────────────
--
-- { name, from_x, from_y, from_z, to_x, to_y, to_z, to_map }
--
-- Add entries here to create new teleporters.  Each teleporter is
-- one-way; for a two-way link, add a second entry with from/to swapped.
-- `to_map` is optional (defaults to the spawn world 0); set it to teleport
-- across worlds (player mobiles only — cross-world NPC transfer is TODO).

local TELEPORTERS = {
    -- Example: Britain bank entrance <-> moongate clearing
    { name = "Brit Bank -> Moonglow",  from_x = 1417, from_y = 1698, from_z = 0,  to_x = 4471, to_y = 1177, to_z = 0 },
    { name = "Moonglow -> Brit Bank",  from_x = 4471, from_y = 1177, from_z = 0,  to_x = 1417, to_y = 1698, to_z = 0 },
    -- Example cross-world link into world 1.
    { name = "Brit -> World 1",        from_x = 1420, from_y = 1698, from_z = 0,  to_x = 1438, to_y = 1696, to_z = 0, to_map = 1 },
}

-- ── Spawn loop ───────────────────────────────────────────────────────────

local GRAPHIC = 0x1BC3   -- nodraw / small invisible item

for _, tp in ipairs(TELEPORTERS) do
    local serial = w:spawn_item({
        graphic = GRAPHIC,
        x       = tp.from_x,
        y       = tp.from_y,
        z       = tp.from_z,
        hidden  = true,
    })

    w:set_item_props(serial, {
        name = tp.name,
        meta = {
            teleport_x   = tp.to_x,
            teleport_y   = tp.to_y,
            teleport_z   = tp.to_z,
            teleport_map = tp.to_map,  -- nil = same world
        },
    })

    w:attach_controller(serial, "teleporter.lua")
    w:persist(serial)

    log(string.format("Teleporter [%s] at (%d,%d,%d) -> (%d,%d,%d) serial=0x%08X",
        tp.name, tp.from_x, tp.from_y, tp.from_z,
        tp.to_x, tp.to_y, tp.to_z, serial))
end

log(string.format("Spawned %d teleporters", #TELEPORTERS))
