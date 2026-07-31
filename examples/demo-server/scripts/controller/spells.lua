-- controller/spells.lua — Spell casting logic (two-phase) for controller mode.
--
-- Implements the same two-phase cast cycle as session/spells.lua but using
-- synchronous ControlContext methods (w:consume_mana, w:find_item_in_container,
-- w:consume_item) instead of async engine RPC.
--
-- Spell definitions are loaded from the Rust engine via w:get_all_spells(),
-- which returns the same SpellDef table as engine:get_all_spells() in
-- lua-session mode.
--
-- Depends on: constants (ANIM, SOUND, EFFECT, HUE, REAGENT, LAYER, EYE_HEIGHT,
--             SPELL_CURSOR_BASE), helpers.lua
-- Uses globals: w, me

-- ══════════════════════════════════════════════════════════════════════════
-- Spell definitions (loaded from Rust engine)
-- ══════════════════════════════════════════════════════════════════════════

--- Master spell table, keyed by spell ID.
--- Contains all fields from the Rust SpellDef: id, name, mana, circle,
--- cast_delay_ms, scroll_cast_delay_ms, damage_min/max, heal_min/max,
--- needs_target, can_self, harmful, words, cast_sound, impact_sound,
--- cast_action, projectile_graphic, target_effect, target_effect_speed,
--- target_effect_duration, lightning_bolt, scroll_graphic, reagents.
spells = w:get_all_spells()

-- ══════════════════════════════════════════════════════════════════════════
-- Cast state
-- ══════════════════════════════════════════════════════════════════════════

--- Pending spell awaiting a target cursor response.
--- Fields: spell_def, caster_serial, cursor_id
pending_spell = nil

--- Active spell cast in progress (waiting for cast_delay to expire).
--- Fields: spell_def, caster_serial, target_serial, finish_at
active_cast = nil

-- ══════════════════════════════════════════════════════════════════════════
-- Spell fizzle helper
-- ══════════════════════════════════════════════════════════════════════════

--- Play the fizzle effect + sound and send a message to the caster.
local function spell_fizzle(caster_serial, message)
    local caster = w:get_entity(caster_serial)
    if caster then
        w:play_sound(SOUND.FIZZLE, caster.x, caster.y, caster.z)
        w:effect({
            direction_type = 3,
            source_serial  = caster_serial,
            target_serial  = 0,
            graphic        = EFFECT.FIZZLE,
            x = caster.x, y = caster.y, z = caster.z,
            speed = 10, duration = 15,
            fixed_direction = false,
            explode = false,
        })
    end
    w:send_message(me, message or "The spell fizzles.", 0x0025)
end

-- ══════════════════════════════════════════════════════════════════════════
-- Reagent helpers (using synchronous controller API)
-- ══════════════════════════════════════════════════════════════════════════

--- Find all required reagent item serials in the caster's backpack.
--- Returns a table of { serial, graphic } entries, or nil if any is missing.
local function find_reagent_items(caster_serial, reagent_graphics)
    local bp_serial = w:get_backpack_serial(caster_serial)
    if not bp_serial then return nil end

    -- Build mutable available count per found item.
    -- We search for each reagent graphic in the backpack.
    local available = {}  -- graphic -> { {serial, remaining}, ... }
    for _, rg in ipairs(reagent_graphics) do
        if not available[rg] then
            local item = w:find_item_in_container(bp_serial, rg)
            if item then
                available[rg] = { serial = item.serial, remaining = math.max(item.amount, 1) }
            end
        end
    end

    local result = {}
    for _, rg in ipairs(reagent_graphics) do
        local stack = available[rg]
        if not stack or stack.remaining <= 0 then
            return nil  -- missing reagent
        end
        table.insert(result, { serial = stack.serial, graphic = rg })
        stack.remaining = stack.remaining - 1
    end
    return result
end

--- Consume all reagent items (one unit each).
local function consume_reagents(reagent_entries)
    for _, entry in ipairs(reagent_entries) do
        w:consume_item(entry.serial, 1, entry.graphic)
    end
end

-- ══════════════════════════════════════════════════════════════════════════
-- Phase 1 — begin_cast
-- ══════════════════════════════════════════════════════════════════════════

