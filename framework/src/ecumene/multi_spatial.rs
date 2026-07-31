//! Spatial index trait and implementations for multi-object lookup.

use std::collections::{HashMap, HashSet};

use u_core::BlockKey;

use super::tile_rect::TileRect;

pub trait SpatialIndex: Clone {
    fn insert(&mut self, serial: u32, bbox: TileRect);
    fn remove(&mut self, serial: u32);
    fn query_point(&self, x: u16, y: u16) -> Vec<u32>;
    fn query_rect(&self, rect: &TileRect) -> Vec<u32>;
    fn clear(&mut self);
}

#[derive(Clone, Default)]
pub struct BlockSpatialIndex {
    blocks: HashMap<BlockKey, Vec<u32>>,
    reverse: HashMap<u32, Vec<BlockKey>>,
}

impl BlockSpatialIndex {
    pub fn new() -> Self { Self::default() }
}

impl SpatialIndex for BlockSpatialIndex {
    fn insert(&mut self, serial: u32, bbox: TileRect) {
        let bx_min = bbox.x_min / BlockKey::BLOCK_SIZE;
        let bx_max = bbox.x_max / BlockKey::BLOCK_SIZE;
        let by_min = bbox.y_min / BlockKey::BLOCK_SIZE;
        let by_max = bbox.y_max / BlockKey::BLOCK_SIZE;

        let mut keys = Vec::with_capacity(
            ((bx_max - bx_min + 1) * (by_max - by_min + 1)) as usize,
        );
        for bx in bx_min..=bx_max {
            for by in by_min..=by_max {
                let key = BlockKey::new(bx, by);
                self.blocks.entry(key).or_default().push(serial);
                keys.push(key);
            }
        }
        self.reverse.insert(serial, keys);
    }

    fn remove(&mut self, serial: u32) {
        if let Some(keys) = self.reverse.remove(&serial) {
            for key in keys {
                if let Some(serials) = self.blocks.get_mut(&key) {
                    serials.retain(|&s| s != serial);
                    if serials.is_empty() { self.blocks.remove(&key); }
                }
            }
        }
    }

    fn query_point(&self, x: u16, y: u16) -> Vec<u32> {
        let key = BlockKey::from_tile(x, y);
        match self.blocks.get(&key) {
            Some(serials) => serials.clone(),
            None => Vec::new(),
        }
    }

    fn query_rect(&self, rect: &TileRect) -> Vec<u32> {
        let bx_min = rect.x_min / BlockKey::BLOCK_SIZE;
        let bx_max = rect.x_max / BlockKey::BLOCK_SIZE;
        let by_min = rect.y_min / BlockKey::BLOCK_SIZE;
        let by_max = rect.y_max / BlockKey::BLOCK_SIZE;

        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for bx in bx_min..=bx_max {
            for by in by_min..=by_max {
                let key = BlockKey::new(bx, by);
                if let Some(serials) = self.blocks.get(&key) {
                    for &s in serials {
                        if seen.insert(s) {
                            result.push(s);
                        }
                    }
                }
            }
        }
        result
    }

    fn clear(&mut self) {
        self.blocks.clear();
        self.reverse.clear();
    }
}

#[derive(Clone, Default)]
pub struct BBoxSpatialIndex {
    entries: Vec<(u32, TileRect)>,
}

impl BBoxSpatialIndex {
    pub fn new() -> Self { Self::default() }
}

impl SpatialIndex for BBoxSpatialIndex {
    fn insert(&mut self, serial: u32, bbox: TileRect) {
        self.entries.push((serial, bbox));
    }

    fn remove(&mut self, serial: u32) {
        self.entries.retain(|&(s, _)| s != serial);
    }

    fn query_point(&self, x: u16, y: u16) -> Vec<u32> {
        self.entries
            .iter()
            .filter(|(_, bbox)| {
                x >= bbox.x_min && x <= bbox.x_max
                    && y >= bbox.y_min && y <= bbox.y_max
            })
            .map(|&(serial, _)| serial)
            .collect()
    }

    fn query_rect(&self, rect: &TileRect) -> Vec<u32> {
        self.entries
            .iter()
            .filter(|(_, bbox)| bbox.overlaps(rect))
            .map(|&(serial, _)| serial)
            .collect()
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}
