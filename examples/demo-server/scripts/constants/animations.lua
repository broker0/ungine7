-- constants/animations.lua — Character animation action IDs.
--
-- Standard action IDs for humanoid body types (0x0190 / 0x0191).
-- Monster bodies use different action IDs — consult the UO animation
-- tables for those.
--
-- Usage:  w:animate(serial, ANIM.CAST_DIRECTED, 7, { repeat_count = 1 })

ANIM = {}

-- ── Locomotion ──────────────────────────────────────────────────────────
ANIM.WALK             = 0x00   -- walk forward
ANIM.WALK_WEAPON      = 0x01   -- walk forward (armed)
ANIM.RUN              = 0x02   -- run forward
ANIM.RUN_WEAPON       = 0x03   -- run forward (armed)
ANIM.STAND            = 0x04   -- stand idle
ANIM.FIDGET_1         = 0x05   -- idle fidget (shift weight)
ANIM.FIDGET_2         = 0x06   -- idle fidget (look around)

-- ── Melee combat ────────────────────────────────────────────────────────
ANIM.SLASH_1H         = 0x09   -- one-handed slash
ANIM.PIERCE_1H        = 0x0A   -- one-handed pierce / stab
ANIM.SWING_2H         = 0x0B   -- two-handed overhead swing
ANIM.SLASH_2H         = 0x0C   -- two-handed slash
ANIM.PIERCE_2H        = 0x0D   -- two-handed pierce

-- ── Ranged combat ───────────────────────────────────────────────────────
ANIM.SHOOT_BOW        = 0x12   -- shoot a bow
ANIM.SHOOT_XBOW       = 0x13   -- shoot a crossbow

-- ── Damage / death ──────────────────────────────────────────────────────
ANIM.GET_HIT          = 0x14   -- take a hit (flinch)
ANIM.DIE_FORWARD      = 0x15   -- fall forward (die)
ANIM.DIE_BACKWARD     = 0x16   -- fall backward (die)

-- ── Magic ───────────────────────────────────────────────────────────────
ANIM.CAST             = 0x10   -- spellcasting gesture (hands raised)
ANIM.CAST_AREA        = 0x10   -- alias: area-of-effect cast (same action)
ANIM.CAST_DIRECTED    = 0x11   -- cast directed at target

-- ── Mounted ─────────────────────────────────────────────────────────────
ANIM.MOUNTED_WALK          = 0x17   -- walk while mounted
ANIM.MOUNTED_RUN           = 0x18   -- run while mounted
ANIM.MOUNTED_STAND         = 0x19   -- stand while mounted
ANIM.MOUNTED_ATTACK        = 0x1A   -- mounted melee / ranged attack
ANIM.MOUNTED_CAST_DIRECTED = 0x1B   -- cast directed while mounted
ANIM.MOUNTED_CAST_AREA     = 0x1C   -- cast area while mounted
ANIM.MOUNTED_GET_HIT       = 0x1D   -- take a hit while mounted

-- ── Emotes / social ─────────────────────────────────────────────────────
ANIM.BOW              = 0x20   -- bow
ANIM.SALUTE           = 0x21   -- salute
ANIM.EAT              = 0x22   -- eat / drink
