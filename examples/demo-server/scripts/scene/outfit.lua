-- scene/outfit.lua: Body presets, Outfit builder, and preset outfits.
--
-- Loaded automatically by scene.lua.
-- Depends on: constants/layers.lua (LAYER table)

-- ═══════════════════════════════════════════════════════════════════════
-- Body presets
-- ═══════════════════════════════════════════════════════════════════════
--
-- A body preset is a table with { graphic, [color], [name] } that can
-- be passed to spawn_actor as the `body` field.  The preset's fields
-- are used as defaults — any explicit params override them.
--
-- Usage:
--   scene:spawn_actor({ body = BODY.MALE, name = "Guard", at = {0,0,0} })
--   scene:spawn_actor({ body = BODY.SKELETON, at = {5, 0, 0} })

BODY = {}

-- ── Humans ──────────────────────────────────────────────────────────────

BODY.MALE           = { graphic = 0x0190, name = "Man" }
BODY.FEMALE         = { graphic = 0x0191, name = "Woman" }

-- Skin-toned variants (common UO skin hues)
BODY.MALE_LIGHT     = { graphic = 0x0190, color = 0x03EA, name = "Man" }
BODY.MALE_TAN       = { graphic = 0x0190, color = 0x0481, name = "Man" }
BODY.MALE_DARK      = { graphic = 0x0190, color = 0x044E, name = "Man" }
BODY.FEMALE_LIGHT   = { graphic = 0x0191, color = 0x03EA, name = "Woman" }
BODY.FEMALE_TAN     = { graphic = 0x0191, color = 0x0481, name = "Woman" }
BODY.FEMALE_DARK    = { graphic = 0x0191, color = 0x044E, name = "Woman" }

-- ── Undead ──────────────────────────────────────────────────────────────

BODY.SKELETON       = { graphic = 0x0032, name = "Skeleton" }
BODY.SKELETON_AXE   = { graphic = 0x0039, name = "Skeleton" }     -- armed skeleton
BODY.SKELETON_MAGE  = { graphic = 0x0094, name = "Skeleton Mage" }
BODY.ZOMBIE         = { graphic = 0x0003, name = "Zombie" }
BODY.LICH           = { graphic = 0x0018, name = "Lich" }
BODY.LICH_LORD      = { graphic = 0x004E, name = "Lich Lord" }
BODY.MUMMY          = { graphic = 0x009A, name = "Mummy" }
BODY.SPECTRE        = { graphic = 0x001A, name = "Spectre" }
BODY.WRAITH         = { graphic = 0x001A, color = 0x4001, name = "Wraith" }  -- translucent

-- ── Monsters ────────────────────────────────────────────────────────────

BODY.DRAGON         = { graphic = 0x003B, name = "Dragon" }
BODY.DRAGON_RED     = { graphic = 0x003B, color = 0x0021, name = "Dragon" }
BODY.DRAKE          = { graphic = 0x003C, name = "Drake" }
BODY.DAEMON         = { graphic = 0x000A, name = "Daemon" }
BODY.BALRON         = { graphic = 0x000A, color = 0x0021, name = "Balron" }  -- red daemon
BODY.ORC            = { graphic = 0x0011, name = "Orc" }
BODY.ORC_LORD       = { graphic = 0x0006, name = "Orc Lord" }
BODY.TROLL          = { graphic = 0x0036, name = "Troll" }
BODY.ETTIN          = { graphic = 0x0002, name = "Ettin" }
BODY.OGRE           = { graphic = 0x0001, name = "Ogre" }
BODY.TITAN          = { graphic = 0x004B, name = "Titan" }
BODY.CYCLOPS        = { graphic = 0x004A, name = "Cyclops" }
BODY.GARGOYLE       = { graphic = 0x0004, name = "Gargoyle" }

-- ── Animals ─────────────────────────────────────────────────────────────

BODY.HORSE          = { graphic = 0x00C8, name = "Horse" }
BODY.HORSE_BROWN    = { graphic = 0x00C8, color = 0x0455, name = "Horse" }
BODY.DEER           = { graphic = 0x00ED, name = "Deer" }
BODY.WOLF           = { graphic = 0x0009, name = "Wolf" }
BODY.BEAR           = { graphic = 0x00D3, name = "Bear" }
BODY.GREAT_HART     = { graphic = 0x00EA, name = "Great Hart" }
BODY.EAGLE          = { graphic = 0x0005, name = "Eagle" }
BODY.SNAKE          = { graphic = 0x0034, name = "Snake" }
BODY.GIANT_SPIDER   = { graphic = 0x001C, name = "Giant Spider" }
BODY.SLIME          = { graphic = 0x0033, name = "Slime" }

-- ── Elemental / Magic ───────────────────────────────────────────────────

BODY.EARTH_ELEMENTAL = { graphic = 0x000E, name = "Earth Elemental" }
BODY.FIRE_ELEMENTAL  = { graphic = 0x000F, name = "Fire Elemental" }
BODY.WATER_ELEMENTAL = { graphic = 0x0010, name = "Water Elemental" }
BODY.AIR_ELEMENTAL   = { graphic = 0x000D, name = "Air Elemental" }
BODY.WISP           = { graphic = 0x0058, name = "Wisp" }

