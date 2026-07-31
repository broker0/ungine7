//! Item property storage trait for [`Zone`](super::zone::Zone).
//!
//! Provides a framework-level abstraction for storing per-item metadata
//! (name, tooltip, custom key-value pairs) keyed by item serial.
//!
//! The design mirrors [`ZoneContainers`](super::container::ZoneContainers):
//! - [`NoItemProps`] — no-op stub (default, zero cost).
//! - Consumers provide a concrete implementation (e.g. `HashItemProps`)
//!   that stores actual property data.
//!
//! The framework does not define the property struct itself — that is
//! game-level logic.  The trait is generic over `V: Send + Clone` so
//! each consumer chooses their own value type.

use std::collections::HashMap;

// ── Trait ─────────────────────────────────────────────────────────────────

/// Trait for item property storage inside a [`Zone`](super::zone::Zone).
///
/// The default type parameter on `Zone` is [`NoItemProps`] — a no-op
/// stub that stores nothing.  Use a concrete implementation (e.g.
/// `HashItemProps<V>`) when you need per-item metadata.
///
/// `V` is the value type stored per item — chosen by the consumer.
pub trait ZoneItemProps: Send + Default {
    /// The per-item value type.
    type Value: Send + Clone;

    /// Look up properties by item serial.
    fn get(&self, serial: u32) -> Option<&Self::Value>;

    /// Look up properties by item serial (mutable).
    fn get_mut(&mut self, serial: u32) -> Option<&mut Self::Value>;

    /// Insert or replace properties for an item.
    fn insert(&mut self, serial: u32, value: Self::Value);

    /// Remove properties for an item.  Returns the removed value, if any.
    fn remove(&mut self, serial: u32) -> Option<Self::Value>;

    /// Remove all stored properties.
    fn clear(&mut self);

    /// Number of items with properties.
    fn len(&self) -> usize;

    /// Whether the store is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over all stored `(serial, value)` pairs.
    fn iter(&self) -> Box<dyn Iterator<Item = (&u32, &Self::Value)> + '_>;

    /// Clone all entries into a `HashMap` (for snapshot / serialisation).
    fn to_map(&self) -> HashMap<u32, Self::Value> {
        self.iter().map(|(&k, v)| (k, v.clone())).collect()
    }
}

// ── NoItemProps ───────────────────────────────────────────────────────────

/// No-op item property storage — item properties are not supported.
///
/// This is the default for `Zone` when the `P` type parameter is not
/// specified.  All operations are no-ops with zero runtime cost.
#[derive(Debug, Clone, Default)]
pub struct NoItemProps;

impl ZoneItemProps for NoItemProps {
    type Value = ();

    fn get(&self, _: u32) -> Option<&()> { None }
    fn get_mut(&mut self, _: u32) -> Option<&mut ()> { None }
    fn insert(&mut self, _: u32, _: ()) {}
    fn remove(&mut self, _: u32) -> Option<()> { None }
    fn clear(&mut self) {}
    fn len(&self) -> usize { 0 }
    fn iter(&self) -> Box<dyn Iterator<Item = (&u32, &())> + '_> {
        Box::new(std::iter::empty())
    }
}

// ── HashItemProps ─────────────────────────────────────────────────────────

/// Full item property storage backed by a [`HashMap`].
///
/// `V` is the per-item value type (e.g. `ItemProps` from the game layer).
#[derive(Debug, Clone)]
pub struct HashItemProps<V: Send + Clone>(pub HashMap<u32, V>);

impl<V: Send + Clone> Default for HashItemProps<V> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<V: Send + Clone + 'static> ZoneItemProps for HashItemProps<V> {
    type Value = V;

    fn get(&self, serial: u32) -> Option<&V> {
        self.0.get(&serial)
    }

    fn get_mut(&mut self, serial: u32) -> Option<&mut V> {
        self.0.get_mut(&serial)
    }

    fn insert(&mut self, serial: u32, value: V) {
        self.0.insert(serial, value);
    }

    fn remove(&mut self, serial: u32) -> Option<V> {
        self.0.remove(&serial)
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (&u32, &V)> + '_> {
        Box::new(self.0.iter())
    }
}
