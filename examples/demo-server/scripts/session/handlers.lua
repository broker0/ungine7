-- session/handlers.lua — Packet and world-event dispatch.
--
-- Translates session events into game logic calls.
-- Supports both typed events (new) and legacy packet events (backward compat).
-- Depends on: constants.lua, helpers.lua, spells.lua, bandage.lua

-- ══════════════════════════════════════════════════════════════════════════
-- Scroll cursor ID base (must not collide with spell / bandage / skill)
-- ══════════════════════════════════════════════════════════════════════════

SCROLL_CURSOR_BASE = 0x5C500000

-- ══════════════════════════════════════════════════════════════════════════
-- Packet handlers
-- ══════════════════════════════════════════════════════════════════════════

--- Handle a CastSpell text command.
--- Accepts both typed event (ev.spell_id) and legacy (ev.command).
function handle_cast_spell(ev)
    local spell_id = ev.spell_id or tonumber(ev.command)
    if not spell_id then return end

    local spell = spells[spell_id]
    if not spell then
        -- Spell not in the session-local table — try the engine (full list).
        spell = engine:get_spell(spell_id)
        if not spell then
            session:send_system_message(string.format("Unknown spell #%d.", spell_id))
            return
        end
    end

    local my_serial = session:player_serial()
    local pos = session:player_position()

    if session:has_blocking_gump() then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are busy.") end
        return
    end
    if pending_spell or pending_bandage then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are already doing something.") end
        return
    end
    if active_cast then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are already casting a spell.") end
        return
    end
    if active_bandage then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You cannot cast spells while healing.") end
        return
    end

    if spell.needs_target then
        local cursor_id = SPELL_CURSOR_BASE + spell.id
        local cursor_type = spell.harmful and 1 or 2
        session:send_target_cursor(cursor_id, cursor_type)
        pending_spell = {
            spell_def = spell,
            caster_serial = my_serial,
            cursor_id = cursor_id,
        }
    end
end

--- Handle a scroll double-click: look up the item, match graphic to a spell,
--- show target cursor.  Returns true if consumed.
function handle_scroll_double_click(ev)
    local item_serial = ev.serial
    if not item_serial or item_serial == 0 then
        log("[scroll] no serial or serial=0")
        return false
    end
    if ev.paperdoll then return false end

    log(string.format("[scroll] looking up item 0x%08X", item_serial))

    -- Look up the item in containers (backpack).
    local info = engine:find_item_info(item_serial)
    if not info then
        log(string.format("[scroll] find_item_info returned nil for 0x%08X", item_serial))
        return false
    end

    log(string.format("[scroll] found item graphic=0x%04X", info.graphic))

    -- Match graphic to a spell scroll via the spells table.
    local spell = nil
    for _, s in pairs(spells) do
        if s.scroll_graphic and s.scroll_graphic == info.graphic then
            spell = s
            break
        end
    end
    if not spell then
        log(string.format("[scroll] no spell matches scroll graphic=0x%04X", info.graphic))
        return false
    end

    log(string.format("[scroll] matched spell: %s (id=%d)", spell.name, spell.id))

    local my_serial = session:player_serial()
    local pos = session:player_position()

    if session:has_blocking_gump() then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are busy.") end
        return true
    end
    if pending_spell or pending_bandage then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are already doing something.") end
        return true
    end
    if active_cast then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are already casting a spell.") end
        return true
    end
    if active_bandage then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You cannot cast spells while healing.") end
        return true
    end

    -- Show target cursor (even for self-targetable spells, matching UO).
    local cursor_id = SCROLL_CURSOR_BASE + spell.id
    local cursor_type = spell.harmful and 1 or 2
    session:send_target_cursor(cursor_id, cursor_type)

    pending_spell = {
        spell_def = spell,
        caster_serial = my_serial,
        cursor_id = cursor_id,
        scroll_item_serial = item_serial,  -- marks this as a scroll cast
    }

    log(string.format("0x%08X double-clicked scroll 0x%04X (%s) serial=0x%08X",
        my_serial, info.graphic, spell.name, item_serial))
    return true
end

--- Handle a spell target-cursor response.
--- Returns true if consumed.
function handle_spell_target(ev)
    if not pending_spell then return false end
    if ev.cursor_id ~= pending_spell.cursor_id then return false end

    local ps = pending_spell
    pending_spell = nil

    local cancelled = (ev.cursor_type == 3 or (ev.target_serial or 0) == 0)
    if cancelled then return true end

    local target_serial = ev.target_serial
    local spell = ps.spell_def

    if not spell.can_self and target_serial == ps.caster_serial then
        session:send_system_message("You can't target yourself with that spell.")
        return true
    end

    if not begin_cast(spell, ps.caster_serial, target_serial, ps.scroll_item_serial) then
        return true
    end

    -- Use scroll cast delay if available, otherwise normal cast delay.
    local is_scroll = (ps.scroll_item_serial ~= nil)
    local delay = spell.cast_delay_ms
    if is_scroll and spell.scroll_cast_delay_ms and spell.scroll_cast_delay_ms > 0 then
        delay = spell.scroll_cast_delay_ms
    end

    active_cast = {
        spell_def = spell,
        caster_serial = ps.caster_serial,
        target_serial = target_serial,
        delay_ms = delay,
        scroll_item_serial = ps.scroll_item_serial,
    }
    return true
