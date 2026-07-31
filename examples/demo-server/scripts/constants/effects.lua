-- constants/effects.lua — Visual effect graphic IDs.
--
-- Item graphic IDs used as visual effects via w:effect() or broadcast:effect().
--
-- Usage:  broadcast:effect({ graphic = EFFECT.FLAMESTRIKE, ... })

EFFECT = {}

-- ── Spell effects ───────────────────────────────────────────────────────
EFFECT.FLAMESTRIKE     = 0x3709   -- flamestrike column (14089)
EFFECT.HEAL            = 0x375A   -- heal sparkles (14170)
EFFECT.TELEPORT        = 0x3728   -- teleport shimmer (14120)
EFFECT.EXPLOSION       = 0x36BD   -- explosion (14013)
EFFECT.FIREBALL        = 0x36D4   -- fireball projectile (14036)
EFFECT.ENERGY_BOLT     = 0x379F   -- energy bolt projectile (14239)
EFFECT.MAGIC_ARROW     = 0x36E4   -- magic arrow projectile (14052)
EFFECT.POISON_CLOUD    = 0x3400   -- poison field / cloud
EFFECT.PARALYZE_FIELD  = 0x3818   -- paralyze field (14360)
EFFECT.FIRE_FIELD      = 0x3996   -- fire field wall (14742)
EFFECT.FIZZLE          = 0x3735   -- spell fizzle puff

-- ── Environmental effects ───────────────────────────────────────────────
EFFECT.SPARKLE         = 0x373A   -- generic sparkle (14138)
EFFECT.SMOKE           = 0x3728   -- smoke puff (same as teleport)
EFFECT.FIRE_BALL_SMALL = 0x36FE   -- small fire burst (14078)
EFFECT.GLOW            = 0x375A   -- glowing particles (same as heal)
EFFECT.MOONGATE        = 0x3818   -- moongate shimmer
