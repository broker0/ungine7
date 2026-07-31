-- controller/helpers.lua — Shared utility functions for controller scripts.
--
-- Geometry, random, target set management, animation/sound resolution.
-- Depends on: constants/animations.lua, constants/sounds.lua, constants/layers.lua
-- No dependencies on other controller modules.

-- ── Math / geometry ──────────────────────────────────────────────────────

--- Chebyshev (king-move) distance between two tile positions.
function chebyshev(x1, y1, x2, y2)
    return math.max(math.abs(x1 - x2), math.abs(y1 - y2))
end

--- Inclusive random integer in [lo, hi].
function random_range(lo, hi)
    return math.random(lo, hi)
end

-- ── Entity helpers ───────────────────────────────────────────────────────

--- Check if an entity has a mount equipped.
function entity_is_mounted(entity)
    if not entity or not entity.items then return false end
    for _, eq in ipairs(entity.items) do
        if eq.layer == LAYER.MOUNT then return true end
    end
    return false
end

-- ── Animation resolution ─────────────────────────────────────────────────

--- Resolve a humanoid animation action ID accounting for mount state.
---
--- Returns the resolved action ID, or nil if the animation should be
--- skipped entirely (e.g. emotes or crafting gestures have no mounted
--- variant).
function resolve_animation(action, mounted)
    if not mounted then return action end
    -- Melee attacks → mounted attack
    if action == ANIM.SLASH_1H  or action == ANIM.PIERCE_1H
    or action == ANIM.SWING_2H  or action == ANIM.SLASH_2H
    or action == ANIM.PIERCE_2H
    or action == ANIM.SHOOT_BOW or action == ANIM.SHOOT_XBOW then
        return ANIM.MOUNTED_ATTACK
    end
    -- Get hit → mounted get hit
    if action == ANIM.GET_HIT then return ANIM.MOUNTED_GET_HIT end
    -- Cast → mounted cast
    if action == ANIM.CAST_DIRECTED then return ANIM.MOUNTED_CAST_DIRECTED end
    if action == ANIM.CAST_AREA    then return ANIM.MOUNTED_CAST_AREA end
    -- Emotes / eat — no mounted variant → skip
    if action == ANIM.BOW or action == ANIM.SALUTE or action == ANIM.EAT then
        return nil
    end
    return action
end

-- ── Gender-aware sound helpers ───────────────────────────────────────────

--- Body graphic for a female human character.
local FEMALE_HUMAN_GRAPHIC = 0x0191

--- Male hurt (pain) sound pool — classic UO T2A.
local MALE_HURT_SOUNDS   = { 0x0154, 0x0155, 0x0156, 0x0157, 0x0158 }
--- Female hurt (pain) sound pool — classic UO T2A.
local FEMALE_HURT_SOUNDS = { 0x014B, 0x014C, 0x014D, 0x014E, 0x014F }

--- Check if a body graphic represents a female character.
function is_female_body(graphic)
    return graphic == FEMALE_HUMAN_GRAPHIC
end

--- Pick a random hurt (pain) sound appropriate for the character's body.
function random_hurt_sound(graphic)
    local pool = is_female_body(graphic) and FEMALE_HURT_SOUNDS or MALE_HURT_SOUNDS
    return pool[math.random(#pool)]
end
