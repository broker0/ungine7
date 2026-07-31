-- init.lua: Server startup script.
--
-- This is the entry point for world initialisation.  Run it via:
--   --lua-script scripts/init.lua
--
-- All init scripts called here should use w:persist(serial) for entities
-- that must survive managed-script reload/stop.  Non-persistent entities
-- (from scene scripts, demos, etc.) are cleaned up automatically.
--
-- After init completes, the managed-script slot remains available:
-- use `.lua scripts/scene_demo.lua` in-game to run a scene without
-- affecting persistent entities (travel stones, guards, etc.).

log("=== Server init starting ===")

-- dofile("scripts/spawn_travel_stone.lua")

log("=== Server init complete ===")
