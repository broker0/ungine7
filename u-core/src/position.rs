use core::fmt;

/// Compass direction (8 values, matching the UO wire format).
///
/// The numeric value matches the on-wire encoding used by movement packets.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Heading {
    North     = 0,
    NorthEast = 1,
    East      = 2,
    SouthEast = 3,
    South     = 4,
    SouthWest = 5,
    West      = 6,
    NorthWest = 7,
}

impl Heading {
    /// All eight headings in order.
    pub const ALL: [Heading; 8] = [
        Heading::North,
        Heading::NorthEast,
        Heading::East,
        Heading::SouthEast,
        Heading::South,
        Heading::SouthWest,
        Heading::West,
        Heading::NorthWest,
    ];

    /// Parse from a raw byte.  Returns `None` for values ≥ 8.
    #[inline]
    pub fn from_raw(value: u8) -> Option<Self> {
        Self::ALL.get(value as usize).copied()
    }

    /// Compute the heading from a tile delta.
    ///
    /// Only the signs of `dx` and `dy` matter (8-directional).
    /// Returns `None` when `dx == 0 && dy == 0`.
    #[inline]
    pub fn from_delta(dx: i32, dy: i32) -> Option<Self> {
        match (dx.signum(), dy.signum()) {
            ( 0, -1) => Some(Self::North),
            ( 1, -1) => Some(Self::NorthEast),
            ( 1,  0) => Some(Self::East),
            ( 1,  1) => Some(Self::SouthEast),
            ( 0,  1) => Some(Self::South),
            (-1,  1) => Some(Self::SouthWest),
            (-1,  0) => Some(Self::West),
            (-1, -1) => Some(Self::NorthWest),
            _        => None,
        }
    }

    /// One-tile step delta `(dx, dy)` for this heading.
    ///
    /// North is `(0, -1)`, East is `(1, 0)`, etc.
    #[inline]
    pub fn delta(self) -> (i32, i32) {
        match self {
            Self::North     => ( 0, -1),
            Self::NorthEast => ( 1, -1),
            Self::East      => ( 1,  0),
            Self::SouthEast => ( 1,  1),
            Self::South     => ( 0,  1),
            Self::SouthWest => (-1,  1),
            Self::West      => (-1,  0),
            Self::NorthWest => (-1, -1),
        }
    }

    /// Turn by `steps` (positive = clockwise, negative = counter-clockwise).
    #[inline]
    pub fn turn(self, steps: i8) -> Self {
        let idx = (self as i8 + steps).rem_euclid(8) as u8;
        // SAFETY: rem_euclid(8) always produces 0..7.
        Self::ALL[idx as usize]
    }

    /// Whether this is a diagonal direction (NE, SE, SW, NW).
    #[inline]
    pub fn is_diagonal(self) -> bool {
        (self as u8) & 1 != 0
    }

    /// The two adjacent straight directions for a diagonal heading.
    ///
    /// For straight headings, returns `None`.
    ///
    /// Example: `NorthEast.adjacent_straight()` → `Some((North, East))`.
    #[inline]
    pub fn adjacent_straight(self) -> Option<(Self, Self)> {
        if !self.is_diagonal() {
            return None;
        }
        Some((self.turn(-1), self.turn(1)))
    }
}

impl TryFrom<u8> for Heading {
    type Error = u8;

    /// Converts a raw byte to a `Heading`.
    ///
    /// Returns `Err(value)` for values ≥ 8.
    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_raw(value).ok_or(value)
    }
}

impl From<Heading> for u8 {
    #[inline]
    fn from(h: Heading) -> u8 {
        h as u8
    }
}

impl fmt::Display for Heading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::North     => "N",
            Self::NorthEast => "NE",
            Self::East      => "E",
            Self::SouthEast => "SE",
            Self::South     => "S",
            Self::SouthWest => "SW",
            Self::West      => "W",
            Self::NorthWest => "NW",
        };
        f.write_str(s)
    }
}


