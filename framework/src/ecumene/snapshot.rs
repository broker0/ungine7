use std::collections::HashMap;
use u_core::BlockKey;

use crate::vessel::tile_shape::TileShape;

/// Shapes of one tile, grouped by entity tag.
///
/// Key — arbitrary u64 tag (usually the entity's serial).
/// This allows removing shapes of a specific entity without affecting
/// shapes of other entities on the same tile.
#[derive(Clone, Default)]
pub struct PathBlock {
    // outer key: entity tag; value: shapes of this entity on this tile
    pub tiles: HashMap<u8, HashMap<u64, Vec<TileShape>>>,
}

impl PathBlock {
    /// Collect dynamic shapes for a tile at local index into `out`.
    ///
    /// `local_idx = oy * 8 + ox` (same layout as [`TileBlock`](super::tile_block::TileBlock)).
    pub fn collect_shapes_at(&self, local_idx: u8, out: &mut Vec<TileShape>) {
        if let Some(by_tag) = self.tiles.get(&local_idx) {
            for shapes in by_tag.values() {
                out.extend(shapes.iter().copied());
            }
        }
    }
}

/// Lightweight collision snapshot of dynamic objects.
///
/// Stores shapes bound to an entity tag, allowing correct
/// removal of individual objects when multiple objects occupy one tile.
#[derive(Clone, Default)]
pub struct CollisionSnapshot {
    pub active_blocks: HashMap<BlockKey, PathBlock>,
}

impl CollisionSnapshot {
    pub fn new() -> Self {
        Self {
            active_blocks: HashMap::new(),
        }
    }

    /// Add a dynamic obstacle shape.
    ///
    /// `tag` — unique entity identifier (e.g., UO serial).
    pub fn add_shape(&mut self, x: u16, y: u16, tag: u64, shape: TileShape) {
        let block_key = BlockKey::from_tile(x, y);
        let local_idx = ((y % 8) * 8 + (x % 8)) as u8;

        self.active_blocks
            .entry(block_key)
            .or_default()
            .tiles
            .entry(local_idx)
            .or_default()
            .entry(tag)
            .or_default()
            .push(shape);
    }

    /// Remove all shapes of a specific entity from the tile.
    ///
    /// Shapes of other entities on the same tile are unaffected.
    pub fn remove_entity_shapes(&mut self, x: u16, y: u16, tag: u64) {
        let block_key = BlockKey::from_tile(x, y);
        let local_idx = ((y % 8) * 8 + (x % 8)) as u8;

        if let Some(block) = self.active_blocks.get_mut(&block_key) {
            if let Some(by_tag) = block.tiles.get_mut(&local_idx) {
                by_tag.remove(&tag);
                if by_tag.is_empty() {
                    block.tiles.remove(&local_idx);
                }
            }
            if block.tiles.is_empty() {
                self.active_blocks.remove(&block_key);
            }
        }
    }

    /// Get all dynamic shapes for a tile (all entities together).
    pub fn get_dynamic_shapes(&self, x: u16, y: u16) -> Option<Vec<TileShape>> {
        let block_key = BlockKey::from_tile(x, y);
        let block = self.active_blocks.get(&block_key)?;
        let local_idx = ((y % 8) * 8 + (x % 8)) as u8;
        let by_tag = block.tiles.get(&local_idx)?;

        // Collect shapes of all entities on this tile into a single list.
        let shapes: Vec<TileShape> = by_tag.values().flatten().cloned().collect();
        if shapes.is_empty() {
            None
        } else {
            Some(shapes)
        }
    }

    /// Get PathBlock for a given block (if dynamic data exists).
    pub fn get_path_block(&self, block: BlockKey) -> Option<&PathBlock> {
        self.active_blocks.get(&block)
    }
}
