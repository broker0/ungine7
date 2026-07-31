-- session/spells.lua — Spell casting logic (two-phase).
--
-- Spell definitions are loaded from the Rust engine via engine:get_all_spells().
-- This script implements:
--   begin_cast()    — phase 1: check reagents, LOS, mana; play words + animation
--   complete_cast() — phase 2: re-check LOS, consume mana + reagents, apply effects
--
-- To use a custom spell system, replace `spells = engine:get_all_spells()`
-- with your own table (e.g. dofile("my_spells.lua")) — the casting logic
-- works with any table that has the expected fields.
--
-- Depends on: constants.lua, helpers.lua

-- ══════════════════════════════════════════════════════════════════════════
-- Spell definitions (loaded from Rust engine)
-- ══════════════════════════════════════════════════════════════════════════

--- Master spell table, keyed by spell ID.
--- Contains all fields from the Rust SpellDef: id, name, mana, circle,
--- cast_delay_ms, scroll_cast_delay_ms, damage_min/max, heal_min/max,
--- needs_target, can_self, harmful, words, cast_sound, impact_sound,
--- cast_action, projectile_graphic, target_effect, target_effect_speed,
--- target_effect_duration, lightning_bolt, scroll_graphic, reagents.
spells = engine:get_all_spells()

-- ══════════════════════════════════════════════════════════════════════════
-- Phase 1 — begin_cast
-- ══════════════════════════════════════════════════════════════════════════

--- Begin a spell cast: check reagents, LOS, mana; play words + animation.
--- If scroll_item_serial is provided, reagent and mana checks are skipped
--- (scroll casts use no mana or reagents).
--- Returns true if cast started successfully.
function begin_cast(spell, caster_serial, target_serial, scroll_item_serial)
    local is_scroll = (scroll_item_serial ~= nil)

    -- 1. Check reagent availability (skip for scroll casts).
    if not is_scroll and spell.reagents and #spell.reagents > 0 then
        if not find_reagent_items(caster_serial, spell.reagents) then
            session:send_system_message("Insufficient reagents.")
            return false
        end
    end

    -- 2. LOS check (skip for self-target).
    if target_serial ~= caster_serial then
        local caster = engine:get_entity(caster_serial)
        local target = engine:get_entity(target_serial)
        if not caster or not target then
            session:send_system_message("Invalid target.")
            return false
        end
        if not engine:has_los(
            caster.x, caster.y, caster.z + EYE_HEIGHT,
            target.x, target.y, target.z + EYE_HEIGHT
        ) then
            session:send_system_message("Target cannot be seen.")
            return false
        end
    end

    -- 3. Mana check.
    if is_scroll then
        -- Scroll cast: check full mana, consume upfront half.
        local caster_ent = engine:get_entity(caster_serial)
        if not caster_ent or caster_ent.mana < spell.mana then
            session:send_system_message("Insufficient mana.")
            return false
        end
        local upfront = math.floor(spell.mana / 2)
        if upfront > 0 then
            engine:consume_mana(caster_serial, upfront)
        end
    else
        -- Reagent cast: only check mana (consumed in complete_cast).
        local caster_ent = engine:get_entity(caster_serial)
        if not caster_ent or caster_ent.mana < spell.mana then
            session:send_system_message("Insufficient mana.")
            return false
        end
    end

    -- 4. Spell words + cast animation.
    local caster = engine:get_entity(caster_serial)
    if not caster then return false end

    local mounted = is_mounted(caster)
    local action = resolve_animation(spell.cast_action or ANIM.CAST_DIRECTED, mounted)

    if spell.words then
        broadcast:speech(caster_serial, caster.graphic, spell.words, {
            speech_type = 0x00,
            color = HUE.SPELL_WORDS,
            font = 3,
            name = caster.name or "",
        })
    end

    if action then
        broadcast:animation(caster_serial, action, 5, { repeat_count = 1 })
    end

    return true
end

-- ══════════════════════════════════════════════════════════════════════════
-- Phase 2 — complete_cast
-- ══════════════════════════════════════════════════════════════════════════

