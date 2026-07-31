//! Physical tile shape for walkability / movement checks.
//!
//! [`TileShape`] describes the vertical geometry of one element in a tile
//! stack.  A tile may be a land surface, a static object, a multi part, or
//! a dynamic item — `TileShape` abstracts the physical properties that the
//! movement continuum needs.
//!
//! Factory methods consume metadata from [`files::tiledata`] and turn it
//! into the appropriate variant.
//!
//! Raw tiledata flags are preserved in `Slope` and `Surface` variants so
//! that [`MovementValidator`](crate::ecumene::movement::MovementValidator) can make
//! context-dependent passability decisions (e.g. water tiles are impassable
//! for land creatures but passable for sea creatures, `HOVER_OVER` tiles are
//! only reachable by flying creatures).

use files::tiledata::{LandTile, StaticTileDef, TileFlags};

// ── TileShape ─────────────────────────────────────────────────────────────

/// Physical shape of a tile element for movement checks.
///
/// `flags` stores the raw [`TileFlags`] from `tiledata.mul` so that
/// passability can be evaluated at query time rather than baked in at
/// construction time.
///
/// Tiles with the [`TileFlags::HOVER_OVER`] flag are represented as a
/// zero-height [`Surface`](TileShape::Surface) — they are only reachable
/// by creatures whose [`MovementValidator`](crate::ecumene::movement::MovementValidator)
/// has `can_fly` set to `true`.
#[derive(Copy, Clone, Debug)]
pub enum TileShape {
    /// A sloped / bridge surface.
    ///
    /// `z_stand` is the midpoint (half-height for bridges), `z_top` is the
    /// highest point.  Used for land tiles with uneven vertices and for
    /// static tiles with the `Bridge` flag.
    Slope {
        z_base: i8,
        z_stand: i8,
        z_top: i8,
        flags: u64,
    },

    /// A flat surface (walkable, impassable, or hover-over flight path).
    ///
    /// `z_stand` equals `z_base + height` — the top of the object.
    /// Tiles with [`TileFlags::HOVER_OVER`] are represented here as
    /// zero-height surfaces (`z_stand == z_base`); they are passable only
    /// for flying creatures.
    Surface {
        z_base: i8,
        z_stand: i8,
        flags: u64,
    },

    /// A background/decorative tile that does not affect movement.
    Background { z_base: i8, z_top: i8 },
}

impl TileShape {
    // ── Factory: static / object tiles ─────────────────────────────────

    /// Build a `TileShape` from a static tile definition and its world Z.
    ///
    /// Raw tiledata flags are stored in the resulting shape; passability
    /// is determined later by [`MovementValidator`](crate::ecumene::MovementValidator) based on creature type.
    ///
    /// Tiles with [`TileFlags::HOVER_OVER`] are emitted as a zero-height
    /// [`Surface`](TileShape::Surface) with all original flags preserved.
    /// The movement validator uses the flag to apply flying-only snap logic.
    pub fn from_static(z: i8, def: &StaticTileDef) -> Self {
        let flags = def.flags.raw();

        let z_base = z;
        let height = def.height as i8;
        let z_top = z_base.saturating_add(height);

        // HoverOver — gargoyle flight paths (height 0 tiles).
        // Represented as a zero-height Surface so flags are preserved and
        // the movement validator can apply flying-specific snap logic.
        if flags & TileFlags::HOVER_OVER != 0 {
            return Self::Surface { z_base, z_stand: z_base, flags };
        }

        // Neither blocking, nor a surface, nor water — purely decorative.
        // `NO_SHOOT` is included so that statics like trees (which lack
        // `IMPASSABLE`/`SURFACE`/`WET` but carry `NO_SHOOT`) become
        // `Surface` with flags preserved, allowing LOS checks to see them.
        if flags & (TileFlags::IMPASSABLE | TileFlags::SURFACE | TileFlags::WET | TileFlags::NO_SHOOT) == 0 {
            return Self::Background { z_base, z_top };
        }

        if flags & TileFlags::BRIDGE != 0 {
            let z_stand = z_base + height / 2;
            Self::Slope {
                z_base,
                z_stand,
                z_top,
                flags,
            }
        } else {
            Self::Surface {
                z_base,
                z_stand: z_top,
                flags,
            }
        }
    }

    // ── Factory: land / map tiles ─────────────────────────────────────

    /// Build a `TileShape` from a land tile with pre-computed vertex Z values.
    ///
    /// - `z_base` — minimum Z of the four tile vertices.
    /// - `z_stand` — standing Z (average of the vertex pair with least delta).
    /// - `z_top` — maximum relevant Z (highest vertex or `z_stand` if flat).
    /// - `tile_id` — land tile graphic id (for void-tile detection).
    /// - `def` — land tile metadata from `tiledata.mul`, if available.
    pub fn from_land(
        z_base: i8,
        z_stand: i8,
        z_top: i8,
        tile_id: u16,
        def: Option<&LandTile>,
    ) -> Self {
        // Void / special land tiles that act as holes.
        if tile_id == 0x0002 || tile_id == 0x01DB || (0x01AE..=0x01B5).contains(&tile_id) {
            return Self::Background { z_base, z_top };
        }

        let flags = def.map(|d| d.flags.raw()).unwrap_or(0);

        if z_base == z_stand && z_stand == z_top {
            Self::Surface {
                z_base,
                z_stand,
                flags,
            }
        } else {
            Self::Slope {
                z_base,
                z_stand,
                z_top,
                flags,
            }
        }
    }

