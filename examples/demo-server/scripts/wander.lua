-- wander.lua: Example script that makes a mobile wander randomly.
--
-- Usage: demo-server --log world.uolog --lua-script scripts/wander.lua
--   or at runtime:  .lua scripts/wander.lua
--
-- The script creates a World context, picks an entity, and moves it
-- in a random direction every few seconds.  Edit SERIAL below to
-- control a specific mobile.
--
-- ======================= API Reference =======================
--
-- World(map_id) -> world object
--   Creates a context bound to a specific map. All methods below
--   send RPC commands to the game worker for that map.
--
--   w:get_entity(serial)              -> table | nil
--     Returns entity info: { type, serial, graphic, x, y, z, ... }
--     Mobile fields:  direction, color, name, hits, hits_max,
--                     notoriety, status
--     Item fields:    is_container
--
--   w:step(serial, direction)         -> table | nil
--     Move mobile one tile. direction: 0=N,1=NE,2=E,...7=NW
--     Returns { x, y, z, direction } on success, nil if blocked.
--
--   w:teleport(serial, x, y, z)
--     Teleport entity (no passability check).
--
--   w:query_area(x1, y1, x2, y2)     -> table of entity tables
--     All entities in the rectangle.
--
--   w:test_step(x, y, z, direction)   -> number | nil
--     Test if a step is passable. Returns new Z or nil.
--
--   w:resolve_z(x, y, z_hint, dir)   -> number | nil
--     Resolve standing height at a tile.
--
--   w:play_sound(sound_id, x, y, z)
--     Play a sound effect at coordinates. Heard by nearby clients.
--
--   w:effect(params_table)
--     Spawn a graphical effect. Fields (all optional with defaults):
--       direction_type  0=projectile,1=lightning,2=stationary,3=follow (default 2)
--       source_serial   source entity (default 0)
--       target_serial   target entity (default 0)
--       graphic         effect graphic id (default 0)
--       x,y,z           source position
--       target_x,y,z    target position (for projectiles)
--       speed           animation speed (default 10)
--       duration        effect duration (default 30)
--       fixed_direction true/false (default true)
--       explode         true/false (default false)
--
--   w:animate(serial, action, frame_count [, opts])
--     Play character animation. opts table (optional):
--       repeat_count    times to play (default 1)
--       reverse         play backwards (default false)
--       repeat          loop forever (default false)
--       frame_delay     delay between frames (default 0)
--     Common actions: 0x00=walk, 0x04=stand, 0x0B=swing,
--       0x10=cast, 0x14=get_hit, 0x15=die, 0x20=bow, 0x21=salute
--     Named constants (ANIM.WALK, ANIM.CAST, etc.) are available
--     after loading scene.lua.
--
--   w:say(serial, message [, opts])
--     Make entity speak. opts table (optional):
--       speech_type     0=normal,1=broadcast,2=emote,6=system
--                       (default 0)
--       color           text hue (default 0x03B2)
--       font            font id (default 3)
--       name            speaker name (default: entity name)
--
--   w:spawn_npc(params)                -> number (serial)
--     Spawn a new NPC mobile. params table:
--       graphic         body graphic id (default 0x0190)
--       x, y, z         world position
--       name            NPC name (default "NPC")
--       color           hue (default 0)
--       direction       facing 0-7 (default 0)
--       notoriety       0-7 (default 1=innocent)
--       hits, hits_max  hit points (default 100/100)
--     Returns the serial of the newly created entity.
--
--   w:remove_entity(serial)
--     Remove an entity from the world.
--
--   w:set_light(level)
--     Set global light level (0x00=day, 0x1F=black).
--     Broadcast to all clients on this map.
--
--   w:set_weather(type [, num_effects, temperature])
--     Set weather: 0=rain, 1=storm, 2=snow, 0xFF=none.
--     num_effects: particle count (default 0x40).
--     temperature: (default 0x10).
--
--   w:set_season(season [, play_sound])
--     Set season: 0=spring, 1=summer, 2=fall, 3=winter, 4=desolation.
--     play_sound: play transition sound (default true).
--
--   w:play_music(music_id)
--     Play background music track.
--
--   w:map_id()                        -> number
--
-- Global functions:
--   sleep(ms)                  Async sleep (cancelable on reload).
--   log(msg)                   Print to server log.
--   poll_event()        -> table | nil   Non-blocking event poll.
--   wait_event(ms)      -> table | nil   Blocking wait with timeout.
--
-- World events (from poll_event / wait_event):
--   { type="entity_moved",   map_id, serial,
--     old_x, old_y, old_z, new_x, new_y, new_z, direction }
--   { type="entity_spawned", map_id, serial, x, y, z }
--   { type="entity_removed", map_id, serial, x, y, z }
--   { type="entity_updated", map_id, serial, x, y, z }
--   { type="sound_played",   map_id, sound_id, x, y, z }
--   { type="effect_played",  map_id, direction_type, source_serial,
--     target_serial, graphic, x, y, z, target_x, target_y, target_z,
--     speed, duration, fixed_direction, explode }
--   { type="animation_played", map_id, serial, action, frame_count,
--     repeat_count, reverse, repeat, frame_delay }
--   { type="speech",         map_id, serial, graphic, speech_type,
--     color, font, name, message, x, y }
--   { type="global_light",   map_id, level }
--   { type="weather",        map_id, weather_type, num_effects,
--     temperature }
--   { type="season",         map_id, season, play_sound }
--   { type="music",          map_id, music_id }
--
-- Commands (type in game client chat):
--   .lua <path>     Load and run a script (hot-reload enabled)
--   .lua reload     Reload current script from disk
--   .lua stop       Stop current script
--
-- =============================================================

