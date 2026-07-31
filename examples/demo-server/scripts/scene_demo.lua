-- scene_demo.lua: Example scene demonstrating the theatre scripting API.
--
-- Usage: demo-server --log world.uolog --lua-script scripts/scene_demo.lua
--
-- This script stages a short "encounter" scene:
-- an old sage warns of danger, a thief sneaks in and gets struck
-- down by lightning, a daemon is summoned and banished, then
-- guards arrive to clean up.
--
-- Demonstrates: body presets (humans, monsters), anchor-based
-- coordinates, attach, outfits, lighting, weather, spawn patterns,
-- parallel movement, effects, named constants (ANIM, SOUND, EFFECT, MUSIC).

dofile("scripts/scene.lua")

-- ── Configuration ───────────────────────────────────────────────────────

local MAP_ID = 0

-- Player serial to attach the scene to (comment out to use static anchor).
-- The player will be teleported to the scene location.
local PLAYER_SERIAL = nil  -- e.g. 0x03F84C13

-- Scene location (absolute world coordinates).
local SCENE_X = 1438
local SCENE_Y = 1696
local SCENE_Z = 0

-- Speech color for villains (thief, daemon).
local COLOR_VILLAIN = 0x0021  -- red

-- Light levels: 0x00 = bright day, 0x1F = pitch black.
local LIGHT_DUSK  = 0x04  -- slightly dim
local LIGHT_DARK  = 0x14  -- quite dark
local LIGHT_DAWN  = 0x02  -- near full day

-- ── Create the scene ────────────────────────────────────────────────────

local w = World(MAP_ID)
local scene = Scene(w)

-- Attach to player entity (teleports them to the scene, sets anchor from
-- their position) or use a static anchor if no player serial.
if PLAYER_SERIAL then
    scene:attach(PLAYER_SERIAL, {
        x = SCENE_X, y = SCENE_Y, z = SCENE_Z,
        offset = {2, 2},  -- stage center is 2 tiles SE of the player
    })
else
    scene:set_anchor(SCENE_X, SCENE_Y, SCENE_Z)
end

log("=== Scene demo starting ===")

-- ── Act 1: Set the stage ────────────────────────────────────────────────
-- All coordinates below are relative to the anchor.

log("--- Act 1: Set the stage ---")

scene:set_light(LIGHT_DUSK)
scene:play_music(MUSIC.APPROACH)

-- Spawn the narrator — body preset handles graphic+color, outfit adds gear.
local sage = scene:spawn_actor({
    body      = BODY.MALE_TAN,
    name      = "Old Sage",
    at        = {0, 0, 0},
    direction = "s",
    outfit    = OUTFITS.MAGE:clone(),
})
sleep(500)

sage:say("Gather 'round, travelers...", { pause = 3000 })
sage:say("Dark omens stir in the east.", { pause = 2500 })

-- ── Act 2: The thief appears ────────────────────────────────────────────

log("--- Act 2: The thief appears ---")

scene:fade_light(LIGHT_DUSK, LIGHT_DARK, 2000)

-- Spawn the thief — dark-skinned rogue.
local thief = scene:spawn_actor({
    body      = BODY.MALE_DARK,
    name      = "Shadowblade",
    at        = {8, 20, 0},
    direction = "nw",
    outfit    = OUTFITS.ROGUE:clone(),
})
sleep(500)

scene:sound(SOUND.SNEAK, thief)
sleep(500)

-- Waypoints are relative to anchor — just {rx, ry}.
thief:walk_route({
    { 6, 6 },
    { 4, 4,
      on_arrive = function(actor)
          actor:say("Your gold or your life!", { pause = 1500, color = COLOR_VILLAIN })
      end
    },
    { 2, 2 },
}, { speed = 250 })

-- ── Act 3: Confrontation ────────────────────────────────────────────────

log("--- Act 3: Confrontation ---")

sage:face_towards({2, 2})
sleep(300)

sage:say("You dare threaten me?!", { pause = 1500 })
sage:animate(ANIM.CAST, 7)
sleep(300)

scene:set_weather("storm", 60)
sleep(800)

scene:lightning(thief)
thief:play_sound(SOUND.LIGHTNING)
sleep(500)

