-- constants/sounds.lua — Sound effect IDs.
--
-- Sound IDs sent via w:play_sound(id, x, y, z) or broadcast:sound(id, x, y, z).
--
-- Usage:  w:play_sound(SOUND.LIGHTNING, x, y, z)

SOUND = {}

-- ── Spells / magic ──────────────────────────────────────────────────────
SOUND.LIGHTNING       = 0x0029   -- lightning bolt strike
SOUND.HEAL            = 0x01F2   -- heal / cure
SOUND.MAGIC_ARROW     = 0x01E5   -- magic arrow impact
SOUND.ENERGY_BOLT     = 0x020A   -- energy bolt impact
SOUND.FLAMESTRIKE     = 0x0208   -- flamestrike / explosion
SOUND.TELEPORT        = 0x01FE   -- teleport arrival (recall)
SOUND.MANA_DRAIN      = 0x01F8   -- mana drain / vampire
SOUND.SUMMON          = 0x0217   -- summoning
SOUND.CURSE           = 0x01FC   -- curse applied
SOUND.BLESS           = 0x0202   -- blessing
SOUND.POISON          = 0x0205   -- poison applied
SOUND.RESURRECT       = 0x0214   -- resurrection
SOUND.FIZZLE          = 0x005C   -- spell fizzle

-- ── Combat / impacts ────────────────────────────────────────────────────
SOUND.SWORD_HIT       = 0x023B   -- sword hit flesh
SOUND.AXE_HIT         = 0x0237   -- axe / polearm hit
SOUND.MACE_HIT        = 0x0233   -- mace / blunt hit
SOUND.ARROW_HIT       = 0x0234   -- arrow hit
SOUND.SHIELD_BLOCK    = 0x023C   -- shield block
SOUND.MISS            = 0x0238   -- weapon miss (whoosh)
SOUND.PUNCH           = 0x0135   -- unarmed punch
SOUND.FIST_HIT        = 0x0145   -- fist hit (melee)
SOUND.SWING           = 0x023C   -- melee swing (played at attacker)
SOUND.WEAPON_SWOOSH   = 0x0159   -- weapon swoosh (lighter swing sound)
SOUND.BANDAGE         = 0x0048   -- bandage application

-- ── Creature sounds ─────────────────────────────────────────────────────
SOUND.DAEMON_ROAR     = 0x0208   -- daemon roar (same as flamestrike)
SOUND.DRAGON_ROAR     = 0x016C   -- dragon roar
SOUND.WOLF_HOWL       = 0x00E5   -- wolf howl
SOUND.HORSE_WHINNY    = 0x00A8   -- horse whinny
SOUND.SKELETON_RATTLE = 0x01C3   -- skeleton bone rattle

-- ── Ambient / environment ───────────────────────────────────────────────
SOUND.THUNDER         = 0x0029   -- thunder clap (same as lightning)
SOUND.WIND            = 0x0014   -- wind
SOUND.WATER_SPLASH    = 0x0025   -- water splash
SOUND.DOOR_OPEN       = 0x00EA   -- door opening
SOUND.DOOR_CLOSE      = 0x00F1   -- door closing
SOUND.FOOTSTEPS       = 0x012B   -- footsteps on stone
SOUND.CAMPFIRE        = 0x0225   -- campfire crackle
SOUND.ANVIL_STRIKE    = 0x002A   -- hammer on anvil

-- ── UI / feedback ───────────────────────────────────────────────────────
SOUND.SNEAK           = 0x01F7   -- stealth / sneaking
SOUND.COINS           = 0x0037   -- coins clinking
SOUND.DRINK           = 0x0031   -- drinking potion
SOUND.EAT             = 0x003A   -- eating

-- ── Character hurt / pain ──────────────────────────────────────────────
SOUND.MALE_HURT       = { 0x0154, 0x0155, 0x0156, 0x0157, 0x0158 }
SOUND.FEMALE_HURT     = { 0x014B, 0x014C, 0x014D, 0x014E, 0x014F }
