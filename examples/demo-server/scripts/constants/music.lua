-- constants/music.lua — Music track IDs.
--
-- UO music IDs for w:play_music(id).  Track names are approximate —
-- actual names vary by client version.
--
-- Usage:  scene:play_music(MUSIC.BRITAIN)

MUSIC = {}

MUSIC.STOP             = 0x1FFF   -- stop / silence

-- ── Towns ───────────────────────────────────────────────────────────────
MUSIC.BRITAIN          = 0x0008   -- Britain theme
MUSIC.TRINSIC          = 0x0009   -- Trinsic theme
MUSIC.MINOC            = 0x000A   -- Minoc theme
MUSIC.YEW              = 0x000B   -- Yew / forest theme
MUSIC.MOONGLOW         = 0x000C   -- Moonglow theme
MUSIC.JHELOM           = 0x000D   -- Jhelom theme
MUSIC.MAGINCIA         = 0x000E   -- Magincia theme
MUSIC.SKARA_BRAE       = 0x000F   -- Skara Brae theme
MUSIC.VESPER           = 0x0010   -- Vesper theme

-- ── Exploration ─────────────────────────────────────────────────────────
MUSIC.FOREST           = 0x0002   -- forest exploration
MUSIC.DUNGEON          = 0x0003   -- dungeon / underground
MUSIC.COMBAT           = 0x0005   -- combat / battle
MUSIC.DEATH            = 0x0006   -- death / defeat
MUSIC.VICTORY          = 0x0007   -- victory fanfare
MUSIC.TAVERN           = 0x0019   -- tavern / inn
MUSIC.OCEAN            = 0x001D   -- ocean / sailing
MUSIC.MOUNTAINS        = 0x001E   -- mountains / highlands

-- ── Atmosphere ──────────────────────────────────────────────────────────
MUSIC.TEMPLE           = 0x0016   -- temple / shrine
MUSIC.CASTLE           = 0x0022   -- castle / throne room
MUSIC.GRAVEYARD        = 0x0023   -- graveyard / undead
MUSIC.SWAMP            = 0x001C   -- swamp / marsh
MUSIC.DESERT           = 0x001B   -- desert
MUSIC.APPROACH         = 0x0037   -- dramatic approach (55)
MUSIC.SADNESS          = 0x0026   -- sad / mournful
MUSIC.SHOPPING         = 0x0028   -- marketplace / shopping