end

--- Handle a CastTargetedSpell (0xBF:0x002D) — spell with pre-selected target.
function handle_cast_targeted_spell(ev)
    local spell_id = ev.spell_id
    local target_serial = ev.target_serial

    if not spell_id or not target_serial or target_serial == 0 then return end

    local spell = spells[spell_id]
    if not spell then
        spell = engine:get_spell(spell_id)
        if not spell then
            session:send_system_message(string.format("Unknown spell #%d.", spell_id))
            return
        end
    end

    local my_serial = session:player_serial()
    local pos = session:player_position()

    if session:has_blocking_gump() then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are busy.") end
        return
    end
    if pending_spell or pending_bandage then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are already doing something.") end
        return
    end
    if active_cast then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You are already casting a spell.") end
        return
    end
    if active_bandage then
        if pos then session:send_fizzle(my_serial, pos.x, pos.y, pos.z, "You cannot cast spells while healing.") end
        return
    end

    if not spell.can_self and target_serial == my_serial then
        session:send_system_message("You can't target yourself with that spell.")
        return
    end

    if not begin_cast(spell, my_serial, target_serial) then
        return
    end

    active_cast = {
        spell_def = spell,
        caster_serial = my_serial,
        target_serial = target_serial,
        delay_ms = spell.cast_delay_ms,
    }
end

--- Handle a TargetCursor response — dispatch to spell or bandage.
function handle_target_cursor(ev)
    if handle_spell_target(ev) then return end
    if handle_bandage_target(ev) then return end
end

-- ══════════════════════════════════════════════════════════════════════════
-- World event handlers
-- ══════════════════════════════════════════════════════════════════════════

--- Interrupt active spell cast when the player takes damage.
--- Also interrupts meditation.
function handle_damage_dealt(ev)
    local my_serial = session:player_serial()
    if ev.serial == my_serial then
        -- Interrupt spell cast.
        if active_cast then
            local caster = engine:get_entity(my_serial)
            if caster then
                session:send_fizzle(my_serial, caster.x, caster.y, caster.z, "The spell fizzles.")
            end
            active_cast = nil
        end
        -- Interrupt meditation.
        interrupt_meditation()
    end
end

--- Handle a UseSkill event.
function handle_use_skill(ev)
    local skill_id = ev.skill_id
    if not skill_id then
        -- Legacy format: "46 0" → parse first number.
        skill_id = ev.command and tonumber(ev.command:match("^(%d+)"))
    end
    if not skill_id then return end

    if session:has_blocking_gump() then
        session:send_system_message("You are busy.")
        return
    end

    if skill_id == SKILL_MEDITATION then
        toggle_meditation()
        return
    end

    -- Unknown / unimplemented skill — acknowledge with a gray message.
    session:send_system_message(string.format("You use skill #%d.", skill_id))
end

-- ══════════════════════════════════════════════════════════════════════════
-- Master event dispatch
-- ══════════════════════════════════════════════════════════════════════════

--- Process a single session event. Called from the main loop.
--- Handles both typed events (new format) and legacy packet events.
function dispatch_event(ev)
    if not ev then return end

    -- log(string.format("[dispatch] event type=%s", ev.type or "nil"))

    -- ── Typed events (new format) ────────────────────────────────
    if ev.type == "cast_spell" then
        interrupt_meditation()
        handle_cast_spell(ev)
        return
    elseif ev.type == "target_cursor" then
        interrupt_meditation()
        handle_target_cursor(ev)
        return
    elseif ev.type == "double_click" then
        interrupt_meditation()
        log(string.format("[dispatch] double_click serial=0x%08X paperdoll=%s",
            ev.serial or 0, tostring(ev.paperdoll)))
        if handle_scroll_double_click(ev) then return end
        handle_bandage_double_click(ev)
        return
    elseif ev.type == "cast_targeted_spell" then
        interrupt_meditation()
        handle_cast_targeted_spell(ev)
        return
    elseif ev.type == "use_skill" then
        handle_use_skill(ev)
        return
    end

    -- ── World events ─────────────────────────────────────────────
    if ev.type == "damage_dealt" then
        handle_damage_dealt(ev)
        return
    end

    -- ── Legacy packet events (backward compat) ───────────────────
    if ev.type == "packet" then
        if ev.id == 0x12 and ev.command_type == "CastSpell" then
            interrupt_meditation()
            handle_cast_spell(ev)
        elseif ev.id == 0x12 and ev.command_type == "UseSkill" then
            handle_use_skill(ev)
        elseif ev.id == 0x06 then
            interrupt_meditation()
            if not handle_scroll_double_click(ev) then
                handle_bandage_double_click(ev)
            end
        elseif ev.id == 0x6C then
            interrupt_meditation()
            handle_target_cursor(ev)
        end
        return
    end
end
