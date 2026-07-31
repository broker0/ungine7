
use u_core::{BlockKey, Heading};
use files::map::{self, MapBlock, MapTile};
use files::multi::MultiPart;
use files::statics::StaticTile;
use files::tiledata::{LandTile, StaticTileDef};

/// Trait for accessing optional static world data.
///
/// Implementations provide access to tiledata and map geometry.
/// `Option` return types reflect that data may not be loaded
/// or coordinates may be out of bounds.
pub trait StaticDataProvider: Send + Sync + 'static {
    // ── Tile data ─────────────────────────────────────────────────────────

    /// Land tile definition by graphic id.
    fn land_tile_def(&self, tile_id: u16) -> Option<&LandTile>;

    /// Static tile definition by graphic id.
    fn static_tile_def(&self, tile_id: u16) -> Option<&StaticTileDef>;

    // ── Map geometry ──────────────────────────────────────────────────────

    /// Land tile at absolute tile coordinates.
    fn land_tile_at(&self, world: u8, x: u16, y: u16) -> Option<&MapTile>;

    /// Compute (z_base, z_stand, z_top) for a land tile accounting for slope.
    fn land_tile_z_stand(&self, world: u8, x: u16, y: u16, direction: Heading) -> Option<(i8, i8, i8)>;

    /// Direction-agnostic land tile Z range: `(z_base, z_stand, z_top)`.
    ///
    /// Unlike [`land_tile_z_stand`](Self::land_tile_z_stand), `z_top` is
    /// computed as the average of all four vertices (rounded towards zero),
    /// making the result independent of approach direction.  This is used
    /// by block-level queries where a single direction is not meaningful.
    ///
    /// The default implementation delegates to `land_tile_z_stand` with
    /// [`Heading::North`].  Implementations that have direct access to the
    /// raw vertex data should override for accuracy.
    fn land_tile_z_range(&self, world: u8, x: u16, y: u16) -> Option<(i8, i8, i8)> {
        self.land_tile_z_stand(world, x, y, Heading::North)
    }

    /// Static tiles at absolute tile coordinates.
    fn statics_at(&self, world: u8, x: u16, y: u16) -> Option<&[StaticTile]>;

    // ── Block-level access ────────────────────────────────────────────────

    /// Land-tile block (8×8) by block coordinates.
    ///
    /// Returns `None` if the world is not loaded or block is out of range.
    /// Used by optimised block-level tile providers to avoid 64 individual
    /// lookups per block.
    fn land_block_at(&self, world: u8, block: BlockKey) -> Option<&MapBlock> { let _ = (world, block); None }

    /// All static tiles in a block, sorted by `(x, y, z)`.
    ///
    /// Returns `None` if the world is not loaded or block is out of range.
    fn statics_block_at(&self, world: u8, block: BlockKey) -> Option<&[StaticTile]> { let _ = (world, block); None }

    // ── Multi data ────────────────────────────────────────────────────────

    /// Parts of a multi-object by graphic id.
    fn multi_parts(&self, graphic: u16) -> &[MultiPart];

    // ── Map dimensions ────────────────────────────────────────────────────

    /// Map dimensions in **tiles** for the given world: `(width, height)`.
    ///
    /// Returns the actual dimensions from the loaded map data when available,
    /// falling back to [`files::map::default_tile_dimensions`] otherwise.
    ///
    /// Used to populate `map_width_minus8` / `map_height` in
    /// `0x1B CharacterLocaleAndBody`.
    fn map_tile_dimensions(&self, world: u8) -> Option<(u16, u16)> {
        map::default_tile_dimensions(world)
    }
}
