-- notre_dame.lua: "Notre-Dame de Paris" — a theatrical production
-- inspired by Victor Hugo's novel and the musical adaptation.
--
-- Usage:
--   demo-server --log world.uolog --lua-script scripts/notre_dame.lua
--
-- To play a single act (for testing):
--   Set PLAY_ACT below to the act number (0 = prologue, 1–6, 7 = epilogue,
--   8 = curtain call), or set PLAY_FROM to start from that act onward.
--
-- Structure:
--   1. Configuration (coordinates, colors)
--   2. Character definitions (CAST table)
--   3. Set dressing functions (cathedral, square, dungeon, siege, tower)
--   4. Stage helpers (transitions, enter/exit, prop management)
--   5. Acts (each is a self-contained function)
--   6. Runner (plays all or selected acts)

dofile("scripts/scene.lua")

-- ═══════════════════════════════════════════════════════════════════════
-- 1. Configuration
-- ═══════════════════════════════════════════════════════════════════════

local MAP_ID = 0

-- Play control: set to nil to play everything, or a number to test one act.
-- 0=prologue, 1–6=acts, 7=epilogue, 8=curtain_call
local PLAY_ACT  = nil   -- e.g. 4 to test only Act 4
local PLAY_FROM = nil   -- e.g. 3 to play from Act 3 onward

-- Anchor at center of the stage.
local STAGE_CENTER_X = 1453
local STAGE_CENTER_Y = 1580
local STAGE_Z        = 30

-- Backstage entry points (actors enter/exit here)
local WINGS_L   = {-7, 0, -8}   -- stage left (west, through staircase room)
local WINGS_R   = {5, 1, 0}     -- stage right (east)
local UPSTAGE   = {0, -3, 0}    -- back of stage (north)
local DOWNSTAGE = {0, 4, 0}     -- front of stage (south, toward audience)