--- Complete a spell cast: re-check LOS, consume mana + reagents, apply effects.
--- If scroll_item_serial is provided, scroll is consumed instead of mana/reagents.
function complete_cast(spell, caster_serial, target_serial, scroll_item_serial)
    local is_scroll = (scroll_item_serial ~= nil)

    -- 1. Get caster.
    local caster = engine:get_entity(caster_serial)
    if not caster then return end

    -- 2. Get target.
    local target = engine:get_entity(target_serial)
    if not target then
        session:send_fizzle(caster_serial, caster.x, caster.y, caster.z, "Invalid target.")
        return
    end

    -- 3. LOS re-check.
    if target_serial ~= caster_serial then
        if not engine:has_los(
            caster.x, caster.y, caster.z + EYE_HEIGHT,
            target.x, target.y, target.z + EYE_HEIGHT
        ) then
            session:send_fizzle(caster_serial, caster.x, caster.y, caster.z, "The spell fizzles.")
            return
        end
    end

    if is_scroll then
        -- 4a. Scroll cast — consume remaining mana half, then consume the scroll.
        local remaining = spell.mana - math.floor(spell.mana / 2)
        if remaining > 0 then
            if not engine:consume_mana(caster_serial, remaining) then
                session:send_fizzle(caster_serial, caster.x, caster.y, caster.z, "Insufficient mana.")
                return
            end
        end
        local consumed = engine:consume_item(scroll_item_serial, 1)
        if not consumed then
            session:send_fizzle(caster_serial, caster.x, caster.y, caster.z, "The scroll is gone.")
            return
        end
    else
        -- 4b. Normal cast — consume mana.
        if not engine:consume_mana(caster_serial, spell.mana) then
            session:send_fizzle(caster_serial, caster.x, caster.y, caster.z, "Insufficient mana.")
            return
        end

        -- 5. Consume reagents.
        if spell.reagents and #spell.reagents > 0 then
            local reagent_serials = find_reagent_items(caster_serial, spell.reagents)
            if not reagent_serials then
                session:send_fizzle(caster_serial, caster.x, caster.y, caster.z, "Insufficient reagents.")
                return
            end
            consume_reagents(reagent_serials)
        end
    end

    -- 6. Visual effects.

    -- Projectile.
    if spell.projectile_graphic and spell.projectile_graphic ~= 0 then
        broadcast:effect({
            direction_type = 0,
            source_serial = caster_serial,
            target_serial = target_serial,
            graphic = spell.projectile_graphic,
            x = caster.x, y = caster.y, z = caster.z + 15,
            target_x = target.x, target_y = target.y, target_z = target.z + 15,
            speed = 10, duration = 30,
            fixed_direction = false,
            explode = false,
        })
    end

    -- Impact sound.
    if spell.impact_sound and spell.impact_sound ~= 0 then
        broadcast:sound(spell.impact_sound, target.x, target.y, target.z)
    end

    -- Lightning bolt effect.
    if spell.lightning_bolt then
        broadcast:effect({
            direction_type = 1,
            source_serial = target_serial,
            target_serial = 0,
            graphic = 0,
            x = target.x, y = target.y, z = target.z,
            speed = 0, duration = 0,
            fixed_direction = false,
            explode = false,
        })
    end

    -- Target effect (sparkle, flamestrike, etc.).
    if spell.target_effect and spell.target_effect ~= 0 then
        broadcast:effect({
            direction_type = 3,
            source_serial = target_serial,
            target_serial = 0,
            graphic = spell.target_effect,
            x = target.x, y = target.y, z = target.z,
            speed = spell.target_effect_speed or 10,
            duration = spell.target_effect_duration or 30,
            fixed_direction = false,
            explode = false,
        })
    end

    -- 7. Damage.
    if spell.damage_max > 0 and target_serial ~= caster_serial then
        local damage = random_range(spell.damage_min, spell.damage_max)
        local result = engine:deal_damage(target_serial, damage, caster_serial)
        if result and result.killed then
            log(string.format("%s killed 0x%08X", spell.name, target_serial))
        end
    end

    -- 8. Healing.
    if spell.heal_max > 0 then
        local heal_amount = random_range(spell.heal_min, spell.heal_max)
        local new_hits = engine:heal_entity(target_serial, heal_amount)
        if new_hits then
            session:send_unicode_speech({
                serial = target_serial,
                graphic = target.graphic or 0,
                color = HUE.HEAL_FEEDBACK,
                font = 9,
                name = target.name or "",
                message = "+" .. tostring(heal_amount),
            })
        end
    end

    log(string.format("0x%08X cast %s on 0x%08X", caster_serial, spell.name, target_serial))
end