/// Direction byte: [`Heading`] + running flag.
///
/// Wraps the raw wire byte with the invariant that only bits 0–2 (heading)
/// and bit 7 (running) are set.  Bits 3–6 are always zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Facing(u8);

impl Facing {
    /// Mask for the meaningful bits: heading (0–2) and running (7).
    const MASK: u8 = 0x87;

    /// Create from a raw direction byte, masking to valid bits.
    #[inline]
    pub fn new(raw: u8) -> Self {
        Self(raw & Self::MASK)
    }

    /// Create from a [`Heading`] without the running flag.
    #[inline]
    pub fn from_heading(heading: Heading) -> Self {
        Self(heading as u8)
    }

    /// The compass heading.
    #[inline]
    pub fn heading(self) -> Heading {
        // SAFETY: bits 0–2 are always in 0..7 due to the MASK invariant.
        Heading::ALL[(self.0 & 0x07) as usize]
    }

    /// Whether the running flag is set.
    #[inline]
    pub fn is_running(self) -> bool {
        self.0 & 0x80 != 0
    }

    /// Return a copy with the running flag set or cleared.
    #[inline]
    pub fn with_running(self, running: bool) -> Self {
        if running {
            Self(self.0 | 0x80)
        } else {
            Self(self.0 & 0x07)
        }
    }

    /// The raw wire byte.
    #[inline]
    pub fn raw(self) -> u8 {
        self.0
    }
}

impl From<Heading> for Facing {
    #[inline]
    fn from(heading: Heading) -> Self {
        Self::from_heading(heading)
    }
}

impl From<Facing> for u8 {
    #[inline]
    fn from(f: Facing) -> u8 {
        f.raw()
    }
}

impl fmt::Display for Facing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.heading())?;
        if self.is_running() {
            f.write_str(" (running)")?;
        }
        Ok(())
    }
}


/// 2D tile coordinate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Pos2D {
    pub x: u16,
    pub y: u16,
}

impl Pos2D {
    /// Create a new 2D position.
    #[inline]
    pub fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }

    /// Compute the [`BlockKey`] for this tile.
    #[inline]
    pub fn block_key(self) -> BlockKey {
        BlockKey::from_tile(self.x, self.y)
    }
}

impl fmt::Display for Pos2D {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}


/// Block index in the 8×8 tile grid.
///
/// UO maps are divided into blocks of [`BLOCK_SIZE`](Self::BLOCK_SIZE) × [`BLOCK_SIZE`](Self::BLOCK_SIZE)
/// tiles.  `BlockKey` identifies one such block.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct BlockKey {
    pub bx: u16,
    pub by: u16,
}

impl BlockKey {
    /// Block size in tiles (UO map blocks are always 8×8).
    pub const BLOCK_SIZE: u16 = 8;

    /// Create a block key directly.
    #[inline]
    pub fn new(bx: u16, by: u16) -> Self {
        Self { bx, by }
    }

    /// Compute the block key for a tile coordinate.
    #[inline]
    pub fn from_tile(x: u16, y: u16) -> Self {
        Self {
            bx: x / Self::BLOCK_SIZE,
            by: y / Self::BLOCK_SIZE,
        }
    }

    /// Top-left tile coordinate of this block.
    #[inline]
    pub fn origin(self) -> Pos2D {
        Pos2D {
            x: self.bx * Self::BLOCK_SIZE,
            y: self.by * Self::BLOCK_SIZE,
        }
    }
}

impl fmt::Display for BlockKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block({}, {})", self.bx, self.by)
    }
}

/// 3D tile coordinate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Pos3D {
    pub x: u16,
    pub y: u16,
    pub z: i8,
}

impl Pos3D {
    /// Create a new 3D position.
    #[inline]
    pub fn new(x: u16, y: u16, z: i8) -> Self {
        Self { x, y, z }
    }

