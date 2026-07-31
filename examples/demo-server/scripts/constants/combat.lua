-- constants/combat.lua — Combat timing, ranges, damage constants.
--
-- Shared between controller and session modes.
--
-- Usage:  if dist > COMBAT.MELEE_RANGE then ... end

-- ── Geometry ─────────────────────────────────────────────────────────────

EYE_HEIGHT = 14   -- LOS offset (z) for humanoid entities

-- ── Combat timing ────────────────────────────────────────────────────────

COMBAT = {
    SWING_DELAY_MS = 2500,   -- time between melee swings
    MELEE_RANGE    = 2,      -- Chebyshev tiles
}

-- ── Base damage (unarmed / fallback) ─────────────────────────────────────

DAMAGE = {
    MIN = 3,
    MAX = 12,
}