    // ── Convenience constructors ──────────────────────────────────────

    /// Flat surface at `z_stand` with the given flags.
    #[inline]
    pub fn flat(z_base: i8, z_stand: i8, flags: u64) -> Self {
        Self::Surface {
            z_base,
            z_stand,
            flags,
        }
    }

    /// Sentinel "cap" tile — placed at `i8::MAX` as the topmost element
    /// during walkability checks to provide an upper bound.
    #[inline]
    pub fn cap() -> Self {
        Self::Surface {
            z_base: i8::MAX,
            z_stand: i8::MAX,
            flags: TileFlags::IMPASSABLE,
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// Base Z coordinate.
    #[inline]
    pub fn z_base(self) -> i8 {
        match self {
            Self::Slope { z_base, .. }
            | Self::Surface { z_base, .. }
            | Self::Background { z_base, .. } => z_base,
        }
    }

    /// Top Z coordinate (topmost extent of the tile).
    #[inline]
    pub fn z_top(self) -> i8 {
        match self {
            Self::Slope { z_top, .. } => z_top,
            Self::Surface { z_stand, .. } => z_stand,
            Self::Background { z_top, .. } => z_top,
        }
    }

    /// Raw tiledata flags for `Slope` / `Surface`; `0` for other variants.
    #[inline]
    pub fn flags(self) -> u64 {
        match self {
            Self::Slope { flags, .. } | Self::Surface { flags, .. } => flags,
            Self::Background { .. } => 0,
        }
    }

    /// Whether this shape is passable for standard ground movement
    /// (i.e. not `IMPASSABLE` and is a surface/slope).
    ///
    /// Note: tiles with [`TileFlags::HOVER_OVER`] are not `IMPASSABLE` in
    /// tiledata, so they return `true` here.  The movement validator gates
    /// them behind `can_fly` separately.
    #[inline]
    pub fn is_passable(self) -> bool {
        match self {
            Self::Slope { flags, .. } | Self::Surface { flags, .. } =>
                flags & TileFlags::IMPASSABLE == 0,
            Self::Background { .. } => false,
        }
    }

    /// Whether this shape is passable given an extra passable-override mask.
    ///
    /// If the tile's flags match `mask`, the tile is considered passable even
    /// if `IMPASSABLE` is set.  This allows e.g. sea creatures to walk on
    /// `WET` tiles by passing `TileFlags::WET` as the mask.
    ///
    /// Note: tiles with [`TileFlags::HOVER_OVER`] are gated by `can_fly` in
    /// the movement validator, not by this mask.
    #[inline]
    pub fn is_passable_with(self, passable_mask: u64) -> bool {
        match self {
            Self::Slope { flags, .. } | Self::Surface { flags, .. } => {
                if passable_mask != 0 && flags & passable_mask != 0 {
                    return true;
                }
                flags & TileFlags::IMPASSABLE == 0
            }
            Self::Background { .. } => false,
        }
    }

    // ── Line-of-sight helpers ─────────────────────────────────────────

    /// Default blocking mask: any of these flags cause a tile to block LOS.
    ///
    /// `NO_SHOOT | IMPASSABLE | WALL`
    pub const LOS_BLOCKING_DEFAULT: u64 =
        TileFlags::NO_SHOOT | TileFlags::IMPASSABLE | TileFlags::WALL;

    /// Default exempt mask: tiles with any of these flags are NOT blocked
    /// even if they match the blocking mask.
    ///
    /// `WINDOW` — walls with windows are transparent.
    pub const LOS_EXEMPT_DEFAULT: u64 = TileFlags::WINDOW;

    /// Fully configurable LOS blocking check.
    ///
    /// A shape blocks LOS when **all** of the following are true:
    ///
    /// 1. It is a `Slope` or `Surface` (not `Background`).
    /// 2. Its flags do **not** match `transparent_mask` (caller-level override,
    ///    e.g. `FOLIAGE` to see through trees).
    /// 3. Its flags do **not** match `exempt_mask` (e.g. `WINDOW`).
    /// 4. At least one flag in `blocking_mask` is set (e.g.
    ///    `NO_SHOOT | IMPASSABLE | WALL`).
    #[inline]
    pub fn blocks_los_masked(
        self,
        blocking_mask: u64,
        exempt_mask: u64,
        transparent_mask: u64,
    ) -> bool {
        let flags = match self {
            Self::Slope { flags, .. } | Self::Surface { flags, .. } => flags,
            Self::Background { .. } => return false,
        };

        // Transparent override — caller can punch through foliage, etc.
        if transparent_mask != 0 && flags & transparent_mask != 0 {
            return false;
        }

        // Exempt — e.g. windows are never solid.
        if exempt_mask != 0 && flags & exempt_mask != 0 {
            return false;
        }

        // Any blocking flag present → blocks.
        flags & blocking_mask != 0
    }

    /// Whether this shape blocks LOS with default rules.
    ///
    /// Equivalent to `blocks_los_masked(LOS_BLOCKING_DEFAULT, LOS_EXEMPT_DEFAULT, 0)`.
    #[inline]
    pub fn blocks_los(self) -> bool {
        self.blocks_los_masked(Self::LOS_BLOCKING_DEFAULT, Self::LOS_EXEMPT_DEFAULT, 0)
    }

    /// Like [`blocks_los`](Self::blocks_los) but with an extra transparency
    /// mask.
    ///
    /// If the tile's flags match `transparent_mask`, the tile is considered
    /// transparent even if it would otherwise block LOS.  For example, pass
    /// [`TileFlags::FOLIAGE`] to see through trees.
    #[inline]
    pub fn blocks_los_with(self, transparent_mask: u64) -> bool {
        self.blocks_los_masked(
            Self::LOS_BLOCKING_DEFAULT,
            Self::LOS_EXEMPT_DEFAULT,
            transparent_mask,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use files::tiledata::TileFlags;
    use u_io::FixedString;

    /// Helper: build a `StaticTileDef` with given flags and height.
    fn make_def(flags: u64, height: u8) -> StaticTileDef {
        StaticTileDef {
            flags: TileFlags(flags),
            weight: 0,
            quality: 0,
            anim_id: 0,
            height,
            hue: 0,
            name: FixedString(String::new()),
        }
    }

    #[test]
    fn tree_with_no_shoot_becomes_surface() {
        // Tree statics typically have NO_SHOOT | FOLIAGE but lack
        // IMPASSABLE / SURFACE / WET.  They must NOT become Background
        // (which discards flags and is invisible to LOS).
        let def = make_def(TileFlags::NO_SHOOT | TileFlags::FOLIAGE, 15);
        let shape = TileShape::from_static(0, &def);

        match shape {
            TileShape::Surface { z_base, z_stand, flags } => {
                assert_eq!(z_base, 0);
                assert_eq!(z_stand, 15); // z_base + height
                assert!(flags & TileFlags::NO_SHOOT != 0, "NO_SHOOT flag must be preserved");
                assert!(flags & TileFlags::FOLIAGE != 0, "FOLIAGE flag must be preserved");
            }
            other => panic!("Expected Surface, got {:?}", other),
        }

        // Must block LOS (NO_SHOOT present).
        assert!(shape.blocks_los());
        // But transparent when FOLIAGE mask is set.
        assert!(!shape.blocks_los_with(TileFlags::FOLIAGE));
    }

    #[test]
    fn purely_decorative_becomes_background() {
        // Static with no relevant flags at all → Background.
        let def = make_def(TileFlags::BACKGROUND, 10);
        let shape = TileShape::from_static(5, &def);

        match shape {
            TileShape::Background { z_base, z_top } => {
                assert_eq!(z_base, 5);
                assert_eq!(z_top, 15);
            }
            other => panic!("Expected Background, got {:?}", other),
        }

        // Must NOT block LOS.
        assert!(!shape.blocks_los());
    }

    #[test]
    fn impassable_wall_blocks_los() {
        let def = make_def(
            TileFlags::WALL | TileFlags::IMPASSABLE,
            20,
        );
        let shape = TileShape::from_static(0, &def);

        assert!(matches!(shape, TileShape::Surface { .. }));
        assert!(shape.blocks_los());
    }

    #[test]
    fn wall_with_window_does_not_block_los() {
        let def = make_def(
            TileFlags::WALL | TileFlags::IMPASSABLE | TileFlags::WINDOW,
            20,
        );
        let shape = TileShape::from_static(0, &def);

        assert!(matches!(shape, TileShape::Surface { .. }));
        assert!(!shape.blocks_los());
    }

    #[test]
    fn impassable_tree_blocks_los() {
        // Tree with Background + Impassable (no NO_SHOOT, no WALL).
        // Real UO tiledata example: many tree trunks have exactly these flags.
        let def = make_def(
            TileFlags::BACKGROUND | TileFlags::IMPASSABLE | TileFlags::PREFIX_A,
            15,
        );
        let shape = TileShape::from_static(0, &def);

        // Must become Surface (IMPASSABLE in the gate).
        assert!(matches!(shape, TileShape::Surface { .. }));
        // Must block LOS — IMPASSABLE alone is enough.
        assert!(shape.blocks_los());
    }

    #[test]
    fn impassable_alone_blocks_los() {
        // Just IMPASSABLE, nothing else — still blocks LOS.
        let shape = TileShape::Surface {
            z_base: 0,
            z_stand: 10,
            flags: TileFlags::IMPASSABLE,
        };
        assert!(shape.blocks_los());
    }
}