    /// Project to 2D (drop Z).
    #[inline]
    pub fn pos2d(self) -> Pos2D {
        Pos2D { x: self.x, y: self.y }
    }

    /// Compute the [`BlockKey`] for this tile.
    #[inline]
    pub fn block_key(self) -> BlockKey {
        BlockKey::from_tile(self.x, self.y)
    }

    /// Return a copy with a different Z.
    #[inline]
    pub fn with_z(self, z: i8) -> Self {
        Self { z, ..self }
    }
}

impl From<Pos3D> for Pos2D {
    #[inline]
    fn from(p: Pos3D) -> Self {
        p.pos2d()
    }
}

impl From<Pos2D> for Pos3D {
    /// Widen to 3D with `z = 0`.
    #[inline]
    fn from(p: Pos2D) -> Self {
        Self { x: p.x, y: p.y, z: 0 }
    }
}

impl fmt::Display for Pos3D {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x, self.y, self.z)
    }
}

/// 3D position on a specific map (world).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TilePos {
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub world: u8,
}

impl TilePos {
    /// Create a new tile position.
    #[inline]
    pub fn new(x: u16, y: u16, z: i8, world: u8) -> Self {
        Self { x, y, z, world }
    }

    /// Project to 2D (drop Z, world).
    #[inline]
    pub fn pos2d(self) -> Pos2D {
        Pos2D { x: self.x, y: self.y }
    }

    /// Project to 3D (drop world).
    #[inline]
    pub fn pos3d(self) -> Pos3D {
        Pos3D { x: self.x, y: self.y, z: self.z }
    }

    /// Compute the [`BlockKey`] for this tile.
    #[inline]
    pub fn block_key(self) -> BlockKey {
        BlockKey::from_tile(self.x, self.y)
    }
}

impl From<TilePos> for Pos3D {
    #[inline]
    fn from(p: TilePos) -> Self {
        p.pos3d()
    }
}

impl From<TilePos> for Pos2D {
    #[inline]
    fn from(p: TilePos) -> Self {
        p.pos2d()
    }
}

impl From<Pos3D> for TilePos {
    /// Widen to `TilePos` with `world = 0`.
    #[inline]
    fn from(p: Pos3D) -> Self {
        Self { x: p.x, y: p.y, z: p.z, world: 0 }
    }
}

impl fmt::Display for TilePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {}) world={}", self.x, self.y, self.z, self.world)
    }
}

/// 3D position with facing direction — for mobiles (players, NPCs).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct MobilePos {
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub facing: Facing,
}

impl MobilePos {
    /// Create a new mobile position.
    #[inline]
    pub fn new(x: u16, y: u16, z: i8, facing: Facing) -> Self {
        Self { x, y, z, facing }
    }

    /// Project to 2D (drop Z, facing).
    #[inline]
    pub fn pos2d(self) -> Pos2D {
        Pos2D { x: self.x, y: self.y }
    }

    /// Project to 3D (drop facing).
    #[inline]
    pub fn pos3d(self) -> Pos3D {
        Pos3D { x: self.x, y: self.y, z: self.z }
    }

    /// Compute the [`BlockKey`] for this tile.
    #[inline]
    pub fn block_key(self) -> BlockKey {
        BlockKey::from_tile(self.x, self.y)
    }

    /// Compass heading (convenience for `self.facing.heading()`).
    #[inline]
    pub fn heading(self) -> Heading {
        self.facing.heading()
    }

    /// Whether the mobile is running (convenience for `self.facing.is_running()`).
    #[inline]
    pub fn is_running(self) -> bool {
        self.facing.is_running()
    }
}

impl From<MobilePos> for Pos3D {
    #[inline]
    fn from(p: MobilePos) -> Self {
        p.pos3d()
    }
}

impl From<MobilePos> for Pos2D {
    #[inline]
    fn from(p: MobilePos) -> Self {
        p.pos2d()
    }
}

impl fmt::Display for MobilePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {}) {}", self.x, self.y, self.z, self.facing)
    }
}
