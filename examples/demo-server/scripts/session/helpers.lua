-- session/helpers.lua — Shared utility functions for session game logic.
--
-- Geometry, entity queries, reagent system.
-- Depends on: constants.lua (LAYER, etc.)

-- ── Math / geometry ──────────────────────────────────────────────────────

--- Inclusive random integer in [min, max].
function random_range(min, max)
    return math.random(min, max)
end

--- Chebyshev (king-move) distance between two tile positions.
function chebyshev(x1, y1, x2, y2)
    return math.max(math.abs(x1 - x2), math.abs(y1 - y2))
end

-- ── Entity helpers ───────────────────────────────────────────────────────

--- Check if a mobile entity has a mount equipped.
function is_mounted(entity)
    if not entity or not entity.items then return false end
    for _, eq in ipairs(entity.items) do
        if eq.layer == LAYER.MOUNT then return true end
    end
    return false
end

--- Find backpack serial from a mobile entity's equipment list.
function get_backpack_serial(entity)
    if not entity or not entity.items then return nil end
    for _, eq in ipairs(entity.items) do
        if eq.layer == LAYER.BACKPACK then return eq.serial end
    end
    return nil
end

--- Find an equipped item by layer. Returns the equipment entry or nil.
function get_equipped(entity, layer)
    if not entity or not entity.items then return nil end
    for _, eq in ipairs(entity.items) do
        if eq.layer == layer then return eq end
    end
    return nil
end

--- Resolve cast animation ID accounting for mount state.
--- @deprecated Use resolve_animation() instead.
function resolve_cast_action(cast_action, mounted)
    return resolve_animation(cast_action, mounted)
end

--- Resolve a humanoid animation action ID accounting for mount state.
---
--- Returns the resolved action ID, or nil if the animation should be
--- skipped entirely (e.g. emotes or crafting gestures have no mounted
--- variant and look wrong on a horse).
---
--- This is the single source of truth for animation resolution on the
--- Lua session side.
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

-- ── Reagent system ───────────────────────────────────────────────────────

--- Find all required reagent item serials in a mobile's backpack.
--- Returns a table of item serials (one per reagent) or nil if any is missing.
---
--- Handles duplicate reagent requirements correctly — each entry in
--- `reagent_graphics` reserves one unit from the matching stack.
function find_reagent_items(caster_serial, reagent_graphics)
    local caster = engine:get_entity(caster_serial)
    if not caster then return nil end

    local bp_serial = get_backpack_serial(caster)
    if not bp_serial then return nil end

    local container = engine:get_container(bp_serial)
    if not container or not container.items then return nil end

    -- Build mutable available stacks.
    local available = {}
    for _, item in ipairs(container.items) do
        table.insert(available, {
            graphic = item.graphic,
            serial = item.serial,
            remaining = math.max(item.amount, 1),
        })
    end

    local result = {}
    for _, reagent_graphic in ipairs(reagent_graphics) do
        local found = false
        for _, stack in ipairs(available) do
            if stack.graphic == reagent_graphic and stack.remaining > 0 then
                table.insert(result, stack.serial)
                stack.remaining = stack.remaining - 1
                found = true
                break
            end
        end
        if not found then return nil end
    end

    return result
end

--- Consume all reagent items (one unit each).
function consume_reagents(reagent_serials)
    for _, serial in ipairs(reagent_serials) do
        engine:consume_item(serial, 1)
    end
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
