-- musician.lua: A bard NPC that plays melodies on command.
--
-- Usage: demo-server --log world.uolog --lua-script scripts/musician.lua
--
-- A musician stands in place and plays melodies when the player
-- types a command in speech (e.g. "play march", "play greensleeves").
-- Type "play list" to see available melodies.
--
-- Uses the music.lua library for instrument definitions, melody
-- parsing, and playback.

dofile("scripts/scene.lua")
dofile("scripts/melodies.lua")

-- ═════════════════════════════════════════════════════════════════════════
-- 1. Scene setup
-- ═════════════════════════════════════════════════════════════════════════

local MAP_ID  = 0
local SCENE_X = 1419
local SCENE_Y = 1671
local SCENE_Z = 10

local w = World(MAP_ID)
local scene = Scene(w)
scene:set_anchor(SCENE_X, SCENE_Y, SCENE_Z)

-- Spawn the musician
local bard = scene:spawn_actor({
    body      = BODY.MALE_TAN,
    name      = "Bard",
    at        = {0, 0, 0},
    direction = "s",
    outfit    = OUTFITS.POET:clone(),
})
sleep(300)

bard:say("I am the Bard. Say 'play <name>' to hear a tune!", { pause = 2000 })
bard:say("Say 'play list' to see what I know.", { pause = 1500 })

-- ═════════════════════════════════════════════════════════════════════════
-- 2. Playback
-- ═════════════════════════════════════════════════════════════════════════

local playing = false

local function play_tune(melody_key)
    local m = MELODIES[melody_key]
    if not m then return end

    playing = true

    bard:say("Now playing: " .. m.title, { pause = 1000 })
    bard:animate(ANIM.BOW, 5)
    sleep(800)

    play(melody_key, "harp", w, bard, {
        stop = function() return not playing end,
    })

    if playing then
        bard:bow()
        sleep(500)
    end
    playing = false
end

-- ═════════════════════════════════════════════════════════════════════════
-- 3. Command listener
-- ═════════════════════════════════════════════════════════════════════════
-- Listen for speech events via wait_event().  When someone says
-- "play <name>", play that melody.
-- "play list" shows available tunes.  "play stop" interrupts.

log("=== Musician ready — say 'play list' in-game ===")

local list_str = "I know: " .. melody_list()

while true do
    local ev = wait_event(5000)

    if ev and ev.type == "speech" then
        local cmd = ev.message:lower():match("^play%s+(.+)$")
        if cmd then
            cmd = cmd:match("^%s*(.-)%s*$")  -- trim

            if cmd == "list" then
                bard:say(list_str, { pause = 3000 })

            elseif cmd == "stop" then
                playing = false
                bard:say("Stopping.", { pause = 500 })

            elseif MELODIES[cmd] then
                if playing then
                    playing = false
                    sleep(600)
                end
                play_tune(cmd)

            else
                bard:say("I don't know '" .. cmd .. "'. " .. list_str, { pause = 3000 })
            end
        end
    end
end
