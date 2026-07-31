-- music.lua: Musical instrument library and melody playback engine.
--
-- Provides:
--   INSTRUMENTS         table of instrument sound mappings
--   parse_melody()      text notation → note list
--   play_melody()       play a note list with clock-based scheduling
--   melody_duration()   total duration of a note list in ms
--   melody_list()       formatted string of available melodies
--
-- Standalone module — depends only on Lua globals provided by the
-- runtime: sleep(), clock(), log(), and a World object for playback.
--
-- Usage:
--   dofile("scripts/music.lua")
--   dofile("scripts/melodies.lua")   -- optional, loads MELODIES table
--
--   local beat_ms = math.floor(60000 / 120 + 0.5)
--   local notes = parse_melody("C1 D1 E1 F1 | G1-", "harp", beat_ms)
--   play_melody(w, notes, {1419, 1671, 10})

-- ═════════════════════════════════════════════════════════════════════════
-- 1. Instrument definitions
-- ═════════════════════════════════════════════════════════════════════════
--
-- Each instrument maps chromatic note names to UO sound IDs.
-- All instruments share the same 25-note layout:
--   A/As/B  octaves 1-2
--   C-Gs    octaves 0-1  (C also has octave 0 = very low)
--
-- A melody written for one instrument plays on any other — just
-- swap the instrument name.

INSTRUMENTS = {
    harp = {
        name  = "Standing Harp",
        notes = {
            A1  = 0x497, A2  = 0x498,
            As1 = 0x499, As2 = 0x49A,
            B1  = 0x49B, B2  = 0x49C,
            C0  = 0x49D, C1  = 0x49E, C2 = 0x49F,
            Cs0 = 0x4A0, Cs1 = 0x4A1,
            D0  = 0x4A2, D1  = 0x4A3,
            Ds0 = 0x4A4, Ds1 = 0x4A5,
            E0  = 0x4A6, E1  = 0x4A7,
            F0  = 0x4A8, F1  = 0x4A9,
            Fs0 = 0x4AA, Fs1 = 0x4AB,
            G0  = 0x4AC, G1  = 0x4AD,
            Gs0 = 0x4AE, Gs1 = 0x4AF,
        },
    },

    lap_harp = {
        name  = "Lap Harp",
        notes = {
            A1  = 0x3CA, A2  = 0x3CB,
            As1 = 0x3CC, As2 = 0x3CD,
            B1  = 0x3CE, B2  = 0x3CF,
            C0  = 0x3D0, C1  = 0x3D1, C2 = 0x3D2,
            Cs0 = 0x3D3, Cs1 = 0x3D4,
            D0  = 0x3D5, D1  = 0x3D6,
            Ds0 = 0x3D7, Ds1 = 0x3D8,
            E0  = 0x3D9, E1  = 0x3DA,
            F0  = 0x3DB, F1  = 0x3DC,
            Fs0 = 0x3DD, Fs1 = 0x3DE,
            G0  = 0x3DF, G1  = 0x3E0,
            Gs0 = 0x3E1, Gs1 = 0x3E2,
        },
    },

    lute = {
        name  = "Lute",
        notes = {
            A1  = 0x3FD, A2  = 0x3FE,
            As1 = 0x3FF, As2 = 0x400,
            B1  = 0x401, B2  = 0x402,
            C0  = 0x404, C1  = 0x405, C2 = 0x406,
            Cs0 = 0x407, Cs1 = 0x408,
            D0  = 0x409, D1  = 0x40A,
            Ds0 = 0x40C, Ds1 = 0x40D,
            E0  = 0x40E, E1  = 0x40F,
            F0  = 0x410, F1  = 0x411,
            Fs0 = 0x412, Fs1 = 0x413,
            G0  = 0x414, G1  = 0x415,
            Gs0 = 0x416, Gs1 = 0x417,
        },
    },
}

-- ═════════════════════════════════════════════════════════════════════════
-- 2. Internal helpers
-- ═════════════════════════════════════════════════════════════════════════

--- Resolve an instrument argument to a note table.
--- Accepts a string key ("harp"), an INSTRUMENTS entry, or a raw
--- {name=id} table.
local function resolve_notes(instrument)
    if type(instrument) == "string" then
        local instr = INSTRUMENTS[instrument]
        if not instr then
            error("unknown instrument: " .. instrument)
        end
        return instr.notes
    elseif instrument.notes then
        return instrument.notes
    else
        return instrument
    end
end

--- Resolve a sound source to x, y, z coordinates.
--- Accepts {x, y, z} table, or an Actor (has :pos() method).
local function resolve_source(source)
    if type(source) == "table" and source.pos then
        return source:pos()
    elseif type(source) == "table" then
        return source[1], source[2], source[3]
    else
        error("play_melody: source must be {x,y,z} or an Actor")
    end
end

-- Duration suffix → multiplier of one beat (quarter note).
local DURATION_MAP = {
    ["--"]  = 4.0,     -- whole
    ["-."]  = 3.0,     -- dotted half
    ["-"]   = 2.0,     -- half
    [".."]  = 1.75,    -- double-dotted quarter
    ["."]   = 1.5,     -- dotted quarter
    ["'."]  = 0.75,    -- dotted eighth
    ["''"]  = 0.25,    -- sixteenth
    ["'"]   = 0.5,     -- eighth
}

