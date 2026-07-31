-- session/regen.lua — Stat regeneration and meditation system.
--
-- Provides periodic HP / Mana / Stamina regeneration for the player session.
-- Meditation is an active skill (skill_id=46) that boosts mana regen and
-- is interrupted by damage, casting, or other actions.
--
-- Depends on: constants.lua (REGEN table)

-- ══════════════════════════════════════════════════════════════════════════
-- Skill IDs
-- ══════════════════════════════════════════════════════════════════════════

SKILL_MEDITATION = 46

-- ══════════════════════════════════════════════════════════════════════════
-- Meditation state
-- ══════════════════════════════════════════════════════════════════════════

is_meditating = false

--- Start meditation. Returns a system message.
function start_meditation()
    is_meditating = true
    return "You enter a meditative trance."
end

--- Stop meditation if active. Returns a message or nil.
function stop_meditation()
    if is_meditating then
        is_meditating = false
        return "You stop meditating."
    end
    return nil
end

--- Toggle meditation (UseSkill handler).
function toggle_meditation()
    if is_meditating then
        local msg = stop_meditation()
        if msg then
            session:send_system_message(msg)
        end
    else
        local msg = start_meditation()
        session:send_system_message(msg)
    end
end

--- Interrupt meditation due to action/damage.
--- Sends a system message if meditation was active.
function interrupt_meditation()
    local msg = stop_meditation()
    if msg then
        session:send_system_message(msg)
    end
end

-- ══════════════════════════════════════════════════════════════════════════
-- Regen tick
-- ══════════════════════════════════════════════════════════════════════════

--- Perform one regen tick: restore HP, mana (with meditation bonus), stamina.
function regen_tick()
    local my_serial = session:player_serial()

    -- HP regen
    engine:heal_entity(my_serial, REGEN.HP_PER_TICK)

    -- Mana regen (boosted by meditation)
    local mana_amount = REGEN.MANA_PER_TICK
    if is_meditating then
        mana_amount = mana_amount + REGEN.MEDITATION_BONUS
    end
    engine:modify_mana(my_serial, mana_amount)

    -- Stamina regen
    engine:modify_stamina(my_serial, REGEN.STAM_PER_TICK)
end
