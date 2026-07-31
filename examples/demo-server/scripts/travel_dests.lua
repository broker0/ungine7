-- Travel stone destinations.
--
-- Each entry: { name, x, y, z, map }.  `map` is the destination world / map
-- facet id; it is optional and defaults to 0 (the starting world).  When the
-- selected destination's `map` differs from the stone's current map, the
-- travel stone performs a cross-world transfer.
DESTINATIONS = {
    { name = "Recall",         x = 1875, y = 1543, z = 0 },
    { name = "Brit Bank",      x = 1417, y = 1698, z = 0 },
    { name = "Moonglow",       x = 4471, y = 1177, z = 0  },
    { name = "Minoc Mines",    x = 2571, y = 592,  z = 0  },
    { name = "Trinsic",        x = 1845, y = 2745, z = 0  },
    { name = "Skara Brae",     x = 596,  y = 2138, z = 0  },
    -- Example cross-world destination (world 1).
    { name = "World 2", x = 1185, y = 1159, z = -25, map = 2 },
}