--- Begin a spell cast: check reagents, LOS, mana; play words + animation.
--- Returns true if cast started successfully.
function begin_cast(spell, caster_serial, target_serial)
    -- 1. Check reagent availability.
    if spell.reagents and #spell.reagents > 0 then
        if not find_reagent_items(caster_serial, spell.reagents) then
            w:send_message(me, "Insufficient reagents.", 0x0025)
            return false
        end
    end

    -- 2. LOS check (skip for self-target).
    if target_serial ~= caster_serial then
        local caster = w:get_entity(caster_serial)
        local target = w:get_entity(target_serial)
        if not caster or not target then
            w:send_message(me, "Invalid target.", 0x0025)
            return false
        end
        if not w:has_los(
            caster.x, caster.y, caster.z + EYE_HEIGHT,
            target.x, target.y, target.z + EYE_HEIGHT
        ) then
            w:send_message(me, "Target cannot be seen.", 0x0025)
            return false
        end
    end

    -- 3. Mana check (don't consume yet — consumed in complete_cast).
    local caster = w:get_entity(caster_serial)
    if not caster or (caster.mana or 0) < spell.mana then
        w:send_message(me, "Insufficient mana.", 0x0025)
        return false
    end

    -- 4. Spell words + cast animation.
    if spell.words then
        w:say(spell.words, {
            speech_type = 0x00,
            color = HUE.SPELL_WORDS,
            font = 3,
            name = caster.name or "",
        })
    end

    local action = spell.cast_action or ANIM.CAST_DIRECTED
    local mounted = entity_is_mounted(caster)
    local resolved = resolve_animation(action, mounted)
    if resolved then
        w:animate(caster_serial, resolved, 5, { repeat_count = 1 })
    end

    return true
end

-- ══════════════════════════════════════════════════════════════════════════
-- Phase 2 — complete_cast
-- ══════════════════════════════════════════════════════════════════════════

--- Complete a spell cast: re-check LOS, consume mana + reagents, apply effects.
function complete_cast(spell, caster_serial, target_serial)
    -- 1. Get caster.
    local caster = w:get_entity(caster_serial)
    if not caster then return end

    -- 2. Get target.
    local target = w:get_entity(target_serial)
    if not target then
        spell_fizzle(caster_serial, "Invalid target.")
        return
    end

    -- 3. LOS re-check (skip for self-target).
    if target_serial ~= caster_serial then
        if not w:has_los(
            caster.x, caster.y, caster.z + EYE_HEIGHT,
            target.x, target.y, target.z + EYE_HEIGHT
        ) then
            spell_fizzle(caster_serial, "The spell fizzles.")
            return
        end
    end

    -- 4. Consume mana.
    if not w:consume_mana(caster_serial, spell.mana) then
        spell_fizzle(caster_serial, "Insufficient mana.")
        return
    end

    -- 5. Consume reagents.
    if spell.reagents and #spell.reagents > 0 then
        local reagent_entries = find_reagent_items(caster_serial, spell.reagents)
        if not reagent_entries then
            spell_fizzle(caster_serial, "Insufficient reagents.")
            return
        end
        consume_reagents(reagent_entries)
    end

    -- 6. Visual effects.

    -- Projectile.
    if spell.projectile_graphic and spell.projectile_graphic ~= 0 then
        w:effect({
            direction_type = 0,
            source_serial  = caster_serial,
            target_serial  = target_serial,
            graphic        = spell.projectile_graphic,
            x = caster.x, y = caster.y, z = caster.z + 15,
            target_x = target.x, target_y = target.y, target_z = target.z + 15,
            speed = 10, duration = 30,
            fixed_direction = false,
            explode = false,
        })
    end

    -- Impact sound.
    if spell.impact_sound and spell.impact_sound ~= 0 then
        w:play_sound(spell.impact_sound, target.x, target.y, target.z)
    end

    -- Lightning bolt effect.
    if spell.lightning_bolt then
        w:effect({
            direction_type = 1,
            source_serial  = target_serial,
            target_serial  = 0,
            graphic        = 0,
            x = target.x, y = target.y, z = target.z,
            speed = 0, duration = 0,
            fixed_direction = false,
            explode = false,
        })
    end

    -- Target effect (heal sparkle, flamestrike, etc.).
    if spell.target_effect and spell.target_effect ~= 0 then
        w:effect({
            direction_type = 3,
            source_serial  = target_serial,
            target_serial  = 0,
            graphic        = spell.target_effect,
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
        local result = w:deal_damage(target_serial, damage)
        if result and result.killed then
            log(string.format("%s killed 0x%08X", spell.name, target_serial))
        end
    end

    -- 8. Healing.
    if spell.heal_max > 0 then
        local heal_amount = random_range(spell.heal_min, spell.heal_max)
        local new_hits = w:heal_entity(target_serial, heal_amount)
        if new_hits then
            -- Show "+N" overhead text on the target.
            -- We use say() with the target's info for a speech bubble.
            -- Note: say() uses the controller's own serial. For healing
            -- feedback on another entity we use send_message instead.
            w:send_message(me,
                string.format("You heal for %d hit points.", heal_amount),
                HUE.HEAL_FEEDBACK)
        end
    end

    log(string.format("0x%08X cast %s on 0x%08X", caster_serial, spell.name, target_serial))
end

-- ══════════════════════════════════════════════════════════════════════════
-- Command handlers (called from handlers.lua)
-- ══════════════════════════════════════════════════════════════════════════

--- Handle a CastSpell command from the client.
--- cmd.spell_id — spell ID, cmd.target_serial — pre-selected target (0 = none)
function handle_cast_spell(cmd)
    local spell_id = cmd.spell_id
    if not spell_id then return end

    local spell = spells[spell_id]
    if not spell then
        w:send_message(me, string.format("Unknown spell #%d.", spell_id), 0x0025)
        return
    end

    -- Block if already casting or have pending target.
    if pending_spell then
        w:send_message(me, "You are already doing something.", 0x0025)
        return
    end
    if active_cast then
        w:send_message(me, "You are already casting a spell.", 0x0025)
        return
    end

    local target_serial = cmd.target_serial or 0

    if spell.needs_target and target_serial == 0 then
        -- Need a target — show target cursor.
        local cursor_id = SPELL_CURSOR_BASE + spell.id
        local cursor_type = spell.harmful and 1 or 2
        w:send_target_cursor(me, cursor_id, cursor_type)
        pending_spell = {
            spell_def      = spell,
            caster_serial  = me,
            cursor_id      = cursor_id,
        }
        return
    end

    -- Pre-targeted spell (0xBF:0x002D) — skip cursor, begin cast immediately.
    if not spell.can_self and target_serial == me then
        w:send_message(me, "You can't target yourself with that spell.", 0x0025)
        return
    end

    if not begin_cast(spell, me, target_serial) then
        return
    end

    active_cast = {
        spell_def      = spell,
        caster_serial  = me,
        target_serial  = target_serial,
        finish_at      = clock() + spell.cast_delay_ms / 1000.0,
    }
end

--- Handle a target cursor response.
--- Returns true if the response was consumed by the spell system.
function handle_spell_target(cmd)
    if not pending_spell then return false end
    if cmd.cursor_id ~= pending_spell.cursor_id then return false end

    local ps = pending_spell
    pending_spell = nil

    -- Check for cancellation.
    local target_serial = cmd.target_serial or 0
    if target_serial == 0 then return true end  -- cancelled

    local spell = ps.spell_def

    if not spell.can_self and target_serial == ps.caster_serial then
        w:send_message(me, "You can't target yourself with that spell.", 0x0025)
        return true
    end

    if not begin_cast(spell, ps.caster_serial, target_serial) then
        return true
    end

    active_cast = {
        spell_def      = spell,
        caster_serial  = ps.caster_serial,
        target_serial  = target_serial,
        finish_at      = clock() + spell.cast_delay_ms / 1000.0,
    }
    return true
end

--- Check if active cast has completed, and if so, call complete_cast.
--- Called from the main loop.
function check_active_cast()
    if not active_cast then return end
    if clock() < active_cast.finish_at then return end

    local cast = active_cast
    active_cast = nil
    complete_cast(cast.spell_def, cast.caster_serial, cast.target_serial)
end

--- Returns the time of the next cast completion, or nil.
function next_cast_time()
    if active_cast then return active_cast.finish_at end
    return nil
end