-- ═════════════════════════════════════════════════════════════════════════
-- 3. Melody parser
-- ═════════════════════════════════════════════════════════════════════════
--
-- Compact text notation for writing melodies as plain strings.
--
-- Format:
--   "C1 D1 E1 F1 | G1- A2. G1' | E1 _' D1 C1"
--
-- Each token is a note or a rest, optionally followed by a duration
-- suffix.
--
-- ── Notes ────────────────────────────────────────────────────────────
--   Note name   = letter + optional "s" (sharp) + octave digit
--   Examples:   C1  Ds0  As2  Fs1  G0  B1
--
-- ── Rests ────────────────────────────────────────────────────────────
--   _           rest (silence)
--
-- ── Duration suffixes ────────────────────────────────────────────────
--   (none)      quarter note          1 beat
--   -           half note             2 beats
--   --          whole note            4 beats
--   .           dotted quarter        1.5 beats
--   -.          dotted half           3 beats
--   '           eighth note           0.5 beat
--   ''          sixteenth note        0.25 beat
--   '.          dotted eighth         0.75 beat
--
-- ── Bar lines ────────────────────────────────────────────────────────
--   |           ignored (visual aid only)
--
-- ── Tempo ────────────────────────────────────────────────────────────
--   One beat = quarter note.  beat_ms controls tempo.
--   120 BPM → beat_ms = 500.   104 BPM → beat_ms ≈ 577.

--- Parse a melody string into a list of { sound_id, duration_ms }.
---
--- @param str        string   melody in text notation
--- @param instrument string|table  instrument key or note table
--- @param beat_ms    number   duration of one quarter note in ms
--- @return table  list of { [1]=sound_id|nil, [2]=duration_ms }
function parse_melody(str, instrument, beat_ms)
    beat_ms = beat_ms or 500
    local note_ids = resolve_notes(instrument)
    local notes = {}

    for token in str:gmatch("[^%s|]+") do
        local sound_id = nil
        local dur_suffix = ""

        if token:sub(1, 1) == "_" then
            -- Rest
            dur_suffix = token:sub(2)
        else
            -- Note: grab the name part (letters + digits), rest is duration
            local name = token:match("^([A-Gs]+%d)")
            if name and note_ids[name] then
                sound_id = note_ids[name]
                dur_suffix = token:sub(#name + 1)
            else
                log("music: unknown note '" .. token .. "', skipping")
                goto continue
            end
        end

        local mul = DURATION_MAP[dur_suffix] or 1.0
        local dur = math.floor(beat_ms * mul + 0.5)
        table.insert(notes, { sound_id, dur })

        ::continue::
    end

    return notes
end

-- ═════════════════════════════════════════════════════════════════════════
-- 4. Playback
-- ═════════════════════════════════════════════════════════════════════════

--- Play a sequence of notes with accurate timing.
---
--- Uses absolute scheduling via clock() to prevent timing drift from
--- accumulating over many notes.
---
--- Works inside scene:parallel() — the scheduler hooks sleep() and
--- timing is preserved correctly.
---
--- @param world   LuaWorld  world object (for play_sound)
--- @param notes   table     result of parse_melody() or manual list
--- @param source  table     {x,y,z} coordinates or an Actor with :pos()
--- @param opts    table|nil optional: { stop = function() → bool }
function play_melody(world, notes, source, opts)
    local start   = clock()
    local elapsed = 0

    for _, entry in ipairs(notes) do
        if opts and opts.stop and opts.stop() then break end

        local sound_id = entry[1]
        local dur      = entry[2]

        if sound_id then
            local x, y, z = resolve_source(source)
            world:play_sound(sound_id, x, y, z)
        end

        elapsed = elapsed + dur
        local wait = elapsed - (clock() - start) * 1000
        if wait > 0 then
            sleep(math.floor(wait + 0.5))
        end
    end
end

-- ═════════════════════════════════════════════════════════════════════════
-- 5. Utility functions
-- ═════════════════════════════════════════════════════════════════════════

--- Calculate total duration of a note list in milliseconds.
---
--- @param notes  table  result of parse_melody()
--- @return number  total duration in ms
function melody_duration(notes)
    local total = 0
    for _, entry in ipairs(notes) do
        total = total + entry[2]
    end
    return total
end

--- Build a comma-separated list of melody names from the MELODIES table.
--- Returns "" if MELODIES is not defined.
---
--- @return string
function melody_list()
    if not MELODIES then return "" end
    local names = {}
    for k in pairs(MELODIES) do
        table.insert(names, k)
    end
    table.sort(names)
    return table.concat(names, ", ")
end

--- Convenience: parse + play a melody from the MELODIES table.
---
--- @param name       string    melody key in MELODIES
--- @param instrument string    instrument key in INSTRUMENTS
--- @param world      LuaWorld  world object
--- @param source     table     {x,y,z} or Actor
--- @param opts       table|nil optional: { stop = function() → bool }
function play(name, instrument, world, source, opts)
    local m = MELODIES[name]
    if not m then
        log("music: unknown melody '" .. name .. "'")
        return
    end
    local beat_ms = math.floor(60000 / (m.bpm or 120) + 0.5)
    local notes = parse_melody(m.data, instrument, beat_ms)
    play_melody(world, notes, source, opts)
end
