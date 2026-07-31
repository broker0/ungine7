---@access full
-- teleporter.lua: Step-on teleporter controller.
--
-- Attach to a hidden item placed on the ground.  When a mobile steps
-- onto the same tile, it is instantly teleported to the configured
-- destination.
--
-- The destination is stored in item_props meta:
--   teleport_x, teleport_y, teleport_z  (required)
--   teleport_map                         (optional, for cross-map)
--
-- Example spawn:
--   local serial = w:spawn_item({ graphic = 0x0001, x = 100, y = 200, z = 0, hidden = true })
--   w:set_item_props(serial, { meta = { teleport_x = 500, teleport_y = 600, teleport_z = 0 } })
--   w:attach_controller(serial, "teleporter.lua")
--   w:persist(serial)

local w = World()
local my_serial = w:serial()

log("Teleporter controller started (serial=" .. string.format("0x%08X", my_serial) .. ")")

-- ── Read destination from item props ─────────────────────────────────────

local function get_destination()
    local props = w:get_item_props(my_serial)
    if not props or not props.meta then return nil end
    local m = props.meta
    local tx = m.teleport_x
    local ty = m.teleport_y
    local tz = m.teleport_z
    if not tx or not ty or not tz then
        return nil
    end
    return {
        x   = math.floor(tx),
        y   = math.floor(ty),
        z   = math.floor(tz),
        map = m.teleport_map,  -- nil = same map
    }
end

-- ── Main event loop ──────────────────────────────────────────────────────

while true do
    local ev = wait_event(60000)

    if ev and ev.type == "stepped_on_by" then
        local dest = get_destination()
        if dest then
            local cur_map = w:map_id()
            if dest.map and math.floor(dest.map) ~= cur_map then
                -- Cross-world: hand off to the player's session.  Only
                -- player mobiles can be moved across worlds this way.
                log(string.format("Teleporting 0x%08X to (%d,%d,%d) on map %d",
                    ev.mobile_serial, dest.x, dest.y, dest.z, math.floor(dest.map)))
                w:teleport_other_world(ev.mobile_serial, math.floor(dest.map),
                    dest.x, dest.y, dest.z)
            else
                log(string.format("Teleporting 0x%08X to (%d,%d,%d)",
                    ev.mobile_serial, dest.x, dest.y, dest.z))
                w:teleport_other(ev.mobile_serial, dest.x, dest.y, dest.z)
            end
        else
            log("Teleporter 0x" .. string.format("%08X", my_serial)
                .. ": no destination configured in item_props.meta")
        end
    end
end
