-- constants/regen.lua — Stat regeneration configuration.
--
-- Shared between controller and session modes.
--
-- Usage:  w:heal_entity(me, REGEN.HP_PER_TICK)

REGEN = {
    TICK_MS          = 2000,
    HP_PER_TICK      = 1,
    STAM_PER_TICK    = 2,
    MANA_PER_TICK    = 1,
    MEDITATION_BONUS = 3,
}
