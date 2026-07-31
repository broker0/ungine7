-- melodies.lua: Library of melodies for use with music.lua.
--
-- Usage:
--   dofile("scripts/music.lua")
--   dofile("scripts/melodies.lua")
--
--   play("march", "harp", w, bard)
--
-- Each melody has:
--   title  — human-readable name
--   bpm    — tempo in beats per minute (quarter note = 1 beat)
--   data   — melody in text notation (see music.lua for format)

if not INSTRUMENTS then
    dofile("scripts/music.lua")
end

MELODIES = {}

MELODIES["greensleeves"] = {
    title = "Greensleeves",
    bpm   = 140,
    data  = [[
        A1 | C1- D1' | E1. D1'' E1' | F1- E1' |
        D1- B1' | G0. A1'' B1' | C1- A1' |
        A1. Gs0'' A1' | B1- Gs0' | E0- _' |
        A1 | C1- D1' | E1. D1'' E1' | F1- E1' |
        D1- B1' | G0. A1'' B1' | C1'. B1'' A1' |
        Gs0. Fs0'' Gs0' | A1-- |
        G1-. | G1. Fs0'' G1' | A1- F1' |
        E0. D0'' E0' | F1- D1' | B1. A1'' B1' |
        C1- A1' | A1. Gs0'' A1' | B1- Gs0' |
        E0- _' | G1-. | G1. Fs0'' G1' |
        A1- F1' | E0. D0'' E0' | D1- B1' |
        G0. A1'' B1' | C1'. B1'' A1' | Gs0. Fs0'' Gs0' |
        A1--
    ]],
}

MELODIES["ode"] = {
    title = "Ode to Joy",
    bpm   = 120,
    data  = [[
        E1 E1 F1 G1 | G1 F1 E1 D1 | C1 C1 D1 E1 | E1. D1' D1- |
        E1 E1 F1 G1 | G1 F1 E1 D1 | C1 C1 D1 E1 | D1. C1' C1- |
        D1 D1 E1 C1 | D1 E1' F1' E1 C1 | D1 E1' F1' E1 D1 | C1 D1 G0- |
        E1 E1 F1 G1 | G1 F1 E1 D1 | C1 C1 D1 E1 | D1. C1' C1-
    ]],
}

MELODIES["scale"] = {
    title = "Chromatic Scale",
    bpm   = 200,
    data  = [[
        A1 As1 B1 C1 Cs1 D0 Ds0 E0 F0 Fs0 G0 Gs0 |
        A2 As2 B2 C2 Cs2 D1 Ds1 E1 F1 Fs1 G1 Gs1 |
        G1 Fs1 F1 E1 Ds1 D1 Cs2 C2 B2 As2 A2 |
        Gs0 G0 Fs0 F0 E0 Ds0 D0 Cs1 C1 B1 As1 A1
    ]],
}

MELODIES["twinkle"] = {
    title = "Twinkle Twinkle Little Star",
    bpm   = 120,
    data  = [[
        C1 C1 G1 G1 | A2 A2 G1- | F1 F1 E1 E1 | D1 D1 C1- |
        G1 G1 F1 F1 | E1 E1 D1- | G1 G1 F1 F1 | E1 E1 D1- |
        C1 C1 G1 G1 | A2 A2 G1- | F1 F1 E1 E1 | D1 D1 C1-
    ]],
}

MELODIES["mary"] = {
    title = "Mary Had a Little Lamb",
    bpm   = 140,
    data  = [[
        E1 D1 C1 D1 | E1 E1 E1- | D1 D1 D1- | E1 G1 G1- |
        E1 D1 C1 D1 | E1 E1 E1 E1 | D1 D1 E1 D1 | C1--
    ]],
}
