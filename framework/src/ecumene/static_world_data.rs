//! Level 0: Global read-only world data loaded from UO client files.
//!
//! [`StaticWorldData`] is loaded once at startup (when `--data-dir` is given)
//! and shared via `Arc` across all shards and sessions.  It is completely
//! immutable after construction — no locks required.
//!
//! Contains:
//! - [`TileData`] — tile metadata (`tiledata.mul`)
//! - [`MultiCollection`] — standard multi definitions (`multi.idx` + `multi.mul`)
//! - [`WorldFiles`] — per-map terrain + statics (up to 6 maps)

use std::io;
use std::path::Path;

use log::{debug, warn};
use u_core::{BlockKey, Heading};

use files::map::{self, MapBlock, MapData, MapTile};
use files::multi::{MultiCollection, MultiPart};
use files::statics::{StaticData, StaticTile};
use files::tiledata::{LandTile, StaticTileDef, TileData};

use super::land_z::{compute_land_z_range, compute_land_z_stand};
use crate::vessel::traits::StaticDataProvider;

// ── WorldFiles ────────────────────────────────────────────────────────────

/// Map + statics for a single UO world.
#[derive(Debug)]
pub struct WorldFiles {
    pub map: MapData,
    pub statics: StaticData,
}

// ── StaticWorldData ───────────────────────────────────────────────────────

/// Number of worlds supported (Felucca, Trammel, Ilshenar, Malas, Tokuno, Ter Mur).
const MAX_WORLDS: usize = 6;

/// Global immutable world data, shared via `Arc<StaticWorldData>`.
///
/// Loaded once from the UO client data directory.  All fields are read-only
/// after construction.
#[derive(Debug)]
pub struct StaticWorldData {
    pub tiledata: TileData,
    pub multis: MultiCollection,
    worlds: [Option<WorldFiles>; MAX_WORLDS],
}

// ── StaticDataProvider impl ───────────────────────────────────────────────

impl StaticDataProvider for StaticWorldData {
    fn land_tile_def(&self, tile_id: u16) -> Option<&LandTile> {
        self.tiledata.land(tile_id)
    }

    fn static_tile_def(&self, tile_id: u16) -> Option<&StaticTileDef> {
        self.tiledata.static_tile(tile_id)
    }

    fn land_tile_at(&self, world: u8, x: u16, y: u16) -> Option<&MapTile> {
        let wf = self.world(world)?;
        let (ux, uy) = Self::wrap(x, y, &wf.map);
        Some(wf.map.tile_at(ux, uy))
    }

    fn land_tile_z_stand(&self, world: u8, x: u16, y: u16, direction: Heading) -> Option<(i8, i8, i8)> {
        let wf = self.world(world)?;

        let left   = self.land_vertex_z_raw(wf, x,     y    );
        let bottom = self.land_vertex_z_raw(wf, x + 1, y    );
        let right  = self.land_vertex_z_raw(wf, x + 1, y + 1);
        let top    = self.land_vertex_z_raw(wf, x,     y + 1);

        Some(compute_land_z_stand(left, bottom, right, top, direction))
    }

    fn land_tile_z_range(&self, world: u8, x: u16, y: u16) -> Option<(i8, i8, i8)> {
        let wf = self.world(world)?;

        let left   = self.land_vertex_z_raw(wf, x,     y    );
        let bottom = self.land_vertex_z_raw(wf, x + 1, y    );
        let right  = self.land_vertex_z_raw(wf, x + 1, y + 1);
        let top    = self.land_vertex_z_raw(wf, x,     y + 1);

        Some(compute_land_z_range(left, bottom, right, top))
    }

    fn statics_at(&self, world: u8, x: u16, y: u16) -> Option<&[StaticTile]> {
        let wf = self.world(world)?;
        let (ux, uy) = Self::wrap(x, y, &wf.map);
        let bx = ux / 8;
        let by = uy / 8;
        let ox = (ux % 8) as u8;
        let oy = (uy % 8) as u8;
        let block_idx = bx * wf.statics.y_blocks() + by;
        Some(wf.statics.block_tile(block_idx, ox, oy))
    }

    fn land_block_at(&self, world: u8, block: BlockKey) -> Option<&MapBlock> {
        let wf = self.world(world)?;
        let bx = block.bx as usize;
        let by = block.by as usize;
        if bx < wf.map.x_blocks() && by < wf.map.y_blocks() {
            Some(wf.map.block_at(bx, by))
        } else {
            None
        }
    }