dofile("scripts/lib.lua")

local SERIAL = 0x03F84C13   -- entity serial to control (change as needed)
local MAP_ID = 0            -- map/world id

local w = World(MAP_ID)

log("wander script started")
local me = w:get_entity(SERIAL)

say(w, SERIAL, "Hey William!", 1000)
say(w, SERIAL, "I'm here!")
w:play_sound(0x449, me.x, me.y, me.z)   -- sound effect
w:animate(SERIAL, 0x1A, 20)             -- ANIM.MOUNTED_ATTACK (also used for teleport shimmer)
run(w, SERIAL, 4, 3)
walk(w, SERIAL, 4, 2)

lightning(w, SERIAL)
sleep(500)

teleport(w, SERIAL, 1879, 1541, 0)
sleep(500)

heal(w, SERIAL)
sleep(500)

flamestrike(w, SERIAL)
sleep(500)

local me = w:get_entity(SERIAL)
w:play_sound(0x42B, me.x, me.y, me.z)   -- sound effect
say(w, SERIAL, "Uhh...!")

log("wander script stopped")


-- Look up the entity to confirm it exists.
local me = w:get_entity(SERIAL)
if me then
    log(string.format("controlling %s '%s' at (%d,%d,%d)", me.type, me.name or "", me.x, me.y, me.z))
else
    log(string.format("entity 0x%08X not found — will retry on movement", SERIAL))
end

while true do
    -- Pick a random direction (0=N, 1=NE, 2=E, ... 7=NW)
    local dir = math.random(0, 7)
    local result = w:step(SERIAL, dir)

    if result then
        log(string.format("moved to (%d,%d,%d) dir=%d",
            result.x, result.y, result.z, result.direction))
    end

    -- Check for world events (non-blocking).
    local event = poll_event()
    while event do
        if event.type == "entity_moved" and event.serial ~= SERIAL then
            log(string.format("nearby: 0x%08X moved to (%d,%d,%d)",
                event.serial, event.new_x, event.new_y, event.new_z))
        end
        event = poll_event()
    end

    sleep(3000)
end
