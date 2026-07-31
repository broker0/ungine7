-- spawn_movement_npcs.lua: Spawn N movement-benchmark NPCs and attach
-- a movement_ctrl.lua controller to each one.
--
-- This script runs in ASYNC mode (--lua-script).  Each NPC gets its
-- own coroutine running movement_ctrl.lua as a controller script,
-- which replicates the steady-state movement behaviour of the Rust
-- bench-movement benchmark (random walk series + optional pause).
--
-- Usage:
--   demo-server --log world.uolog \
--     --lua-script scripts/spawn_movement_npcs.lua
--
-- All NPCs are removed when the script stops or is hot-reloaded.
--
-- ======================= Configuration ============================

-- Number of NPCs to spawn.
local NPC_COUNT = 10

-- Body graphic and color — match the test/bench player characters.
-- 0x0190 = human male body.
-- 0x0481 = tan hue, the same color used by test accounts (constants::hue::TEST_PLAYER).
local NPC_GRAPHIC = 0x0190
local NPC_COLOR   = 0x0481

-- Mount item graphic equipped on Layer::Mount (0x19).
-- 0x3E9F = the horse mount item used by test player entities (constants::item::HORSE_MOUNT).
-- The mount's color matches the rider color so it looks uniform.
local MOUNT_GRAPHIC = 0x3E9F
local MOUNT_COLOR   = NPC_COLOR
local MOUNT_LAYER   = 0x19    -- LAYER.MOUNT

-- Spawn region: NPCs scatter randomly within this rectangle.
-- By default this is centred on Britain bank (1438, 1696) — the same
-- area used by bench-movement / WanderController.
-- Z is resolved automatically via w:resolve_z(); the centre Z is only
-- used as a fallback when all resolution attempts fail.
local SPAWN_CENTER_X = 1438
local SPAWN_CENTER_Y = 1696
local SPAWN_RADIUS   = 20    -- half-width/height of the scatter area

-- Map ID (0 = Felucca / Trammel in standard UO data).
local MAP_ID = 0

-- Path to the controller script, relative to the scripts/ directory.
-- This is joined to "scripts/" internally by attach_controller().
local CONTROLLER_PATH = "movement_ctrl.lua"

-- Per-NPC movement configuration forwarded to movement_ctrl.lua via the
-- MOVEMENT_CFG global.  Set to nil to use defaults.
--
-- Note: this table is set as a Lua global BEFORE the controller script
-- is loaded.  Because each controller runs in its own Lua VM instance
-- (LuaController owns the VM), the global is private to each NPC.
--
-- If you want each NPC to have different settings, see the per-NPC
-- override section inside spawn_npcs() below.
local MOVEMENT_CFG_DEFAULT = {
    run_len_min   = 3,
    run_len_max   = 20,
    pause_min_ms  = 2000,
    pause_max_ms  = 5000,
    step_delay_ms = 100,
    run_flag      = true,    -- running (direction | 0x80)
}

-- Stagger delay (ms) between successive NPC spawns to avoid flooding the
-- world worker with simultaneous attach_controller commands.
local SPAWN_STAGGER_MS = 50

-- NPC name prefix — each NPC is named "<PREFIX> N".
local NPC_NAME_PREFIX = "Wanderer"

-- ===================== (end of configuration) =====================

local w = World(MAP_ID)

log(string.format(
    "[spawn_movement] starting: count=%d  graphic=0x%04X  color=0x%04X  mount=0x%04X  area=(%d..%d, %d..%d)  ctrl=%s",
    NPC_COUNT, NPC_GRAPHIC, NPC_COLOR, MOUNT_GRAPHIC,
    SPAWN_CENTER_X - SPAWN_RADIUS, SPAWN_CENTER_X + SPAWN_RADIUS,
    SPAWN_CENTER_Y - SPAWN_RADIUS, SPAWN_CENTER_Y + SPAWN_RADIUS,
    CONTROLLER_PATH))

-- Track spawned serials so the cleanup callback can remove them all.
local spawned_serials = {}

