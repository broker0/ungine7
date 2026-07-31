-- constants/items.lua — Item graphics, reagents, bandage configuration.
--
-- Usage:  if info.graphic == BANDAGE_GRAPHIC then ... end

-- ── Bandage ──────────────────────────────────────────────────────────────

BANDAGE_GRAPHIC = 0x0E21

BANDAGE = {
    DELAY_MS = 3000,
    RANGE    = 2,
    HEAL_MIN = 15,
    HEAL_MAX = 30,
    SOUND    = 0x0048,
}

-- ── Reagent graphics ─────────────────────────────────────────────────────

REAGENT = {
    BLACK_PEARL    = 0x0F7A,
    BLOOD_MOSS     = 0x0F7B,
    GARLIC         = 0x0F84,
    GINSENG        = 0x0F85,
    MANDRAKE_ROOT  = 0x0F86,
    NIGHTSHADE     = 0x0F88,
    SULPHUROUS_ASH = 0x0F8C,
    SPIDERS_SILK   = 0x0F8D,
}
