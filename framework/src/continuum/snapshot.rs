//! Zone snapshot trait for save/restore.
//!
//! Defines a framework-level abstraction for capturing and restoring the
//! mutable state of a [`Zone`].  The concrete serialisation format and
//! the actual entity type are chosen by the consumer — the framework only
//! specifies the contract.
//!
//! Derived state (collision snapshot, spatial indices, entity registry) is
//! **not** part of the snapshot — it is rebuilt automatically when entities
//! are re-inserted via [`Zone::spawn`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::vessel::objects::Entity;
use crate::vessel::traits::StaticDataProvider;
use super::container::ZoneContainers;
use super::item_props::{ZoneItemProps, NoItemProps};
use super::zone::Zone;

/// Snapshot of a single zone's state.
///
/// Implementations choose `SaveData` (e.g. a `serde`-friendly struct) and
/// `Error` (conversion / IO error).
///
/// # Saving
///
/// [`save`](Self::save) extracts all entities and containers from the zone
/// into a self-contained `SaveData` value.  Static data, spatial indices,
/// and collision caches are excluded — they are either immutable or derived.
///
/// # Restoring
///
/// [`restore`](Self::restore) builds a fresh `Zone` from `SaveData`.
/// The caller supplies `static_data` separately (it is never serialised).
/// Entities are inserted via [`Zone::spawn`], which automatically rebuilds
/// all derived indices.
pub trait ZoneSnapshot<E: Entity, C: ZoneContainers, P: ZoneItemProps = NoItemProps> {
    /// Serialisable representation of the zone state.
    type SaveData: Send;
    /// Error type for restore operations.
    type Error: std::fmt::Debug;

    /// Extract a snapshot from a live zone.
    fn save(zone: &Zone<E, C, P>) -> Self::SaveData;

    /// Rebuild a zone from a previously saved snapshot.
    ///
    /// `static_data` is provided externally — it is loaded from game files
    /// and is not part of the snapshot.
    fn restore(
        data: Self::SaveData,
        static_data: Option<Arc<dyn StaticDataProvider>>,
    ) -> Result<Zone<E, C, P>, Self::Error>;
}

/// Snapshot of an entire world (all zones).
///
/// Same contract as [`ZoneSnapshot`] but operates on the full
/// `HashMap<u8, Zone>` that lives inside a [`Worker`](super::worker::Worker).
pub trait WorldSnapshot<E: Entity, C: ZoneContainers, P: ZoneItemProps = NoItemProps> {
    /// Serialisable representation of all zones.
    type SaveData: Send;
    /// Error type for restore operations.
    type Error: std::fmt::Debug;

    /// Snapshot every zone in the world.
    fn save(zones: &HashMap<u8, Zone<E, C, P>>) -> Self::SaveData;

    /// Rebuild all zones from a saved snapshot.
    fn restore(
        data: Self::SaveData,
        static_data: Option<Arc<dyn StaticDataProvider>>,
    ) -> Result<HashMap<u8, Zone<E, C, P>>, Self::Error>;
}