-- Key stage positions
local POS = {
    CENTER      = {0, 0, 0},
    THRONE      = {0, -2, 0},    -- back-center (Frollo's seat / altar)
    LEFT        = {-3, 1, 0},
    RIGHT       = {2, 1, 0},
    FRONT_LEFT  = {-3, 3, 0},
    FRONT_RIGHT = {3, 3, 0},
    BACK_LEFT   = {-3, -1, 0},
    BACK_RIGHT  = {2, -1, 0},
}

-- Speech colors
local COLOR = {
    NORMAL   = 0x0058,  -- warm white
    FROLLO   = 0x0455,  -- dark / sinister
    ESME     = 0x0315,  -- green / vibrant
    QUASI    = 0x0481,  -- tan / earthy
    CLOPIN   = 0x0021,  -- red / bold
    PHOEBUS  = 0x0003,  -- blue / noble
    NARRATOR = 0x0044,  -- grey / neutral
    WHISPER  = 0x03B2,  -- faded
}

-- ═══════════════════════════════════════════════════════════════════════
-- 2. Character definitions
-- ═══════════════════════════════════════════════════════════════════════
-- Each character is a table with body/name/outfit that can be passed
-- directly to spawn().  Outfits are generated fresh each time via a
-- factory function so :clone() is not needed by callers.

local QUASI_OUTFIT = Outfit()
    :hair(0x203B, 0x0386)
    :shirt(0x1517, 0x0253)
    :pants(0x152E, 0x0253)

local CAST = {}

CAST.gringoire = {
    body   = BODY.MALE,
    name   = "Gringoire",
    outfit = function() return OUTFITS.POET:clone() end,
    color  = COLOR.NARRATOR,
}

CAST.frollo = {
    body   = BODY.MALE_LIGHT,
    name   = "Frollo",
    outfit = function() return OUTFITS.PRIEST:clone() end,
    color  = COLOR.FROLLO,
}

CAST.frollo_armed = {
    body   = BODY.MALE_LIGHT,
    name   = "Frollo",
    outfit = function() return OUTFITS.PRIEST:clone():right_hand(0x0F52) end,
    color  = COLOR.FROLLO,
}

CAST.esmeralda = {
    body   = BODY.FEMALE_TAN,
    name   = "Esmeralda",
    outfit = function() return OUTFITS.GYPSY:clone() end,
    color  = COLOR.ESME,
}

CAST.quasimodo = {
    body   = BODY.MALE_TAN,
    name   = "Quasimodo",
    outfit = function() return QUASI_OUTFIT:clone() end,
    color  = COLOR.QUASI,
}

CAST.phoebus = {
    body   = BODY.MALE_LIGHT,
    name   = "Phoebus",
    outfit = function() return OUTFITS.PLATE_GUARD:clone():cloak(0x1515, 0x0455) end,
    color  = COLOR.PHOEBUS,
}

CAST.clopin = {
    body   = BODY.MALE_DARK,
    name   = "Clopin",
    outfit = function() return OUTFITS.ROGUE:clone():cloak(0x1515, 0x0021) end,
    color  = COLOR.CLOPIN,
}

CAST.guard = {
    body   = BODY.MALE,
    outfit = function() return OUTFITS.CHAIN_GUARD:clone() end,
}

CAST.beggar = {
    body   = BODY.MALE,
    outfit = function() return OUTFITS.BEGGAR:clone() end,
}

CAST.rebel = {
    body   = BODY.MALE,
    outfit = function() return OUTFITS.BEGGAR:clone():right_hand(0x0F52) end,
}

-- ═══════════════════════════════════════════════════════════════════════
-- 3. Set dressing (reusable decoration layouts)
-- ═══════════════════════════════════════════════════════════════════════
-- Each returns a list of props (for tracking / removal).

local function dress_cathedral(add)
    add(PROPS.CANDELABRA, {-3, -2, 0})
    add(PROPS.CANDELABRA, {2, -2, 0})
    add(PROPS.ALTAR,      POS.THRONE)
    add(PROPS.ANKH,       {0, -3, 0})
    add(PROPS.TORCH_WALL, {-4, -3, 0})
    add(PROPS.TORCH_WALL, {4, -3, 0})
    add(PROPS.CANDLE,     {-2, -2, 0})
    add(PROPS.CANDLE,     {2, -2, 0})
end

local function dress_cathedral_bell(add)
    dress_cathedral(add)
    add(PROPS.BELL, {0, -3, 0})
end

local function dress_square(add)
    add(PROPS.BARREL,      {-4, 2, 0})
    add(PROPS.BARREL,      {4, 2, 0})
    add(PROPS.HAY_BALE,    {-4, -1, 0})
    add(PROPS.CRATE_SMALL, {5, -1, 0})
    add(PROPS.BANNER_RED,  {-2, -3, 0})
    add(PROPS.BANNER_BLUE, {2, -3, 0})
    add(PROPS.TORCH,       {-4, -3, 0})
    add(PROPS.TORCH,       {4, -3, 0})
    add(PROPS.CAMPFIRE,    {0, 2, 0})
end

local function dress_dungeon(add)
    add(PROPS.CAULDRON,       {0, 0, 0})
    add(PROPS.BARREL,         {-3, -2, 0})
    add(PROPS.BARREL,         {3, 1, 0})
    add(PROPS.SACK,           {-2, 2, 0})
    add(PROPS.SACK,           {4, -1, 0})
    add(PROPS.CRATE_SMALL,    {-4, 1, 0})
    add(PROPS.TORCH,          {-4, -3, 0})
    add(PROPS.TORCH,          {4, -3, 0})
    add(PROPS.CAMPFIRE_SMALL, {-2, 0, 0})
    add(PROPS.SKULL,          {3, -2, 0})
    add(PROPS.BONES_PILE,     {-3, 2, 0})
end

local function dress_siege(add)
    add(PROPS.TORCH, {-4, -3, 0})
    add(PROPS.TORCH, {4, -3, 0})
    add(PROPS.TORCH, {-4, 3, 0})
    add(PROPS.TORCH, {4, 3, 0})
    add(PROPS.ALTAR, POS.THRONE)
    add(PROPS.ANKH,  {0, -3, 0})
end

local function dress_tower(add)
    add(PROPS.TORCH, {-4, -3, 0})
    add(PROPS.TORCH, {4, -3, 0})
    add(PROPS.BELL,  {0, -3, 0})
    add(PROPS.ANKH,  {-2, -3, 0})
end

local function dress_night(add)
    add(PROPS.TORCH,   {-4, 0, 0})
    add(PROPS.TORCH,   {4, 0, 0})
    add(PROPS.LANTERN, {0, 3, 0})
end

-- ═══════════════════════════════════════════════════════════════════════
-- Create the scene + stage helpers
-- ═══════════════════════════════════════════════════════════════════════

local w = World(MAP_ID)
local scene = Scene(w)
scene:set_anchor(STAGE_CENTER_X, STAGE_CENTER_Y, STAGE_Z)

-- ── Lighting helpers ────────────────────────────────────────────────────

local current_light = 0x00

local function blackout(duration_ms)
    scene:fade_light(current_light, 0x1F, 800)
    current_light = 0x1F
    sleep(duration_ms or 2000)
end

local function lights_up(level, fade_ms)
    level = level or 0x04
    scene:fade_light(0x1F, level, fade_ms or 1200)
    current_light = level
end

-- ── Prop management ─────────────────────────────────────────────────────
-- Props are tracked per-set and cleared between acts.

local stage_props = {}

local function add_prop(preset, pos, color)
    local p = scene:spawn_prop(preset, pos, color)
    table.insert(stage_props, p)
    return p
end

local function clear_stage()
    for _, p in ipairs(stage_props) do
        p:remove()
    end
    stage_props = {}
end

--- Apply a set dressing function (pass add_prop automatically).
local function dress(layout_fn)
    layout_fn(add_prop)
end

-- ── Actor spawn / lifecycle helpers ─────────────────────────────────────

--- Spawn an actor from a CAST definition.
--- spawn(CAST.frollo, WINGS_L, "e")
--- spawn(CAST.frollo_armed, POS.RIGHT, "w")
local function spawn(def, at, direction)
    return scene:spawn_actor({
        body      = def.body,
        name      = def.name,
        at        = at,
        direction = direction or "s",
        outfit    = def.outfit(),
    })
end

--- Spawn a line of actors from a CAST definition.
--- spawn_guards(3, WINGS_L, {-6, 2, 0}, "e", names, group_name)
local function spawn_squad(def, count, from, to, face, names, group_name)
    return scene:spawn_line({
        count    = count,
        template = { body = def.body, outfit = def.outfit() },
        from     = from,
        to       = to,
        names    = names,
        face     = face,
        group    = group_name,
    })
end

--- Actor enters: spawn at wings, walk to position.
local function enter(def, wings, target, opts)
    opts = opts or {}
    local dir = wings == WINGS_L and "e" or "w"
    local actor = spawn(def, wings, dir)
    sleep(opts.spawn_delay or 300)
    if opts.run then
        actor:run_to(target, { speed = opts.speed or 150 })
    else
        actor:walk_to(target, { speed = opts.speed or 200 })
    end
    sleep(opts.arrive_delay or 200)
    return actor
end

--- Actor exits to wings and is removed from the world.
local function exit(actor, wings, opts)
    opts = opts or {}
    actor:walk_to(wings, { speed = opts.speed or 200 })
    actor:remove()
end

--- Exit multiple actors in parallel, then remove them all.
local function exit_all(actors_and_wings, opts)
    opts = opts or {}
    local speed = opts.speed or 200
    local fns = {}
    for _, pair in ipairs(actors_and_wings) do
        local actor, wings = pair[1], pair[2]
        table.insert(fns, function()
            actor:walk_to(wings, { speed = speed })
        end)
    end
    scene:parallel(table.unpack(fns))
    for _, pair in ipairs(actors_and_wings) do
        pair[1]:remove()
    end
end

--- Exit all actors in a group to a given wings position, then remove.
local function exit_group(group, wings, opts)
    opts = opts or {}
    local actors = group:actors()
    local pairs = {}
    for _, a in ipairs(actors) do
        table.insert(pairs, { a, wings })
    end
    exit_all(pairs, opts)
end

--- Dialogue shortcut: actor says lines with character color.
--- talk(actor, CAST.frollo, { "Line 1", "Line 2" }, pause_ms)
local function talk(actor, def, lines, pause)
    pause = pause or 2500
    for _, line in ipairs(lines) do
        actor:say(line, { pause = pause, color = def.color })
    end
end

--- Single line shortcut.
local function say(actor, def, line, pause)
    actor:say(line, { pause = pause or 2500, color = def.color })
end

--- Whisper (faded color).
local function whisper(actor, line, pause)
    actor:say(line, { pause = pause or 2500, color = COLOR.WHISPER })
end

-- ═══════════════════════════════════════════════════════════════════════
-- 5. The Acts
-- ═══════════════════════════════════════════════════════════════════════
-- Each act is a function that receives no arguments and manages its own
-- actors/props.  Acts call clear_stage() + blackout/lights_up as needed.

-- ── Prologue: The Age of the Cathedrals ─────────────────────────────────

local function prologue()
    log("--- Prologue: The Age of the Cathedrals ---")

    scene:set_light(0x0A)
    current_light = 0x0A
    scene:play_music(MUSIC.TEMPLE)

    dress(dress_cathedral)
    sleep(1000)

    -- Gringoire the narrator enters
    local gringoire = enter(CAST.gringoire, WINGS_L, DOWNSTAGE, { speed = 250 })
    gringoire:face("s")
    sleep(300)
    gringoire:bow()
    sleep(800)

    talk(gringoire, CAST.gringoire, {
        "Good people of Britain, lend me your ears!",
        "I shall tell you a tale of stone and fire...",
        "Of a cathedral that reached for heaven...",
        "And the souls who were broken beneath its shadow.",
    }, 2500)

    -- Introduce the cast: each enters, bows, moves to position
    local function introduce(def, intro_line, wings, rest_pos, emote)
        say(gringoire, CAST.gringoire, intro_line, 1500)
        local actor = enter(def, wings, POS.CENTER)
        actor:face("s")
        if emote == "salute" then
            actor:salute()
        else
            actor:bow()
        end
        sleep(800)
        actor:walk_to(rest_pos, { speed = 400 })
        actor:face(rest_pos == POS.RIGHT and "w" or "e")
        sleep(300)
        return actor
    end

    local frollo    = introduce(CAST.frollo,    "The Archdeacon Claude Frollo... a man of God and shadow.", WINGS_L, POS.BACK_LEFT)
    local esmeralda = introduce(CAST.esmeralda, "The beautiful Esmeralda... a gypsy with fire in her heart.", WINGS_R, POS.BACK_RIGHT)
    local quasimodo = introduce(CAST.quasimodo, "Quasimodo... the bell-ringer of Notre-Dame, shunned by all.", WINGS_L, POS.LEFT)
    local phoebus   = introduce(CAST.phoebus,   "Captain Phoebus... the golden soldier.", WINGS_R, POS.RIGHT, "salute")
    local clopin    = introduce(CAST.clopin,    "And Clopin... king of the outcasts.", WINGS_L, POS.FRONT_LEFT)

    talk(gringoire, CAST.gringoire, {
        "And so our story begins...",
        "At the Festival of Fools!",
    }, 2000)

    -- Clear stage
    exit_all({
        {gringoire, WINGS_L}, {frollo, WINGS_L}, {esmeralda, WINGS_R},
        {quasimodo, WINGS_L}, {phoebus, WINGS_R}, {clopin, WINGS_L},
    })
    clear_stage()
end

-- ── Act 1: The Festival of Fools ────────────────────────────────────────

local function act1()
    blackout(2000)
    log("--- Act 1: The Festival of Fools ---")

    dress(dress_square)
    scene:play_music(MUSIC.TAVERN)
    lights_up(0x02, 1500)
    sleep(500)

    -- The crowd
    local crowd = scene:spawn_group({
        template = { body = BODY.MALE, outfit = OUTFITS.BEGGAR:clone() },
        positions = { {-2, 1, 0}, {1, 2, 0}, {3, 0, 0}, {-1, -1, 0} },
        names = { "Beggar", "Vagabond", "Urchin", "Peasant" },
        face = "s", group = "crowd",
    })
    sleep(500)

    local townwoman = scene:spawn_actor({
        body = BODY.FEMALE, name = "Townwoman",
        at = {2, -1, 0}, direction = "s",
        outfit = OUTFITS.PEASANT:clone():skirt(0x1537, 0x0384),
    })
    sleep(300)

    -- Clopin: master of ceremonies
    local clopin = enter(CAST.clopin, WINGS_L, POS.CENTER)
    clopin:face("s")
    sleep(300)

    talk(clopin, CAST.clopin, {
        "People of Paris! Welcome to the Festival of Fools!",
        "Today we crown the King of Fools!",
    })
    clopin:animate(ANIM.FIDGET_1, 5)
    sleep(500)

    crowd:say("Hurrah!")
    sleep(1500)

    -- Esmeralda dances
    local esmeralda = spawn(CAST.esmeralda, WINGS_R, "w")
    sleep(300)
    scene:sound(SOUND.COINS, esmeralda)
    esmeralda:walk_route({
        { 3, 2 },
        { 1, 0,  on_arrive = function(a) a:animate(ANIM.FIDGET_2, 5); sleep(600) end },
        { -1, 1, on_arrive = function(a) a:animate(ANIM.FIDGET_1, 5); sleep(600) end },
        { 0, 1 },
    }, { speed = 300 })

    townwoman:say("Look at her dance!", { pause = 1500, color = COLOR.NORMAL })

    -- Quasimodo watches from the shadows
    local quasimodo = spawn(CAST.quasimodo, {-4, -3, 0}, "se")
    sleep(500)
    whisper(quasimodo, "Beautiful...", 2000)

    -- Frollo appears
    local frollo = spawn(CAST.frollo, {4, -3, 0}, "sw")
    sleep(500)
    say(frollo, CAST.frollo, "Witchcraft... This is the devil's work.", 2500)
    sleep(500)

    frollo:face_towards({-4, -3})
    sleep(300)
    say(frollo, CAST.frollo, "Quasimodo! Return to the bell tower at once!")
    say(quasimodo, CAST.quasimodo, "Yes, master...", 1500)
    exit(quasimodo, WINGS_L, { speed = 350 })

    -- Clopin taunts
    clopin:face_towards({4, -3})
    sleep(200)
    say(clopin, CAST.clopin, "The Archdeacon graces us! How rare!")
    say(frollo, CAST.frollo, "This festival is an abomination.")

    frollo:face_towards({0, 1})
    sleep(500)
    whisper(frollo, "That gypsy girl... She will be the ruin of us all.", 2500)

    sleep(1000)
    say(clopin, CAST.clopin, "The festival continues! Drink and be merry!", 1500)

    exit_all({
        {clopin, WINGS_L}, {esmeralda, WINGS_R},
        {frollo, WINGS_L}, {townwoman, WINGS_R},
    })
    crowd:remove()
    clear_stage()
end

-- ── Act 2: The Abduction ────────────────────────────────────────────────

local function act2()
    blackout(2500)
    log("--- Act 2: The Abduction ---")

    dress(dress_night)
    scene:play_music(MUSIC.APPROACH)
    lights_up(0x10, 1500)
    sleep(500)

    -- Esmeralda alone
    local esmeralda = spawn(CAST.esmeralda, POS.CENTER, "s")
    sleep(500)
    say(esmeralda, CAST.esmeralda, "The stars are so bright tonight...", 2500)
    esmeralda:animate(ANIM.FIDGET_2, 5)
    sleep(800)

    -- Guards creep in
    scene:sound(SOUND.SNEAK, esmeralda)
    sleep(500)

    local guards = spawn_squad(CAST.guard, 3, WINGS_L, {-6, 2, 0}, "e",
        { "Guard", "Guard", "Guard" }, "frollo_guards")
    sleep(500)

    local frollo = enter(CAST.frollo, WINGS_L, POS.BACK_LEFT, { speed = 250 })
    say(frollo, CAST.frollo, "Seize the gypsy witch!", 2000)
    say(frollo, CAST.frollo, "She has bewitched the people with her sorcery!", 2500)

    guards:walk_to(POS.CENTER, { speed = 200 })
    sleep(300)

    say(esmeralda, CAST.esmeralda, "No! Leave me alone!", 1500)
    esmeralda:walk_to(POS.FRONT_RIGHT, { speed = 150 })
    sleep(300)

    -- Phoebus bursts in
    local phoebus = spawn(CAST.phoebus, WINGS_R, "w")
    scene:sound(SOUND.DOOR_OPEN, phoebus)
    sleep(300)
    phoebus:run_to(POS.RIGHT, { speed = 150 })
    sleep(200)

    say(phoebus, CAST.phoebus, "HALT!", 1500)
    phoebus:animate(ANIM.SLASH_1H, 7)
    scene:sound(SOUND.WEAPON_SWOOSH, phoebus)
    sleep(500)

    say(phoebus, CAST.phoebus, "By whose authority do you seize this woman?", 2500)

    frollo:face_towards(POS.RIGHT)
    sleep(200)
    say(frollo, CAST.frollo, "By the authority of the Church, Captain.", 2500)

    talk(phoebus, CAST.phoebus, {
        "The Church does not command the King's soldiers.",
        "Release her. Now.",
    }, 2000)

    sleep(1000)
    say(frollo, CAST.frollo, "You will regret this, Captain...", 2000)

    -- Guards + Frollo retreat
    exit_group(guards, WINGS_L)
    exit(frollo, WINGS_L, { speed = 300 })
    sleep(500)

    -- Tender moment
    phoebus:walk_to({1, 2, 0}, { speed = 200 })
    phoebus:face_towards(POS.FRONT_RIGHT)
    sleep(300)

    say(phoebus, CAST.phoebus, "Are you hurt, mademoiselle?", 2000)
    esmeralda:face_towards({1, 2})
    sleep(200)
    say(esmeralda, CAST.esmeralda, "No... Thank you, Captain.", 2000)
    say(phoebus, CAST.phoebus, "Phoebus. My name is Phoebus.", 2000)
    say(esmeralda, CAST.esmeralda, "Phoebus... Like the sun god.", 2000)
    sleep(1000)

    exit_all({ {phoebus, WINGS_R}, {esmeralda, WINGS_R} })
    clear_stage()
end

-- ── Act 3: The Court of Miracles ────────────────────────────────────────

local function act3()
    blackout(2500)
    log("--- Act 3: The Court of Miracles ---")

    dress(dress_dungeon)
    scene:play_music(MUSIC.DUNGEON)
    lights_up(0x12, 1200)
    sleep(500)

    -- Clopin holds court
    local clopin = spawn(CAST.clopin, {0, -2, 0}, "s")
    sleep(300)

    local vagabonds = scene:spawn_group({
        template = { body = BODY.MALE, outfit = OUTFITS.BEGGAR:clone() },
        positions = { {-2, -1, 0}, {2, -1, 0}, {-1, 1, 0}, {1, 1, 0}, {3, 0, 0} },
        names = { "Trouillefou", "Bellevigne", "Clochard", "Gueux", "Truand" },
        face = "s", group = "vagabonds",
    })

    local beggar_woman = scene:spawn_actor({
        body = BODY.FEMALE, name = "Mirette",
        at = {-3, 0, 0}, direction = "e",
        outfit = OUTFITS.BEGGAR:clone():skirt(0x1537, 0x0253),
    })
    sleep(500)

    talk(clopin, CAST.clopin, {
        "Brothers! Sisters of the Court of Miracles!",
    }, 3000)
    clopin:animate(ANIM.FIDGET_1, 5)
    sleep(300)
    talk(clopin, CAST.clopin, {
        "The archdeacon Frollo hunts our people in the streets!",
        "He calls us vermin... thieves... heretics!",
    }, 2500)

    vagabonds:say("Down with Frollo!")
    sleep(1500)

    -- Gringoire stumbles in
    local gringoire = enter(CAST.gringoire, WINGS_R, {2, 0, 0}, { speed = 250 })
    clopin:face_towards({2, 0})
    sleep(200)

    say(clopin, CAST.clopin, "What is this? A spy in our midst!", 2500)
    say(gringoire, CAST.gringoire, "N-no! I am merely a poet! I am lost!", 2000)
    say(clopin, CAST.clopin, "A poet? Even worse! String him up!", 2000)

    vagabonds:walk_to({2, 0, 0}, { speed = 250 })
    sleep(500)
    say(gringoire, CAST.gringoire, "Please! I beg you! Mercy!", 2000)

    -- Esmeralda intervenes
    local esmeralda = enter(CAST.esmeralda, WINGS_L, {0, 1, 0})

    talk(esmeralda, CAST.esmeralda, {
        "Wait, Clopin!",
        "This man is no spy. I will vouch for him.",
    }, 1500)

    clopin:face_towards(POS.CENTER)
    sleep(200)
    say(clopin, CAST.clopin, "You take responsibility for this fool, Esmeralda?", 2500)
    say(esmeralda, CAST.esmeralda, "I do.", 1500)
    say(clopin, CAST.clopin, "Very well. He lives... for now.", 2000)
    say(gringoire, CAST.gringoire, "Thank you... I owe you my life.", 2000)
    sleep(1000)

    exit_all({
        {clopin, WINGS_L}, {gringoire, WINGS_R},
        {esmeralda, WINGS_L}, {beggar_woman, WINGS_L},
    })
    vagabonds:remove()
    clear_stage()
end

-- ── Act 4: Sanctuary ────────────────────────────────────────────────────

local function act4()
    blackout(2500)
    log("--- Act 4: Sanctuary ---")

    dress(dress_cathedral_bell)
    scene:play_music(MUSIC.SADNESS)
    lights_up(0x0C, 1500)
    sleep(500)

    -- Esmeralda runs in, pursued
    local esmeralda = enter(CAST.esmeralda, WINGS_R, POS.CENTER, { run = true, speed = 150 })
    say(esmeralda, CAST.esmeralda, "Someone help me! Please!", 1500)

    -- Guards chase
    local guards = spawn_squad(CAST.guard, 3, WINGS_R, {6, 2, 0}, "w",
        { "Guard", "Guard", "Guard" }, "chase_guards")
    sleep(300)
    guards:walk_to(POS.RIGHT, { speed = 200 })
    sleep(300)

    -- Frollo follows
    local frollo = enter(CAST.frollo, WINGS_R, POS.FRONT_RIGHT, { speed = 250 })
    say(frollo, CAST.frollo, "There is nowhere left to run, gypsy.", 2500)

    -- QUASIMODO!
    scene:sound(SOUND.THUNDER, esmeralda)
    sleep(300)
    local quasimodo = spawn(CAST.quasimodo, UPSTAGE, "s")
    sleep(200)
    quasimodo:run_to(POS.CENTER, { speed = 150 })
    sleep(200)

    -- The iconic moment
    say(quasimodo, CAST.quasimodo, "SANCTUARY!!!", 500)
    say(quasimodo, CAST.quasimodo, "SANCTUARY!!!", 500)
    say(quasimodo, CAST.quasimodo, "SANCTUARY!!!", 2000)

    -- Holy protection
    scene:heal(esmeralda)
    scene:sound(SOUND.BLESS, esmeralda)
    sleep(500)
    scene:heal(esmeralda)
    sleep(500)
    scene:effect_at(esmeralda, EFFECT.HEAL, { duration = 50 })
    sleep(300)

    -- Guards halt
    local gl = guards:actors()
    gl[1]:say("The bell-ringer invokes sanctuary!", { pause = 2000, color = COLOR.NORMAL })
    gl[2]:say("We cannot enter the cathedral by force...", { pause = 2000, color = COLOR.NORMAL })

    frollo:face_towards(POS.CENTER)
    sleep(300)
    say(frollo, CAST.frollo, "She cannot hide in there forever.", 2500)
    whisper(frollo, "Mark my words... she will burn.", 2500)

    -- Guards + Frollo withdraw
    exit_group(guards, WINGS_R)
    exit(frollo, WINGS_R, { speed = 300 })
    sleep(500)

    -- Quiet moment
    scene:play_music(MUSIC.TEMPLE)
    quasimodo:face_towards(POS.CENTER)
    esmeralda:face_towards(UPSTAGE)
    sleep(300)

    say(esmeralda, CAST.esmeralda, "Why... why did you save me?", 2500)
    say(quasimodo, CAST.quasimodo, "Because...", 1500)
    say(quasimodo, CAST.quasimodo, "You are the only person who was ever kind to me.", 3000)

    say(esmeralda, CAST.esmeralda, "This cathedral is beautiful.", 2000)
    say(quasimodo, CAST.quasimodo, "I will show you the bells. They are my only friends.", 2500)
    say(quasimodo, CAST.quasimodo, "The great bell... I call her Marie.", 2500)

    scene:sound(SOUND.ANVIL_STRIKE, quasimodo)
    sleep(1000)

    say(esmeralda, CAST.esmeralda, "It is wonderful, Quasimodo.", 2000)
    say(quasimodo, CAST.quasimodo, "No one has ever said my name... like that before.", 3000)
    sleep(1500)

    exit_all({ {quasimodo, UPSTAGE}, {esmeralda, POS.LEFT} })
    clear_stage()
end

-- ── Act 5: Fire and Siege ───────────────────────────────────────────────

local function act5()
    blackout(3000)
    log("--- Act 5: Fire and Siege ---")

    dress(dress_siege)
    scene:play_music(MUSIC.COMBAT)
    lights_up(0x08, 1000)
    sleep(500)

    -- Frollo with soldiers
    local frollo = enter(CAST.frollo_armed, WINGS_R, POS.FRONT_RIGHT, { speed = 250 })

    local guards = spawn_squad(CAST.guard, 4, {6, -1, 0}, {6, 2, 0}, "w",
        { "Soldier", "Soldier", "Soldier", "Soldier" }, "siege_guards")
    sleep(500)

    talk(frollo, CAST.frollo, {
        "If the witch will not come out...",
        "Then BURN the cathedral to the ground!",
    }, 2500)

    -- Storm + Fire
    scene:set_weather("storm", 80)
    sleep(500)

    local fire1 = add_prop(PROPS.CAMPFIRE, {-3, 2, 0})
    local fire2 = add_prop(PROPS.CAMPFIRE, {3, 2, 0})
    sleep(300)

    scene:effect_at(fire1, EFFECT.FLAMESTRIKE)
    scene:sound(SOUND.FLAMESTRIKE, fire1)
    sleep(400)
    scene:effect_at(fire2, EFFECT.FLAMESTRIKE)
    scene:sound(SOUND.FLAMESTRIKE, fire2)
    sleep(400)

    add_prop(PROPS.CAMPFIRE, {0, 3, 0})
    sleep(500)

    -- Guards advance
    guards:walk_to(POS.CENTER, { speed = 200 })
    sleep(300)

    -- Quasimodo defends
    local quasimodo = spawn(CAST.quasimodo, UPSTAGE, "s")
    sleep(300)
    say(quasimodo, CAST.quasimodo, "LEAVE HER ALONE!", 1500)
    quasimodo:animate(ANIM.CAST, 7)
    sleep(300)

    -- Molten lead!
    local gl = guards:actors()
    scene:lightning(gl[1])
    sleep(300)
    scene:flamestrike(gl[2])
    gl[2]:play_sound(SOUND.FLAMESTRIKE)
    sleep(300)

    gl[1]:animate(ANIM.GET_HIT, 5)
    gl[1]:say("Molten lead! Fall back!", { pause = 1500, color = COLOR.NORMAL })
    sleep(300)
    gl[2]:animate(ANIM.DIE_FORWARD, 5)
    sleep(500)

    -- Clopin counter-attacks
    local clopin = spawn(CAST.clopin, WINGS_L, "e")
    sleep(200)
    say(clopin, CAST.clopin, "For the Court of Miracles! ATTACK!", 2000)

    local rebels = spawn_squad(CAST.rebel, 3, {-6, 0, 0}, {-6, 2, 0}, "e",
        { "Rebel", "Rebel", "Rebel" }, "rebels")
    sleep(300)

    -- Parallel battle
    scene:parallel(
        function()
            clopin:run_to(POS.LEFT, { speed = 150 })
            clopin:animate(ANIM.SLASH_1H, 7)
            clopin:play_sound(SOUND.SWORD_HIT)
        end,
        function()
            rebels:walk_to(POS.CENTER, { speed = 200 })
        end,
        function()
            gl[3]:face("w"); sleep(500)
            gl[3]:animate(ANIM.SLASH_1H, 7)
            gl[3]:play_sound(SOUND.SWORD_HIT)
            sleep(400)
            gl[3]:animate(ANIM.GET_HIT, 5)
        end,
        function()
            gl[4]:face("w"); sleep(300)
            gl[4]:animate(ANIM.SWING_2H, 7)
            gl[4]:play_sound(SOUND.AXE_HIT)
            sleep(600)
            gl[4]:animate(ANIM.DIE_BACKWARD, 5)
        end
    )
    sleep(500)

    scene:lightning(gl[3])
    scene:sound(SOUND.THUNDER, gl[3])
    sleep(400)
    gl[3]:animate(ANIM.DIE_FORWARD, 5)
    sleep(500)

    say(frollo, CAST.frollo, "Fall back! Regroup!", 1500)
    whisper(frollo, "I will deal with the monster myself...", 2000)
    sleep(1000)

    -- Cleanup battlefield
    exit_all({ {clopin, WINGS_L} })
    exit_group(rebels, WINGS_L)

    scene:set_weather("none")
    guards:remove()
    quasimodo:remove()
    frollo:remove()
    clear_stage()
end

-- ── Act 6: Finale ───────────────────────────────────────────────────────

local function act6()
    blackout(3000)
    log("--- Act 6: Finale ---")

    dress(dress_tower)
    scene:play_music(MUSIC.APPROACH)
    lights_up(0x0E, 1500)
    sleep(500)

    -- Esmeralda weakened
    local esmeralda = spawn(CAST.esmeralda, POS.CENTER, "s")
    sleep(300)
    say(esmeralda, CAST.esmeralda, "The smoke... I can barely breathe...", 2500)

    -- Quasimodo protecting her
    local quasimodo = spawn(CAST.quasimodo, {-1, 0, 0}, "s")
    sleep(300)
    say(quasimodo, CAST.quasimodo, "Stay strong, Esmeralda. I will protect you.", 2500)
    sleep(500)

    -- Frollo sneaks in
    local frollo = spawn(CAST.frollo_armed, WINGS_R, "w")
    sleep(200)
    scene:sound(SOUND.SNEAK, frollo)
    frollo:walk_to(POS.RIGHT, { speed = 300 })
    sleep(300)

    talk(frollo, CAST.frollo, {
        "She is a witch, Quasimodo.",
        "She has corrupted your mind. She must die.",
    }, 2500)

    quasimodo:face_towards(POS.RIGHT)
    sleep(300)

    talk(quasimodo, CAST.quasimodo, {
        "All my life you told me the world was dark and cruel.",
        "But now I see that the only thing dark and cruel...",
        "...is you.",
    }, 2500)
    sleep(500)

    -- Frollo attacks!
    scene:play_music(MUSIC.COMBAT)
    say(frollo, CAST.frollo, "Insolent wretch!", 1000)
    frollo:animate(ANIM.SLASH_1H, 7)
    frollo:play_sound(SOUND.WEAPON_SWOOSH)
    sleep(400)

    frollo:walk_to({1, 0, 0}, { speed = 150 })

    -- Strikes Esmeralda
    scene:effect_at(esmeralda, EFFECT.FIRE_BALL_SMALL)
    esmeralda:play_sound(SOUND.POISON)
    sleep(300)
    esmeralda:animate(ANIM.GET_HIT, 5)
    esmeralda:say("Ah...!", { pause = 1000, color = COLOR.ESME })

    -- Quasimodo fights back
    quasimodo:face_towards({1, 0})
    sleep(200)
    quasimodo:animate(ANIM.SWING_2H, 5)
    quasimodo:play_sound(SOUND.FIST_HIT)
    sleep(400)

    frollo:animate(ANIM.GET_HIT, 5)
    sleep(300)

    quasimodo:walk_to({1, 0, 0}, { speed = 150 })
    quasimodo:animate(ANIM.SWING_2H, 7)
    quasimodo:play_sound(SOUND.MACE_HIT)
    sleep(400)

    -- Frollo falls
    frollo:animate(ANIM.GET_HIT, 5)
    sleep(200)
    scene:lightning(frollo)
    scene:sound(SOUND.THUNDER, frollo)
    sleep(400)

    say(frollo, CAST.frollo, "No...! This cannot be...!", 1500)
    frollo:animate(ANIM.DIE_BACKWARD, 7)
    sleep(500)
    scene:flamestrike(frollo)
    frollo:play_sound(SOUND.FLAMESTRIKE)
    sleep(600)
    frollo:kill()
    sleep(1000)

    -- Tragic ending
    scene:play_music(MUSIC.DEATH)
    quasimodo:walk_to(POS.CENTER, { speed = 200 })
    quasimodo:face_towards(POS.CENTER)
    sleep(300)

    say(esmeralda, CAST.esmeralda, "Quasimodo...", 2000)
    whisper(esmeralda, "The bells... they are so beautiful...", 3000)
    esmeralda:animate(ANIM.DIE_FORWARD, 7)
    sleep(1000)
    esmeralda:kill()
    sleep(1000)

    say(quasimodo, CAST.quasimodo, "No... NO!", 2000)
    sleep(500)
    say(quasimodo, CAST.quasimodo, "Esmeralda...!", 3000)

    -- Bell tolls
    for i = 1, 3 do
        scene:sound(SOUND.ANVIL_STRIKE, quasimodo)
        sleep(1500)
    end
    sleep(500)

    -- Stash quasimodo for epilogue re-use (don't remove yet)
    return quasimodo
end

-- ── Epilogue ────────────────────────────────────────────────────────────

local function epilogue(quasimodo_actor)
    log("--- Epilogue ---")

    local gringoire = enter(CAST.gringoire, WINGS_L, DOWNSTAGE, { speed = 250 })
    gringoire:face("s")
    sleep(500)

    talk(gringoire, CAST.gringoire, {
        "And so the bells of Notre-Dame fell silent...",
        "The stone endures. The gargoyles watch.",
        "But the cathedral remembers the ones it sheltered.",
        "And if you listen closely, on a quiet night...",
        "You can still hear the bells.",
    }, 3000)

    scene:sound(SOUND.ANVIL_STRIKE, gringoire)
    sleep(2000)

    if quasimodo_actor then quasimodo_actor:remove() end
    gringoire:remove()
    clear_stage()
end

-- ── Curtain Call ─────────────────────────────────────────────────────────

local function curtain_call()
    log("--- Curtain Call ---")

    sleep(2000)
    scene:play_music(MUSIC.CASTLE)
    lights_up(0x00, 2000)
    sleep(1500)

    local cast_line = scene:spawn_line({
        count = 6,
        template = { body = BODY.MALE },
        from = {-4, 2, 0}, to = {4, 2, 0},
        names = { "Gringoire", "Clopin", "Phoebus", "Esmeralda", "Quasimodo", "Frollo" },
        face = "s", group = "curtain_call",
    })
    sleep(300)

    -- Update appearances
    local ca = cast_line:actors()
    local appearances = {
        { outfit = OUTFITS.POET:clone() },
        { color = 0x044E, outfit = OUTFITS.ROGUE:clone():cloak(0x1515, 0x0021) },
        { color = 0x03EA, outfit = OUTFITS.PLATE_GUARD:clone():cloak(0x1515, 0x0455) },
        { graphic = 0x0191, color = 0x0481, outfit = OUTFITS.GYPSY:clone() },
        { color = 0x0481, outfit = QUASI_OUTFIT:clone() },
        { color = 0x03EA, outfit = OUTFITS.PRIEST:clone() },
    }
    for i, app in ipairs(appearances) do
        ca[i]:update(app)
        sleep(100)
    end
    sleep(1000)

    cast_line:animate(ANIM.BOW, 5)
    sleep(1500)
    cast_line:animate(ANIM.BOW, 5)
    sleep(1500)
    cast_line:animate(ANIM.SALUTE, 5)
    sleep(2000)

    cast_line:remove()
end

-- ═══════════════════════════════════════════════════════════════════════
-- 6. Runner
-- ═══════════════════════════════════════════════════════════════════════
-- Acts are ordered in a list.  PLAY_ACT runs just one; PLAY_FROM runs
-- from that act to the end; nil plays everything.

local acts = {
    [0] = { name = "prologue",     fn = prologue },
    [1] = { name = "act1",         fn = act1 },
    [2] = { name = "act2",         fn = act2 },
    [3] = { name = "act3",         fn = act3 },
    [4] = { name = "act4",         fn = act4 },
    [5] = { name = "act5",         fn = act5 },
    [6] = { name = "act6",         fn = act6 },
    [7] = { name = "epilogue",     fn = function() epilogue(nil) end },
    [8] = { name = "curtain_call", fn = curtain_call },
}

log("=== Notre-Dame de Paris: starting ===")

--- Determine which acts to run.
local function should_run(act_num)
    if PLAY_ACT then return act_num == PLAY_ACT end
    if PLAY_FROM then return act_num >= PLAY_FROM end
    return true
end

-- Special handling: act6 returns quasimodo for epilogue continuity.
local quasimodo_from_act6 = nil

for i = 0, 8 do
    if should_run(i) then
        if i == 6 then
            quasimodo_from_act6 = act6()
        elseif i == 7 then
            epilogue(quasimodo_from_act6)
        else
            acts[i].fn()
        end
    end
end

-- Final cleanup
scene:cleanup()
scene:set_light(0x00)
scene:stop_music()

log("=== Notre-Dame de Paris: finished ===")