thief:say("Argh!", { pause = 500, color = COLOR_VILLAIN })
thief:animate(ANIM.DIE_FORWARD, 7)
sleep(1000)

scene:flamestrike(thief)
sleep(800)

thief:kill()

-- ── Act 4: The Daemon ───────────────────────────────────────────────────

log("--- Act 4: The Daemon ---")

sage:say("But the darkness runs deeper...", { pause = 2500 })
sage:animate(ANIM.CAST, 7)
sleep(500)

-- Pentagram and braziers mark the summoning site.
local pentagram = scene:spawn_prop(PROPS.PENTAGRAM, {6, 1, 0})
local brazier_l = scene:spawn_prop(PROPS.BRAZIER, {6, -1, 0})
local brazier_r = scene:spawn_prop(PROPS.BRAZIER, {6,  3, 0})
sleep(300)

-- Flamestrike at the summoning point, then the daemon appears.
scene:effect_at(pentagram, EFFECT.FLAMESTRIKE)
scene:sound(SOUND.DAEMON_ROAR, pentagram)
sleep(800)

local daemon = scene:spawn_actor({
    body      = BODY.DAEMON,
    name      = "Xargathos",
    at        = {6, 1, 0},
    direction = "w",
    notoriety = 6,  -- murderer (red)
})
sleep(500)

scene:lightning(daemon)
sleep(300)

daemon:say("Who dares summon me?!", { pause = 2000, color = COLOR_VILLAIN })
daemon:animate(ANIM.STAND, 5)  -- idle/intimidate
sleep(800)

sage:face_towards({6, -4})
sleep(200)
sage:say("Begone, fiend!", { pause = 1500 })
sage:animate(ANIM.CAST, 7)
sleep(300)

-- Barrage of effects on the daemon.
scene:lightning(daemon)
sleep(400)
scene:flamestrike(daemon)
sleep(400)
scene:lightning(daemon)
daemon:play_sound(SOUND.DAEMON_ROAR)
sleep(600)

daemon:say("This is not over, mortal...", { pause = 1500, color = COLOR_VILLAIN })
sleep(500)

-- Daemon vanishes in a final flamestrike.
scene:flamestrike(daemon)
sleep(300)
daemon:remove()
sleep(500)

-- Clean up the summoning props.
pentagram:remove()
brazier_l:remove()
brazier_r:remove()

-- ── Act 5: Resolution ───────────────────────────────────────────────────

log("--- Act 5: Resolution ---")

scene:set_weather("none")
scene:fade_light(LIGHT_DARK, LIGHT_DAWN, 2000)

sage:say("Let that be a lesson.", { pause = 2000 })
sage:bow()
sleep(1000)

-- ── Act 6: Guards arrive — using spawn_line with body preset ────────────

log("--- Act 6: Guards arrive ---")

-- Spawn a line of 3 guards to the west, facing east.
local guards = scene:spawn_line({
    count = 3,
    template = {
        body   = BODY.MALE,
        outfit = OUTFITS.CHAIN_GUARD:clone(),
    },
    from  = {-8, -1, 0},
    to    = {-8, 1, 0},
    names = { "Guard Alric", "Guard Beren", "Guard Cedric" },
    face  = "e",
    group = "guards",
})
sleep(500)

sage:say("Guards! Remove this ruffian.", { pause = 1000 })

-- All guards march to the thief's body in parallel.
guards:walk_to({2, 2}, { speed = 200 })
sleep(500)

guards:say("Aye, captain!")
sleep(1000)

-- Guards walk back in parallel.
local guard_list = guards:actors()
scene:parallel(
    function() guard_list[1]:walk_to({-8, -1}, { speed = 200 }) end,
    function() guard_list[2]:walk_to({-8,  0}, { speed = 200 }) end,
    function() guard_list[3]:walk_to({-8,  1}, { speed = 200 }) end
)

-- ── Epilogue ────────────────────────────────────────────────────────────

log("--- Epilogue ---")

sage:say("Peace is restored.", { pause = 2000 })
sage:bow()
sleep(1000)

scene:cleanup()
scene:set_light(0x00)

log("=== Scene demo finished ===")
