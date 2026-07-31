-- spawn_monster.lua: Spawn a killable monster with loot.
--
-- Usage:  .lua scripts/spawn_monster.lua
--
-- Spawns a monster at a predefined location, attaches the monster_ctrl
-- controller for AI behaviour.  The monster can be killed by players
-- and will drop a corpse with equipment + loot table items.
--
-- Edit MONSTERS below to add more monsters or change spawn locations.

dofile("scripts/constants/layers.lua")
dofile("scripts/scene/outfit.lua")

local w = World(0)

-- ═════════════════════════════════════════════════════════════════════
-- Monster definitions
-- ═════════════════════════════════════════════════════════════════════
--
-- Each entry defines a monster to spawn.  Fields:
--   body        — BODY preset from outfit.lua (graphic + default name)
--   x, y, z    — spawn position
--   name        — display name (overrides BODY default)
--   color       — hue override (0 = use BODY default)
--   notoriety   — 1=Innocent, 3=Attackable, 5=Enemy, 6=Murderer
--   hits        — HP
--   items       — equipment table (optional)
--   aggro_range — detection range (default: 10)
--   leash_range — max chase distance from spawn (default: 20)
--   damage      — "min,max" melee damage (default: "5,15")
--   swing_delay — ms between attacks (default: 2500)

local MONSTERS = {
    -- ── Orc warrior near Britain ─────────────────────────────────
    {
        body = BODY.ORC,
        name = "an orc",
        x = 1440, y = 1700, z = 0,
        notoriety = 6,    -- Murderer (red name)
        hits = 80,
        color = 0,
        aggro_range = 10,
        leash_range = 20,
        damage = "8,18",
        swing_delay = 2500,
        items = {
            { graphic = 0x13B9, layer = 0x01, color = 0 },   -- leather cap
            { graphic = 0x13CC, layer = 0x0D, color = 0 },   -- leather tunic
            { graphic = 0x0F5E, layer = 0x02, color = 0 },   -- broadsword
        },
    },

    -- ── Skeleton near the road ───────────────────────────────────
    {
        body = BODY.SKELETON_AXE,
        name = "a skeleton",
        x = 1445, y = 1700, z = 0,
        notoriety = 6,
        hits = 50,
        color = 0,
        aggro_range = 8,
        leash_range = 15,
        damage = "4,12",
        swing_delay = 3000,
        items = {},
    },

    -- ── Troll on the hill ────────────────────────────────────────
    {
        body = BODY.TROLL,
        name = "a troll",
        x = 1450, y = 1700, z = 0,
        notoriety = 6,
        hits = 120,
        color = 0,
        aggro_range = 8,
        leash_range = 18,
        damage = "12,25",
        swing_delay = 3000,
        items = {},
    },
}

-- ═════════════════════════════════════════════════════════════════════
-- Spawn each monster
-- ═════════════════════════════════════════════════════════════════════

local spawned = {}

for _, def in ipairs(MONSTERS) do
    local body = def.body or BODY.ORC
    local name = def.name or body.name or "Monster"
    local color = def.color or body.color or 0
    local hits = def.hits or 100

    -- Spawn the mobile.
    local serial = w:spawn_npc({
        graphic   = body.graphic,
        x         = def.x,
        y         = def.y,
        z         = def.z,
        name      = name,
        color     = color,
        notoriety = def.notoriety or 6,
        hits      = hits,
        hits_max  = hits,
        items     = def.items or {},
    })

    -- Store AI configuration in item_props.meta so the controller
    -- script can read it.
    w:set_item_props(serial, {
        name = name,
        meta = {
            aggro_range  = tostring(def.aggro_range or 10),
            leash_range  = tostring(def.leash_range or 20),
            melee_damage = def.damage or "5,15",
            swing_delay  = tostring(def.swing_delay or 2500),
        },
    })

    -- Attach the monster controller script.
    w:attach_controller(serial, "monster_ctrl.lua")
    w:persist(serial)

    table.insert(spawned, serial)
    log(string.format("  [spawn] %s at (%d,%d,%d) serial=0x%08X hp=%d",
        name, def.x, def.y, def.z, serial, hits))
end

log(string.format("Spawned %d monsters", #spawned))
