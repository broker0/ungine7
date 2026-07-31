-- controller/combat.lua — Melee combat system for player controller.
--
-- Target management and swing logic.
-- Depends on: constants/animations.lua, constants/sounds.lua,
--             constants/combat.lua, helpers.lua
--
-- Uses globals: w, me (set by main.lua)

-- ── Target state ─────────────────────────────────────────────────────────

targets        = {}    -- set of target serials: { [serial] = true }
primary_target = nil   -- current primary attack target serial
war_mode       = false -- peace/war mode
next_swing_at  = 0     -- clock() time of next allowed swing

-- ── Target management ────────────────────────────────────────────────────

function has_targets()
    return next(targets) ~= nil
end

function remove_target(serial)
    targets[serial] = nil
    if primary_target == serial then
        primary_target = next(targets)
    end
end

function clear_targets()
    targets = {}
    primary_target = nil
end

-- ── Melee swing ──────────────────────────────────────────────────────────

--- Attempt a melee swing at the primary target.
--- Checks range, LOS, plays animation/sound, deals damage.
function try_swing()
    if not primary_target then return end

    local self_info = w:get_entity(me)
    if not self_info then return end

    local target_info = w:get_entity(primary_target)
    if not target_info then
        remove_target(primary_target)
        return
    end

    -- Range check
    local dist = chebyshev(self_info.x, self_info.y, target_info.x, target_info.y)
    if dist > COMBAT.MELEE_RANGE then return end

    -- LOS check
    if not w:has_los(
        self_info.x, self_info.y, self_info.z + EYE_HEIGHT,
        target_info.x, target_info.y, target_info.z + EYE_HEIGHT
    ) then return end

    -- Swing animation + sound
    local self_mounted = entity_is_mounted(self_info)
    local attack_anim = resolve_animation(ANIM.SLASH_1H, self_mounted)
    if attack_anim then
        w:animate(me, attack_anim, 7, { repeat_count = 1 })
    end
    w:play_sound(SOUND.SWING, self_info.x, self_info.y, self_info.z)

    -- Deal damage
    local amount = random_range(DAMAGE.MIN, DAMAGE.MAX)
    local new_hp, killed = w:deal_damage(primary_target, amount)

    -- Hit sound + animation on target
    w:play_sound(SOUND.FIST_HIT, target_info.x, target_info.y, target_info.z)
    local target_mounted = entity_is_mounted(target_info)
    local hit_anim = resolve_animation(ANIM.GET_HIT, target_mounted)
    if hit_anim then
        w:animate(primary_target, hit_anim, 5, { repeat_count = 1 })
    end

    -- Gender-aware hurt sound on the target
    if target_info.graphic then
        local hurt_snd = random_hurt_sound(target_info.graphic)
        w:play_sound(hurt_snd, target_info.x, target_info.y, target_info.z)
    end

    if killed then
        remove_target(primary_target)
    end

    next_swing_at = clock() + COMBAT.SWING_DELAY_MS / 1000.0
end
