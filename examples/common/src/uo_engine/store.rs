//! [`DemoStore`] — simple `HashMap`-based entity store.
//!
//! Implements [`EntityStore`] for [`DemoEntity`].

use super::entity::DemoEntity;
use framework::continuum::EntityStore;
use std::collections::HashMap;

pub struct DemoStore {
    entities: HashMap<u32, DemoEntity>,
}

impl EntityStore<DemoEntity> for DemoStore {
    fn new() -> Self {
        Self {
            entities: HashMap::new(),
        }
    }

    fn insert(&mut self, id: u32, data: DemoEntity) {
        self.entities.insert(id, data);
    }

    fn remove(&mut self, id: u32) -> Option<DemoEntity> {
        self.entities.remove(&id)
    }

    fn get(&self, id: u32) -> Option<&DemoEntity> {
        self.entities.get(&id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut DemoEntity> {
        self.entities.get_mut(&id)
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (&u32, &DemoEntity)> + '_> {
        Box::new(self.entities.iter())
    }

    fn clear(&mut self) {
        self.entities.clear();
    }
}
