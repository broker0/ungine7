-- scene/props.lua: Prop class, multi-object helpers, and preset prop
-- definitions.
--
-- Loaded automatically by scene.lua.

-- ═══════════════════════════════════════════════════════════════════════
-- Multi-object helper
-- ═══════════════════════════════════════════════════════════════════════

--- Build a multi-object preset from an explicit list of part descriptors.
---
--- UO multi-objects have no consistent graphic-ID ordering — each object
--- arranges its tile IDs arbitrarily.  This helper lets you define the
--- layout once and reuse it via scene:spawn_prop().
---
--- Each entry is { graphic, dx, dy [, dz] [, color] }.
---
--- Usage:
---   PROPS.PENTAGRAM = multi_parts({
---       { 0x0FE6, 0, 1 },  -- left-center
---       { 0x0FE7, 0, 0 },  -- top-left
---       { 0x0FE8, 1, 0 },  -- top-center
---       ...
---   })
---
--- @param list  table  Array of {graphic, dx, dy [, dz] [, color]}.
--- @return table  Preset table with a `parts` array.
function multi_parts(list)
    local parts = {}
    for _, entry in ipairs(list) do
        parts[#parts + 1] = {
            graphic = entry[1],
            dx      = entry[2] or 0,
            dy      = entry[3] or 0,
            dz      = entry[4] or 0,
            color   = entry[5] or nil,
        }
    end
    return { parts = parts }
end


-- ═══════════════════════════════════════════════════════════════════════
-- Prop class (world items / decorations / "stage props")
-- ═══════════════════════════════════════════════════════════════════════
--
-- A Prop wraps one or more item serials.  Single-tile props behave
-- exactly as before; multi-tile props (created from presets with a
-- `parts` field) transparently manage every constituent tile.
--
-- The "anchor" is always the first serial — serial(), entity(), pos()
-- refer to it, so existing code that does `prop:pos()` or
-- `prop:serial()` keeps working unchanged.

Prop = {}
Prop.__index = Prop

--- Create a single-tile Prop (backward-compatible constructor).
function Prop.new(scene, serial)
    local self = setmetatable({}, Prop)
    self._scene = scene
    self._w = scene._w
    self._serial = serial      -- anchor serial
    self._serials = { serial } -- all part serials
    return self
end

--- Create a multi-tile Prop from an array of serials.
--- The first serial in the array is the anchor.
function Prop.new_multi(scene, serials)
    local self = setmetatable({}, Prop)
    self._scene = scene
    self._w = scene._w
    self._serial = serials[1]  -- anchor serial
    self._serials = serials
    return self
end

--- Return the anchor item serial.
function Prop:serial()
    return self._serial
end

--- Return the array of all part serials.
function Prop:serials()
    return self._serials
end

--- True if this prop consists of more than one tile.
function Prop:is_multi()
    return #self._serials > 1
end

--- Return the number of constituent parts.
function Prop:part_count()
    return #self._serials
end

--- Return the current entity data for the anchor.
function Prop:entity()
    return self._w:get_entity(self._serial)
end

--- Return the anchor prop's current position as x, y, z.
function Prop:pos()
    local e = self:entity()
    if e then return e.x, e.y, e.z end
    return 0, 0, 0
end

--- Play a sound at the anchor's location.
function Prop:play_sound(sound_id)
    local x, y, z = self:pos()
    self._w:play_sound(sound_id, x, y, z)
end

--- Play a graphical effect at the anchor's location.
function Prop:effect(graphic, opts)
    opts = opts or {}
    local x, y, z = self:pos()
    self._w:effect({
        direction_type = opts.direction_type or 2,
        graphic = graphic,
        x = x, y = y, z = z,
        speed = opts.speed or 10,
        duration = opts.duration or 30,
        fixed_direction = opts.fixed_direction or true,
    })
end

--- Remove this prop (all parts) from the world.
function Prop:remove()
    for _, s in ipairs(self._serials) do
        self._w:remove_entity(s)
    end