-- Cleanup: remove every NPC we spawned when the script stops / reloads.
register_cleanup(function()
    log(string.format("[spawn_movement] cleanup: removing %d NPCs", #spawned_serials))
    for _, serial in ipairs(spawned_serials) do
        w:remove_entity(serial)
    end
end)

-- ── Helpers ──────────────────────────────────────────────────────────────

--- Find a valid spawn position near (cx, cy) within radius tiles.
--- Mirrors the logic in game_session/spawn.rs `resolve_test_account_spawn`:
---   * Try up to max_attempts random positions in [cx±radius, cy±radius].
---   * For each candidate call w:resolve_z(x, y, 0, 0) — returns the
---     standing Z on passable ground, or nil for water/rock/void.
---   * If all attempts fail, fall back to (cx, cy, 0).
---
--- Returns x, y, z.
local function find_valid_spawn(cx, cy, radius, max_attempts)
    local attempts = max_attempts or 50
    for _ = 1, attempts do
        local x = cx + math.random(-radius, radius)
        local y = cy + math.random(-radius, radius)
        local z = w:resolve_z(x, y, 0, 0)
        if z then
            return x, y, z
        end
    end
    -- Fallback to centre (may be impassable, but avoids silent failure).
    log(string.format("[spawn_movement] warning: no valid ground found after %d attempts, using fallback (%d,%d)",
        attempts, cx, cy))
    return cx, cy, 0
end

-- ── Spawn loop ───────────────────────────────────────────────────────────

local function spawn_npcs()
    for i = 1, NPC_COUNT do
        -- Find a valid (passable, above ground) spawn position.
        local x, y, z = find_valid_spawn(
            SPAWN_CENTER_X, SPAWN_CENTER_Y, SPAWN_RADIUS, 50)

        local name = NPC_NAME_PREFIX .. " " .. i

        -- Spawn the NPC in async mode.
        -- color=NPC_COLOR  matches test/bench player hue (0x0481 = tan).
        -- items table equips the horse mount on Layer::Mount (0x19).
        local serial = w:spawn_npc({
            graphic   = NPC_GRAPHIC,
            x         = x,
            y         = y,
            z         = z,
            name      = name,
            color     = NPC_COLOR,
            direction = math.random(0, 7),
            notoriety = 1,   -- innocent (blue)
            hits      = 100,
            hits_max  = 100,
            items     = {
                { graphic = MOUNT_GRAPHIC, layer = MOUNT_LAYER, color = MOUNT_COLOR },
            },
        })

        table.insert(spawned_serials, serial)
        log(string.format("[spawn_movement] spawned 0x%08X '%s' at (%d,%d,%d)",
            serial, name, x, y, z))

        -- Per-NPC movement config override example:
        --   Uncomment and edit to give each NPC unique parameters.
        --
        -- local npc_cfg = {
        --     run_len_min   = MOVEMENT_CFG_DEFAULT.run_len_min + i,
        --     run_len_max   = MOVEMENT_CFG_DEFAULT.run_len_max + i * 2,
        --     pause_min_ms  = MOVEMENT_CFG_DEFAULT.pause_min_ms,
        --     pause_max_ms  = MOVEMENT_CFG_DEFAULT.pause_max_ms,
        --     step_delay_ms = MOVEMENT_CFG_DEFAULT.step_delay_ms,
        --     run_flag      = MOVEMENT_CFG_DEFAULT.run_flag,
        -- }

        -- Attach the controller script.
        -- attach_controller(serial, path) loads movement_ctrl.lua into a
        -- brand-new Lua VM for this NPC and registers it as its
        -- EntityController.  The path is relative to scripts/.
        w:attach_controller(serial, CONTROLLER_PATH)
        log(string.format("[spawn_movement] 0x%08X  controller attached (%s)",
            serial, CONTROLLER_PATH))

        -- Stagger spawns to avoid a burst of simultaneous engine commands.
        if i < NPC_COUNT then
            sleep(SPAWN_STAGGER_MS)
        end
    end

    log(string.format("[spawn_movement] all %d NPCs spawned and running.", NPC_COUNT))
end

spawn_npcs()

-- ── Monitoring loop ──────────────────────────────────────────────────────
-- Keep the script alive and print a periodic headcount.
-- Also drains world events (entity_removed, etc.) to keep the log quiet.

local MONITOR_INTERVAL_MS = 10000   -- print stats every 10 s
local alive_count = NPC_COUNT

while true do
    -- Wait for events or timeout.
    local ev = wait_event(MONITOR_INTERVAL_MS)
    if ev then
        -- Log entity removals so we know when NPCs disappear.
        if ev.type == "entity_removed" then
            for idx, serial in ipairs(spawned_serials) do
                if serial == ev.serial then
                    alive_count = alive_count - 1
                    log(string.format("[spawn_movement] 0x%08X removed (alive=%d/%d)",
                        ev.serial, alive_count, NPC_COUNT))
                    table.remove(spawned_serials, idx)
                    break
                end
            end
        end
        -- Drain any remaining buffered events without sleeping.
        while poll_event() do end
    else
        -- Periodic status line.
        log(string.format("[spawn_movement] status: %d/%d NPCs alive",
            alive_count, NPC_COUNT))
    end
end
