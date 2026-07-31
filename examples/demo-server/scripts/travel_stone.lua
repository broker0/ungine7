---@access full
-- travel_stone.lua: Interactive travel stone controller.
--
-- Double-click the stone (within 2 tiles) to open a blocking destination
-- menu.  While the menu is open the player cannot cast spells or use
-- skills (bandages remain allowed).  After selecting a destination, a
-- 2-second delay begins — the player must stay near the stone or the
-- teleport is cancelled.

dofile("scripts/lib.lua")
dofile("scripts/travel_dests.lua")

local GUMP_ID       = 0xBEEF0001
local USE_RANGE      = 2        -- max Chebyshev distance to use the stone
local TELEPORT_DELAY = 2.0      -- seconds before teleport fires

local w     = World()
local sched = Scheduler.new()

-- Track which players currently have the gump open.
local open_gumps = {}  -- [player_serial] = true

-- ── Build gump layout (buttons only, no text entries) ────────────────────

local function build_gump()
    local height = 60 + #DESTINATIONS * 30 + 20
    local layout = "{ resizepic 0 0 9200 300 " .. height .. " }"
    layout = layout .. "{ text 20 20 0 0 }"  -- title

    for i, dest in ipairs(DESTINATIONS) do
        local y = 30 + i * 30
        layout = layout .. "{ button 20 " .. y .. " 4005 4007 1 0 " .. i .. " }"
        layout = layout .. "{ text 55 " .. y .. " 0 " .. i .. " }"
    end

    local texts = { "Travel Stone" }
    for _, dest in ipairs(DESTINATIONS) do
        texts[#texts + 1] = dest.name
    end

    return layout, texts
end

local layout, texts = build_gump()

log("Travel Stone controller started (serial=" .. string.format("0x%08X", w:serial()) .. ")")

-- ── Main event loop ──────────────────────────────────────────────────────

while true do
    local ev = wait_event(sched:next_timeout(60000))

    -- Tick deferred teleports.
    sched:tick()

    if ev then
        if ev.type == "used_by" then
            -- Distance check.
            local player = w:get_entity(ev.player_serial)
            local stone  = w:get_entity(w:serial())
            if player and stone then
                if chebyshev(player.x, player.y, stone.x, stone.y) > USE_RANGE then
                    w:send_message(ev.player_serial, "That is too far away.", 0x03B2)
                elseif open_gumps[ev.player_serial] then
                    -- Already has the menu open — ignore.
                else
                    log("Player " .. string.format("0x%08X", ev.player_serial)
                        .. " used travel stone")
                    w:send_gump(ev.player_serial, GUMP_ID, 200, 200,
                                layout, texts, true)  -- blocking = true
                    open_gumps[ev.player_serial] = true
                end
            end

        elseif ev.type == "gump_response" then
            if ev.gump_id == GUMP_ID then
                open_gumps[ev.player_serial] = nil

                if ev.button_id > 0 then
                    local dest = DESTINATIONS[ev.button_id]
                    if dest then
                        local ps = ev.player_serial
                        w:send_message(ps,
                            "Teleporting to " .. dest.name .. " in 2 seconds...", 0x03B2)

                        sched:after(TELEPORT_DELAY, function()
                            -- Re-check: player must still exist and be near the stone.
                            local p = w:get_entity(ps)
                            local s = w:get_entity(w:serial())
                            if not p or not s then return end
                            if chebyshev(p.x, p.y, s.x, s.y) > USE_RANGE then
                                w:send_message(ps,
                                    "You moved too far away from the stone.", 0x03B2)
                                return
                            end
                            log("Teleporting player " .. string.format("0x%08X", ps)
                                .. " to " .. dest.name)
                            local cur_map = w:map_id()
                            local dest_map = dest.map or 0
                            if dest_map ~= cur_map then
                                -- Cross-world transfer (players only).
                                w:teleport_other_world(ps, dest_map,
                                    dest.x + 2, dest.y, dest.z)
                            else
                                w:teleport_other(ps, dest.x + 2, dest.y, dest.z)
                            end
                            w:send_message(ps,
                                "You have been teleported to " .. dest.name .. ".", 0x03B2)
                        end)
                    end
                end
            end
        end
    end
end