end


-- ═══════════════════════════════════════════════════════════════════════
-- Prop presets
-- ═══════════════════════════════════════════════════════════════════════
--
-- Single-tile presets:  { graphic=, [color=], [amount=] }
-- Multi-tile presets:   { parts = { {graphic=, dx=, dy=, [dz=], [color=]}, … } }
--
-- Use multi_parts({...}) to define multi-tile objects with explicit
-- per-part offsets (UO has no consistent graphic-ID ordering).
--
-- Pass directly to scene:spawn_prop() with x/y/z or relative {rx,ry,rz}:
--   scene:spawn_prop(PROPS.TORCH, {5, 3, 0})
--   scene:spawn_prop(PROPS.PENTAGRAM, {6, 1, 0})  -- spawns all 9 tiles

PROPS = {}

-- ── Light sources ───────────────────────────────────────────────────────
PROPS.TORCH           = { graphic = 0x0A12 }  -- standing torch (animated flame)
PROPS.TORCH_WALL      = { graphic = 0x0A28 }  -- wall torch (south)
PROPS.TORCH_WALL_E    = { graphic = 0x0A25 }  -- wall torch (east)
PROPS.CANDELABRA      = { graphic = 0x0B26 }  -- candelabra
PROPS.CANDLE          = { graphic = 0x0A0F }  -- candle
PROPS.LANTERN         = { graphic = 0x0A22 }  -- lantern (lit)
PROPS.CAMPFIRE        = { graphic = 0x0DE3 }  -- campfire (animated)
PROPS.CAMPFIRE_SMALL  = { graphic = 0x0DE7 }  -- small campfire
PROPS.BRAZIER         = { graphic = 0x0E31 }  -- brazier (lit)

-- ── Furniture ───────────────────────────────────────────────────────────
PROPS.CHAIR_WOOD      = { graphic = 0x0B4F }  -- wooden chair (south)
PROPS.CHAIR_FANCY     = { graphic = 0x0B56 }  -- fancy chair (south)
PROPS.BENCH           = { graphic = 0x0B2C }  -- wooden bench (south)
PROPS.THRONE          = { graphic = 0x0B33 }  -- throne (south)
PROPS.TABLE_WOOD      = { graphic = 0x0B7D }  -- wooden table
PROPS.TABLE_STONE     = { graphic = 0x1202 }  -- stone table
PROPS.BED             = { graphic = 0x0A63 }  -- bed (south)
PROPS.CHEST_WOOD      = { graphic = 0x0E43 }  -- wooden chest
PROPS.CHEST_METAL     = { graphic = 0x0E7C }  -- metal chest
PROPS.BOOKSHELF       = { graphic = 0x0A97 }  -- bookshelf

-- ── Containers / barrels ────────────────────────────────────────────────
PROPS.BARREL           = { graphic = 0x0E77 }  -- barrel
PROPS.CRATE_SMALL      = { graphic = 0x0E3C }  -- small crate
PROPS.CRATE_LARGE      = { graphic = 0x0E3D }  -- large crate
PROPS.SACK             = { graphic = 0x09B0 }  -- sack
PROPS.BASKET           = { graphic = 0x0990 }  -- basket

-- ── Nature / outdoor ────────────────────────────────────────────────────
PROPS.ROCK_SMALL       = { graphic = 0x1363 }  -- small rock
PROPS.ROCK_LARGE       = { graphic = 0x1367 }  -- large rock
PROPS.LOG              = { graphic = 0x1BE1 }  -- fallen log
PROPS.TREE_STUMP       = { graphic = 0x0E56 }  -- tree stump
PROPS.HAY_BALE         = { graphic = 0x0F36 }  -- hay bale
PROPS.MUSHROOM         = { graphic = 0x0D16 }  -- mushroom cluster
PROPS.FERN             = { graphic = 0x0C8F }  -- fern plant
PROPS.FLOWERS          = { graphic = 0x0C93 }  -- flowers

