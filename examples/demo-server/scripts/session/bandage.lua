-- session/bandage.lua — Bandage healing system.
--
-- Handles:
--   handle_bandage_double_click() — detect bandage item, show target cursor
--   handle_bandage_target()       — validate target, start timer
--   complete_bandage()            — re-check range/LOS, consume, heal
--
-- Depends on: constants.lua, helpers.lua

-- ══════════════════════════════════════════════════════════════════════════
-- Double-click handler
-- ══════════════════════════════════════════════════════════════════════════

--- Check if a DoubleClick event targets a bandage item.
--- If so, show a target cursor and set pending_bandage.
--- Returns true if consumed (even on error).
function handle_bandage_double_click(ev)
    local item_serial = ev.serial
    if not item_serial or item_serial == 0 then
        log("[bandage] no serial or serial=0")
        return false
    end
    if ev.paperdoll then return false end

    log(string.format("[bandage] looking up item 0x%08X", item_serial))

    local my_serial = session:player_serial()
    local pos = session:player_position()
    if not pos then
        log("[bandage] no player position")
        return false
    end

    -- Look up item info.
    local info = engine:find_item_info(item_serial)

    local is_bandage_item = false
    if info then
        log(string.format("[bandage] find_item_info: graphic=0x%04X (bandage=0x%04X)", info.graphic, BANDAGE_GRAPHIC))
        is_bandage_item = (info.graphic == BANDAGE_GRAPHIC)
    else
        log(string.format("[bandage] find_item_info returned nil, trying get_entity"))
        -- Not in a container — check as ground entity.
        local ent = engine:get_entity(item_serial)
        if ent and ent.type == "item" and ent.graphic == BANDAGE_GRAPHIC then
            if chebyshev(pos.x, pos.y, ent.x, ent.y) > BANDAGE.RANGE then
                session:send_system_message("That is too far away.")
                return true
            end
            is_bandage_item = true
        else
            log(string.format("[bandage] get_entity: %s", ent and ent.type or "nil"))
        end
    end

    if not is_bandage_item then
        log("[bandage] not a bandage item")
        return false
    end

    log("[bandage] is bandage, checking state...")

    -- Block if busy.
    if pending_spell or pending_bandage then
        session:send_system_message("You are already doing something.")
        return true
    end
    if active_bandage then
        session:send_system_message("You are already applying bandages.")
        return true
    end

    -- Show helpful target cursor.
    local cursor_id = BANDAGE_CURSOR_BASE + (item_serial % 0x10000)
    log(string.format("[bandage] sending target cursor id=0x%08X", cursor_id))
    session:send_target_cursor(cursor_id, 2)

    pending_bandage = {
        healer_serial = my_serial,
        bandage_item_serial = item_serial,
        cursor_id = cursor_id,
    }

    session:send_system_message("Who would you like to heal?")
    return true
end

-- ══════════════════════════════════════════════════════════════════════════
-- Target cursor handler
-- ══════════════════════════════════════════════════════════════════════════

--- Handle a bandage target-cursor response.
--- Returns true if consumed.
function handle_bandage_target(ev)
    if not pending_bandage then return false end
    if ev.cursor_id ~= pending_bandage.cursor_id then return false end

    local pb = pending_bandage
    pending_bandage = nil

    -- Cancelled.
    local cancelled = (ev.cursor_type == 3 or (ev.target_serial or 0) == 0)
    if cancelled then return true end

    local target_serial = ev.target_serial
    local pos = session:player_position()
    if not pos then return true end

    -- Target must be a mobile.
    local target = engine:get_entity(target_serial)
    if not target or target.type ~= "mobile" then
        session:send_system_message("You can only use bandages on a living creature.")
        return true
    end

    -- Full HP check.
    if target.hits >= target.hits_max then
        session:send_system_message("The patient seems to be quite all right")
        return true
    end

    -- Distance check.
    if chebyshev(pos.x, pos.y, target.x, target.y) > BANDAGE.RANGE then
        session:send_system_message("That is too far away.")
        return true
    end

    -- LOS check.
    if not engine:has_los(
        pos.x, pos.y, pos.z + EYE_HEIGHT,
        target.x, target.y, target.z + EYE_HEIGHT
    ) then
        session:send_system_message("Target cannot be seen.")
        return true
    end

    -- Start bandage timer.
    active_bandage = {
        healer_serial = pb.healer_serial,
        target_serial = target_serial,
        bandage_item_serial = pb.bandage_item_serial,
        delay_ms = BANDAGE.DELAY_MS,
    }
    return true
end

-- ══════════════════════════════════════════════════════════════════════════
-- Completion
-- ══════════════════════════════════════════════════════════════════════════

--- Complete a bandage action: re-check range/LOS, consume bandage, heal.
function complete_bandage(healer_serial, target_serial, bandage_item_serial)
    local healer = engine:get_entity(healer_serial)
    if not healer then
        session:send_system_message("You are unable to apply bandages.")
        return
    end

    local target = engine:get_entity(target_serial)
    if not target then
        session:send_system_message("Your target is no longer there.")
        return
    end

    if target.hits >= target.hits_max then
        session:send_system_message("The patient seems to be quite all right")
        return
    end

    if chebyshev(healer.x, healer.y, target.x, target.y) > BANDAGE.RANGE then
        session:send_system_message("You cannot reach the target.")
        return
    end

    if healer_serial ~= target_serial then
        if not engine:has_los(
            healer.x, healer.y, healer.z + EYE_HEIGHT,
            target.x, target.y, target.z + EYE_HEIGHT
        ) then
            session:send_system_message("Target cannot be seen.")
            return
        end
    end

    local consumed = engine:consume_item(bandage_item_serial, 1, BANDAGE_GRAPHIC)
    if not consumed then
        session:send_system_message("You have no bandages left.")
        return
    end

    local heal_amount = random_range(BANDAGE.HEAL_MIN, BANDAGE.HEAL_MAX)
    engine:heal_entity(target_serial, heal_amount)
    broadcast:sound(BANDAGE.SOUND, target.x, target.y, target.z)
    session:send_system_message("You place a bloody bandage in your backpack")

    log(string.format("0x%08X bandaged 0x%08X for %d HP", healer_serial, target_serial, heal_amount))
end
