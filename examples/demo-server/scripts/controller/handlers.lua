-- controller/handlers.lua — Command and event dispatch for player controller.
--
-- Translates player commands and world events into game logic calls.
-- Depends on: constants.lua, helpers.lua, combat.lua, regen.lua, spells.lua
--
-- Uses globals: w, me (set by main.lua)

-- ══════════════════════════════════════════════════════════════════════════
-- Command handlers
-- ══════════════════════════════════════════════════════════════════════════

--- Process a single player command.
function handle_command(cmd)
    if not cmd then return end

    if cmd.type == "attack" then
        local t = cmd.target_serial
        if t and t ~= 0 and t ~= me then
            targets[t] = true
            primary_target = t
            -- Try immediate swing if charged
            if clock() >= next_swing_at then
                try_swing()
            end
        end

    elseif cmd.type == "cancel_attack" then
        clear_targets()

    elseif cmd.type == "toggle_war_mode" then
        war_mode = cmd.fighting
        if not war_mode then
            clear_targets()
        end

    elseif cmd.type == "move" then
        -- Movement is handled by the session (infra) and the engine.
        -- The controller receives it but doesn't need to do anything
        -- for now — the engine already moved us.

    elseif cmd.type == "cast_spell" then
        handle_cast_spell(cmd)

    elseif cmd.type == "use_skill" then
        -- TODO: implement skill use in controller
        w:send_message(me, "Skills not yet implemented in controller mode.", 0x0035)

    elseif cmd.type == "target_response" then
        -- Try spell target first
        if handle_spell_target(cmd) then return end
        -- Other target consumers can go here (bandage, skill, etc.)
        log("unhandled target response: cursor_id=" .. tostring(cmd.cursor_id))
    end
end

-- ══════════════════════════════════════════════════════════════════════════
-- Event handlers
-- ══════════════════════════════════════════════════════════════════════════

--- Process a single world event.
function handle_event(ev)
    if not ev then return end

    if ev.type == "damage_received" then
        -- Auto-retaliate
        local src = ev.source_serial
        if src and src ~= 0 and src ~= me then
            targets[src] = true
            if not primary_target then
                primary_target = src
            end
        end
        -- Interrupt spell cast on damage
        if active_cast then
            spell_fizzle_on_damage()
            active_cast = nil
        end

    elseif ev.type == "timer_fired" then
        -- Not using scheduler timers yet; using clock() instead.
    end
end

--- Fizzle active cast when taking damage.
function spell_fizzle_on_damage()
    local caster = w:get_entity(me)
    if caster then
        w:play_sound(SOUND.FIZZLE, caster.x, caster.y, caster.z)
        w:effect({
            direction_type = 3,
            source_serial  = me,
            target_serial  = 0,
            graphic        = EFFECT.FIZZLE,
            x = caster.x, y = caster.y, z = caster.z,
            speed = 10, duration = 15,
            fixed_direction = false,
            explode = false,
        })
    end
    w:send_message(me, "The spell fizzles.", 0x0025)
end