    fn statics_block_at(&self, world: u8, block: BlockKey) -> Option<&[StaticTile]> {
        let wf = self.world(world)?;
        let bx = block.bx as usize;
        let by = block.by as usize;
        if bx < wf.map.x_blocks() && by < wf.map.y_blocks() {
            Some(wf.statics.block_at(bx, by))
        } else {
            None
        }
    }

    fn multi_parts(&self, graphic: u16) -> &[MultiPart] {
        self.multis.parts(graphic)
    }

    fn map_tile_dimensions(&self, world: u8) -> Option<(u16, u16)> {
        // Use actual loaded map dimensions when available.
        if let Some(wf) = self.world(world) {
            return Some((wf.map.width() as u16, wf.map.height() as u16));
        }
        // Fallback to default constants.
        map::default_tile_dimensions(world)
    }
}

// ── StaticWorldData impl ──────────────────────────────────────────────────

impl StaticWorldData {
    /// Load all available world data from the client data directory.
    ///
    /// Missing worlds are skipped (their slot is `None`).
    /// Returns an error only if `tiledata.mul` or `multi.idx`/`multi.mul`
    /// cannot be loaded — those are required.
    pub fn load(data_dir: &Path) -> io::Result<Self> {
        debug!("loading static world data from {}", data_dir.display());

        let tiledata = TileData::read(data_dir)?;
        debug!(
            "tiledata: {} land tiles, {} static tiles",
            tiledata.land_tiles().len(),
            tiledata.static_tiles().len(),
        );

        let multis = MultiCollection::read(data_dir)?;
        debug!(
            "multis: {} entries ({} with data, {} total parts)",
            multis.len(),
            multis.valid_count(),
            multis.total_parts(),
        );

        let worlds = Self::load_worlds(data_dir);

        Ok(Self { tiledata, multis, worlds })
    }

    fn load_worlds(data_dir: &Path) -> [Option<WorldFiles>; MAX_WORLDS] {
        let mut worlds: Vec<Option<WorldFiles>> = Vec::with_capacity(MAX_WORLDS);

        for world_idx in 0..MAX_WORLDS {
            let idx = world_idx as u8;
            match Self::load_world(data_dir, idx) {
                Ok(wf) => {
                    debug!(
                        "world {idx}: map {}x{} blocks, {} static tiles",
                        wf.map.x_blocks(),
                        wf.map.y_blocks(),
                        wf.statics.total_tiles(),
                    );
                    worlds.push(Some(wf));
                }
                Err(e) => {
                    warn!("world {idx}: not loaded ({e})");
                    worlds.push(None);
                }
            }
        }

        worlds
            .try_into()
            .unwrap_or_else(|_| panic!("worlds vec should have exactly {MAX_WORLDS} elements"))
    }

    fn load_world(data_dir: &Path, world: u8) -> io::Result<WorldFiles> {
        let map_data = map::read(data_dir, world)?;
        let x_blocks = map_data.x_blocks();
        let y_blocks = map_data.y_blocks();
        let statics = StaticData::read_with_size(data_dir, world, x_blocks, y_blocks)?;
        Ok(WorldFiles { map: map_data, statics })
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// Get the world files for a specific map index, if loaded.
    pub fn world(&self, idx: u8) -> Option<&WorldFiles> {
        self.worlds.get(idx as usize).and_then(Option::as_ref)
    }

    /// Get the Z coordinate for a land vertex.
    pub fn land_vertex_z(&self, world: u8, x: u16, y: u16) -> Option<i8> {
        self.land_tile_at(world, x, y).map(|t| t.z)
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Vertex Z for a coordinate, wrapping around map boundaries.
    fn land_vertex_z_raw(&self, wf: &WorldFiles, x: u16, y: u16) -> i8 {
        let (ux, uy) = Self::wrap(x, y, &wf.map);
        wf.map.tile_at(ux, uy).z
    }

    /// Wrap tile coordinates to map dimensions.
    fn wrap(x: u16, y: u16, map: &MapData) -> (usize, usize) {
        let w = map.width();
        let h = map.height();
        let ux = (x as usize) % w;
        let uy = (y as usize) % h;
        (ux, uy)
    }
}
