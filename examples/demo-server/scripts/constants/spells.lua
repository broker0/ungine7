-- constants/spells.lua — Spell IDs and cursor ID bases.
--
-- Cursor base values must not collide between systems.
--
-- Usage:  local cursor_id = SPELL_CURSOR_BASE + spell.id

-- ── Well-known spell IDs ─────────────────────────────────────────────────

SPELL_MAGIC_ARROW    = 5
SPELL_HEAL           = 4
SPELL_GREATER_HEAL   = 29
SPELL_LIGHTNING      = 30
SPELL_ENERGY_BOLT    = 42
SPELL_FLAMESTRIKE    = 51

-- ── Cursor ID bases ──────────────────────────────────────────────────────

SPELL_CURSOR_BASE   = 0xDEAD0000
BANDAGE_CURSOR_BASE = 0xBA9D0000
SKILL_CURSOR_BASE   = 0x5C110000