-- ═══════════════════════════════════════════════════════════════════════
-- Outfit builder
-- ═══════════════════════════════════════════════════════════════════════

--- An Outfit is a reusable set of equipment items.
--- Build one with Outfit(), add pieces, then pass to spawn_actor or equip.
---
--- Usage:
---   local guard_outfit = Outfit()
---       :shirt(0x1517, 0x0455)
---       :pants(0x152E)
---       :shoes(0x170B, 0x0)
---       :helmet(0x1412, 0x0455)
---       :right_hand(0x0F5C)        -- mace
---       :left_hand(0x1B76)         -- shield
---       :hair(0x203C, 0x044E)
---       :cloak(0x1515, 0x0455)

Outfit = {}
Outfit.__index = Outfit

function Outfit.new()
    local self = setmetatable({}, Outfit)
    self._items = {}
    return self
end

setmetatable(Outfit, { __call = function(cls) return cls.new() end })

--- Add a raw equipment item: { graphic=, layer=, [color=] }.
function Outfit:add(layer, graphic, color)
    table.insert(self._items, {
        graphic = graphic,
        layer = layer,
        color = color,
    })
    return self  -- chainable
end

--- Return the items list (for passing to spawn_npc).
function Outfit:items()
    return self._items
end

--- Create a copy of this outfit (so modifying doesn't affect original).
function Outfit:clone()
    local copy = Outfit()
    for _, item in ipairs(self._items) do
        copy:add(item.layer, item.graphic, item.color)
    end
    return copy
end

--- Merge another outfit's items into this one (overwriting same-layer).
function Outfit:merge(other)
    local by_layer = {}
    for i, item in ipairs(self._items) do
        by_layer[item.layer] = i
    end
    for _, item in ipairs(other:items()) do
        local idx = by_layer[item.layer]
        if idx then
            self._items[idx] = { graphic = item.graphic, layer = item.layer, color = item.color }
        else
            table.insert(self._items, { graphic = item.graphic, layer = item.layer, color = item.color })
            by_layer[item.layer] = #self._items
        end
    end
    return self
end

-- ── Convenience methods for each layer ──────────────────────────────────

function Outfit:right_hand(graphic, color) return self:add(LAYER.RIGHT_HAND, graphic, color) end
function Outfit:left_hand(graphic, color)  return self:add(LAYER.LEFT_HAND, graphic, color) end
function Outfit:shoes(graphic, color)      return self:add(LAYER.SHOES, graphic, color) end
function Outfit:pants(graphic, color)      return self:add(LAYER.PANTS, graphic, color) end
function Outfit:shirt(graphic, color)      return self:add(LAYER.SHIRT, graphic, color) end
function Outfit:helmet(graphic, color)     return self:add(LAYER.HELMET, graphic, color) end
function Outfit:gloves(graphic, color)     return self:add(LAYER.GLOVES, graphic, color) end
function Outfit:ring(graphic, color)       return self:add(LAYER.RING, graphic, color) end
function Outfit:necklace(graphic, color)   return self:add(LAYER.NECKLACE, graphic, color) end
function Outfit:hair(graphic, color)       return self:add(LAYER.HAIR, graphic, color) end
function Outfit:waist(graphic, color)      return self:add(LAYER.WAIST, graphic, color) end
function Outfit:torso(graphic, color)      return self:add(LAYER.TORSO, graphic, color) end
function Outfit:beard(graphic, color)      return self:add(LAYER.BEARD, graphic, color) end
function Outfit:tunic(graphic, color)      return self:add(LAYER.TUNIC, graphic, color) end
function Outfit:arms(graphic, color)       return self:add(LAYER.ARMS, graphic, color) end
function Outfit:cloak(graphic, color)      return self:add(LAYER.CLOAK, graphic, color) end
function Outfit:robe(graphic, color)       return self:add(LAYER.ROBE, graphic, color) end
function Outfit:skirt(graphic, color)      return self:add(LAYER.SKIRT, graphic, color) end
function Outfit:legs(graphic, color)       return self:add(LAYER.LEGS, graphic, color) end
function Outfit:mount(graphic, color)      return self:add(LAYER.MOUNT, graphic, color) end
function Outfit:backpack(graphic, color)   return self:add(LAYER.BACKPACK, graphic, color) end
function Outfit:face(graphic, color)       return self:add(LAYER.FACE, graphic, color) end

-- ═══════════════════════════════════════════════════════════════════════
-- Preset outfits
-- ═══════════════════════════════════════════════════════════════════════

OUTFITS = {}

--- Plate-armored guard with halberd.
OUTFITS.PLATE_GUARD = Outfit()
    :hair(0x203C, 0x044E)
    :shirt(0x1517)
    :pants(0x152E)
    :shoes(0x170B)
    :torso(0x1415)           -- plate chest
    :arms(0x1410)            -- plate arms
    :gloves(0x1414)          -- plate gloves
    :legs(0x1411)            -- plate legs
    :helmet(0x1412)          -- close helmet
    :right_hand(0x143E)      -- halberd

--- Chainmail guard with sword & shield.
OUTFITS.CHAIN_GUARD = Outfit()
    :hair(0x203C, 0x044E)
    :shirt(0x1517)
    :pants(0x152E)
    :shoes(0x170B)
    :tunic(0x13BF)           -- chainmail tunic
    :legs(0x13BE)            -- chainmail leggings
    :gloves(0x13C6)          -- leather gloves
    :helmet(0x1412)          -- close helmet
    :right_hand(0x0F5E)      -- broadsword
    :left_hand(0x1B76)       -- metal kite shield

--- Leather-clad rogue.
OUTFITS.ROGUE = Outfit()
    :hair(0x2048, 0x0386)
    :shirt(0x1517, 0x0455)
    :pants(0x152E, 0x0455)
    :shoes(0x170B, 0x0455)
    :torso(0x13CC, 0x0455)   -- leather chest
    :gloves(0x13C6, 0x0455)  -- leather gloves
    :cloak(0x1515, 0x0455)
    :right_hand(0x0F52)      -- dagger

--- Simple commoner / peasant.
OUTFITS.PEASANT = Outfit()
    :hair(0x203B, 0x044E)
    :shirt(0x1517, 0x0384)
    :pants(0x152E, 0x0253)
    :shoes(0x170F)           -- sandals

--- Fancy noble.
OUTFITS.NOBLE = Outfit()
    :hair(0x203C, 0x044E)
    :shirt(0x1EFD, 0x0455)   -- fancy shirt
    :pants(0x152E, 0x0455)
    :shoes(0x170B, 0x0455)
    :cloak(0x1515, 0x0455)
    :necklace(0x1088)

--- Mage / wizard with robe and staff.
OUTFITS.MAGE = Outfit()
    :hair(0x2047, 0x0481)
    :beard(0x204B, 0x0481)
    :robe(0x1F03, 0x0003)    -- robe
    :shoes(0x170F)           -- sandals
    :right_hand(0x0DF0)      -- black staff

--- Blacksmith / craftsman.
OUTFITS.SMITH = Outfit()
    :hair(0x203C, 0x044E)
    :shirt(0x1517)
    :pants(0x152E)
    :shoes(0x170B)
    :waist(0x153B)           -- half apron
    :gloves(0x13C6)

--- Mounted knight on a horse (mount graphic 0x3EA0 = horse).
OUTFITS.MOUNTED_KNIGHT = Outfit()
    :hair(0x203C, 0x044E)
    :shirt(0x1517)
    :pants(0x152E)
    :shoes(0x170B)
    :torso(0x1415)           -- plate chest
    :arms(0x1410)            -- plate arms
    :gloves(0x1414)          -- plate gloves
    :legs(0x1411)            -- plate legs
    :helmet(0x1412)          -- close helmet
    :right_hand(0x0F62)      -- war hammer
    :left_hand(0x1B76)       -- metal kite shield
    :cloak(0x1515, 0x0021)   -- red cloak
    :mount(0x3EA0)           -- horse

--- Priest / archdeacon — dark robe, no weapon, austere look.
OUTFITS.PRIEST = Outfit()
    :hair(0x2048, 0x0481)    -- receding hair, grey
    :robe(0x1F03, 0x0455)    -- dark robe
    :shoes(0x170F)           -- sandals
    :necklace(0x1088)        -- necklace (cross stand-in)

--- Gypsy / dancer — colorful skirt and shirt.
OUTFITS.GYPSY = Outfit()
    :hair(0x2049, 0x0044)    -- long hair, black
    :shirt(0x1517, 0x0021)   -- red-dyed shirt
    :skirt(0x1537, 0x0315)   -- colorful skirt (green hue)
    :shoes(0x170F)           -- sandals
    :necklace(0x1088)        -- necklace / jewelry
    :waist(0x153B, 0x0021)   -- red sash

--- Beggar / vagabond — ragged minimal clothing.
OUTFITS.BEGGAR = Outfit()
    :hair(0x203B, 0x044E)    -- short hair, dark
    :shirt(0x1517, 0x0253)   -- dirty shirt
    :pants(0x152E, 0x0253)   -- dirty pants
    -- no shoes (barefoot)

--- Poet / bard — elegant but unarmed.
OUTFITS.POET = Outfit()
    :hair(0x203C, 0x044E)    -- medium hair
    :beard(0x204B, 0x044E)   -- short beard
    :shirt(0x1EFD, 0x0315)   -- fancy shirt, green
    :pants(0x152E, 0x0455)   -- dark pants
    :shoes(0x170B, 0x0455)   -- dark shoes
    :cloak(0x1515, 0x0315)   -- green cloak

--- Judge / magistrate — authoritative dark robe with ornaments.
OUTFITS.JUDGE = Outfit()
    :hair(0x2047, 0x0481)    -- grey hair
    :beard(0x204B, 0x0481)   -- grey beard
    :robe(0x1F03, 0x0001)    -- black robe
    :shoes(0x170B, 0x0001)   -- black shoes
    :necklace(0x1088, 0x0455) -- gold chain
