-- Example rpc-proxy Lua script
--
-- Demonstrates the World API and event system.
-- Run with: --lua-script scripts/example.lua
-- Or in-game: .lua scripts/example.lua
--
-- API Reference:
--
--   World()                 Create a World handle
--   w:get_state()           → {character, x, y, z, world}
--   w:get_items()           → array of {serial, graphic, color, x, y, z, count, flags}
--   w:get_mobiles()         → array of {serial, graphic, color, x, y, z, direction, flags, notoriety, equipment}
--   w:get_mobile(serial)    → table or nil
--   w:get_equipment(serial) → array of {serial, graphic, color, layer, parent}
--   w:step(heading)         → true/false  (0=N 1=NE 2=E 3=SE 4=S 5=SW 6=W 7=NW)
--   w:raw_step(heading)     → true/false  (no passability check)
--   w:use_object(serial)    → void
--   w:say(text)             → void
--   w:inject(hex_string)    → void (inject raw C→S packet)
--
--   sleep(ms)               Async sleep (cancellable)
--   log(msg)                Print to server log
--   poll_event()            → event table or nil (non-blocking)
--   wait_event(timeout_ms)  → event table or nil (async, waits up to timeout)
--   register_cleanup(fn)    Register a function to call on script shutdown
--
-- Event types:
--   mobile_appeared   {serial, graphic, color, x, y, z, direction, notoriety}
--   mobile_moved      {serial, graphic, color, x, y, z, direction, notoriety}
--   mobile_removed    {serial}
--   item_appeared     {serial, graphic, color, x, y, z, count}
--   item_removed      {serial}
--   position_changed  {x, y, z, direction}
--   sound_played      {sound_id, x, y, z}
--   effect_played     {direction_type, source_serial, target_serial, graphic, ...}
--   animation_played  {serial, action, frame_count, ...}
--   speech            {serial, graphic, speech_type, color, font, name, message}
--   cliloc_message    {serial, cliloc_id, speech_type, color, font, name, args}
--   damage_dealt      {serial, amount}
--   hp_updated        {serial, hits, max_hits}
--   mana_updated      {serial, mana, max_mana}
--   stamina_updated   {serial, stamina, max_stamina}
--   global_light      {level}
--   weather           {weather_type, num_effects, temperature}
--   season            {season, play_sound}
--   music             {music_id}

local w = World()

-- Print initial state.
local state = w:get_state()
log(string.format("Connected as: %s at (%d, %d, %d) world=%d",
    state.character or "Unknown",
    state.x, state.y, state.z, state.world))

-- Count visible entities.
local mobiles = w:get_mobiles()
local items = w:get_items()
log(string.format("Visible: %d mobiles, %d items", #mobiles, #items))

-- Register cleanup hook.
register_cleanup(function()
    log("Script shutting down — cleanup complete")
end)

-- Main event loop.
log("Entering event loop (Ctrl+C or .lua stop to exit)")
while true do
    local event = wait_event(5000) -- wait up to 5 seconds

    if event then
        if event.type == "speech" then
            log(string.format("[Speech] %s (0x%08X): %s",
                event.name, event.serial, event.message))
        elseif event.type == "mobile_appeared" then
            log(string.format("[Appeared] Mobile 0x%08X graphic=0x%04X at (%d,%d,%d)",
                event.serial, event.graphic, event.x, event.y, event.z))
        elseif event.type == "mobile_removed" then
            log(string.format("[Removed] Mobile 0x%08X", event.serial))
        elseif event.type == "position_changed" then
            log(string.format("[Moved] Position: (%d, %d, %d) dir=%d",
                event.x, event.y, event.z, event.direction))
        elseif event.type == "damage_dealt" then
            log(string.format("[Damage] 0x%08X took %d damage",
                event.serial, event.amount))
        elseif event.type == "hp_updated" then
            log(string.format("[HP] 0x%08X: %d/%d",
                event.serial, event.hits, event.max_hits))
        elseif event.type == "sound_played" then
            log(string.format("[Sound] id=%d at (%d,%d,%d)",
                event.sound_id, event.x, event.y, event.z))
        end
    end
end
