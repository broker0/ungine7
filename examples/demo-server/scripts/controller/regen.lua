-- controller/regen.lua — Stat regeneration for player controller.
--
-- Periodic HP / Mana / Stamina regeneration.
-- Depends on: constants/regen.lua
--
-- Uses globals: w, me (set by main.lua)

-- ── Regen state ──────────────────────────────────────────────────────────

next_regen_at = 0  -- clock() time of next regen tick

-- ── Regen tick ───────────────────────────────────────────────────────────

--- Perform one regen tick: restore HP, mana, stamina.
function regen_tick()
    local info = w:get_entity(me)
    if not info then return end

    if info.hits and info.hits_max and info.hits < info.hits_max then
        w:heal_entity(me, REGEN.HP_PER_TICK)
    end
    w:modify_mana(me, REGEN.MANA_PER_TICK)
    w:modify_stamina(me, REGEN.STAM_PER_TICK)

    next_regen_at = clock() + REGEN.TICK_MS / 1000.0
end