-- ── Signs / banners ─────────────────────────────────────────────────────
PROPS.SIGN_WOOD        = { graphic = 0x0BA3 }  -- wooden sign
PROPS.SIGN_HANGING     = { graphic = 0x0BC5 }  -- hanging sign
PROPS.BANNER_RED       = { graphic = 0x15AE }  -- red banner
PROPS.BANNER_BLUE      = { graphic = 0x15B0 }  -- blue banner

-- ── Gore / combat scenery ───────────────────────────────────────────────
PROPS.BLOOD_POOL       = { graphic = 0x122A }  -- blood pool
PROPS.BLOOD_SPLATTER   = { graphic = 0x122B }  -- blood splatter
PROPS.BONES_PILE       = { graphic = 0x1B09 }  -- pile of bones
PROPS.SKULL            = { graphic = 0x1AE0 }  -- skull
PROPS.GRAVESTONE       = { graphic = 0x1165 }  -- gravestone
PROPS.COFFIN           = { graphic = 0x0D52 }  -- coffin

-- ── Misc ────────────────────────────────────────────────────────────────
PROPS.ANVIL            = { graphic = 0x0FAF }  -- anvil
PROPS.FORGE            = { graphic = 0x0FB1 }  -- small forge
PROPS.CAULDRON         = { graphic = 0x0974 }  -- cauldron
PROPS.SPINNING_WHEEL   = { graphic = 0x1015 }  -- spinning wheel
PROPS.LOOM             = { graphic = 0x105F }  -- loom
PROPS.BOOK_OPEN        = { graphic = 0x0FF4 }  -- open book
PROPS.GOLD_PILE        = { graphic = 0x0EED }  -- pile of gold coins
PROPS.GEM_PILE         = { graphic = 0x0F19 }  -- pile of gems
PROPS.POTION_RED       = { graphic = 0x0F09 }  -- red potion
PROPS.POTION_BLUE      = { graphic = 0x0F0D }  -- blue potion

-- ── Multi-tile objects ──────────────────────────────────────────────────
--
-- UO multi-objects have arbitrary graphic-ID-to-tile mappings.
-- Each must be defined manually with explicit {graphic, dx, dy} offsets.

PROPS.PENTAGRAM = multi_parts({          -- pentagram 3x3, 0x0FE6–0x0FEE
    { 0x0FE6, -1,  0 },  --  +----+----+----+       anchor (0,0) = center tile
    { 0x0FE7, -1, -1 },  --  |  1 |  2 |  5 |
    { 0x0FE8,  0, -1 },  --  +----+----+----+
    { 0x0FE9, -1,  1 },  --  |  0 | *4 |  8 |  * = anchor
    { 0x0FEA,  0,  0 },  --  +----+----+----+
    { 0x0FEB,  1, -1 },  --  |  3 |  6 |  7 |
    { 0x0FEC,  0,  1 },  --  +----+----+----+
    { 0x0FED,  1,  1 },
    { 0x0FEE,  1,  0 },
})

-- ── Theatre / cathedral ─────────────────────────────────────────────────
PROPS.STOCKS           = { graphic = 0x1260 }  -- pillory / stocks
PROPS.BELL             = { graphic = 0x1C12 }  -- bell
PROPS.ALTAR            = { graphic = 0x12A5 }  -- stone altar
PROPS.ANKH             = { graphic = 0x0EDD }  -- ankh / cross
PROPS.RUG              = { graphic = 0x0AC3 }  -- carpet / rug
PROPS.GOBLET           = { graphic = 0x0995 }  -- goblet / chalice
PROPS.CURTAIN_S        = { graphic = 0x12DB }  -- curtain (south)
PROPS.CURTAIN_E        = { graphic = 0x12DD }  -- curtain (east)
PROPS.STATUE           = { graphic = 0x12CA }  -- stone statue
PROPS.PILLAR           = { graphic = 0x0C00 }  -- stone pillar
