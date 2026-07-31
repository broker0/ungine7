-- scene.lua: Theatre / cutscene scripting library for the demo server.
--
-- Provides a high-level API for orchestrating "theatrical" scenes:
-- spawning NPCs, moving them along routes, playing effects, speech,
-- controlling lighting/weather/music, and running parallel actions.
--
-- Features:
--   * Anchor-based relative coordinates — set an anchor point, then
--     use {rx, ry, rz} offsets everywhere instead of absolute coords.
--   * Entity attachment — attach the scene to a player or NPC; the
--     entity is teleported to the scene location and the anchor tracks
--     its position.
--   * Spawn patterns — spawn_line(), spawn_circle(), spawn_group()
--     for batch placement of actors in formations.
--   * String directions — "n","ne","e","se","s","sw","w","nw" anywhere
--     a direction is expected.
--
-- Usage:
--   dofile("scripts/scene.lua")
--   local w = World(0)
--
--   -- Option A: static anchor
--   local scene = Scene(w, { anchor = {1438, 1696} })
--
--   -- Option B: attach to a player entity
--   local scene = Scene(w)
--   scene:attach(player_serial, { x = 1438, y = 1696, z = 0 })
--
--   -- Spawn actors with relative coordinates
--   local sage = scene:spawn_actor({
--       graphic = 0x0190, name = "Sage",
--       at = {0, 0, 0}, direction = "s",
--       outfit = OUTFITS.MAGE:clone(),
--   })
--   sage:walk_to({5, 5})
--
--   scene:cleanup()
--
-- Module structure:
--   scripts/lib.lua              — low-level helpers (walk, cast, etc.)
--   scripts/scene/helpers.lua    — direction, geometry, pathfinding
--   scripts/constants/           — ANIM, SOUND, EFFECT, MUSIC, LAYER constants
--   scripts/scene/outfit.lua     — BODY, Outfit class, OUTFITS presets
--   scripts/scene/props.lua      — Prop class, PROPS presets
--   scripts/scene/actor.lua      — Actor class
--   scripts/scene/group.lua      — ActorGroup class
--   scripts/scene/scene.lua      — Scene class

dofile("scripts/lib.lua")
dofile("scripts/scene/helpers.lua")
dofile("scripts/constants/animations.lua")
dofile("scripts/constants/sounds.lua")
dofile("scripts/constants/effects.lua")
dofile("scripts/constants/music.lua")
dofile("scripts/constants/layers.lua")
dofile("scripts/scene/outfit.lua")
dofile("scripts/scene/props.lua")
dofile("scripts/scene/actor.lua")
dofile("scripts/scene/group.lua")
dofile("scripts/scene/scene.lua")
