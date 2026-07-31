//! Container inventory system for the continuum.
//!
//! Tracks container state (items inside containers) using pure domain
//! types — no dependency on network packets or wire formats.
//!
//! Container data is stored **separately** from the entity store — items
//! inside containers have gump-relative coordinates, not world coordinates,
//! and don't participate in collision detection.

use std::collections::HashMap;

// ── Data structures ──────────────────────────────────────────────────────

/// A single item inside a container.
///
/// This is a protocol-agnostic domain type representing an item's visual
/// and logical state within a container's inventory grid.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ContainerItem {
    /// Unique serial of the item.
    pub serial: u32,
    /// Graphic / artwork id.
    pub graphic: u16,
    /// Stack amount (1 for non-stackable items).
    pub amount: u16,
    /// Gump-relative X position inside the container.
    pub x: u16,
    /// Gump-relative Y position inside the container.
    pub y: u16,
    /// Item colour / hue.
    pub color: u16,
    /// Grid index for grid-layout containers (modern clients).
    ///
    /// `None` for legacy (free-form) container layouts.
    pub grid_index: Option<u8>,
}

/// State of a single container.
///
/// Holds the container's identity (serial + gump model) and a flat list
/// of [`ContainerItem`]s representing the current inventory snapshot.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ContainerInfo {
    /// Serial of the container entity.
    pub serial: u32,
    /// Gump model id (determines the visual frame shown to the client).
    pub gump_model: u16,
    /// Items currently inside this container.
    pub items: Vec<ContainerItem>,
}

/// Per-session container cache.
///
/// Tracks containers whose contents were observed by a session.
///
/// This is a **client-side cache** — contrast with [`HashContainerStore`]
/// which is the server-side authoritative store inside a
/// [`Zone`](super::zone::Zone).
#[derive(Debug, Clone, Default)]
pub struct ContainerStore {
    containers: HashMap<u32, ContainerInfo>,
}

impl ContainerStore {
    /// Create an empty container store.
    pub fn new() -> Self {
        Self { containers: HashMap::new() }
    }

    /// Look up a container by serial.
    pub fn get(&self, serial: u32) -> Option<&ContainerInfo> {
        self.containers.get(&serial)
    }

    /// Look up a container by serial (mutable).
    pub fn get_mut(&mut self, serial: u32) -> Option<&mut ContainerInfo> {
        self.containers.get_mut(&serial)
    }

    /// Number of tracked containers.
    pub fn len(&self) -> usize {
        self.containers.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }

    /// Clear all containers.
    pub fn clear(&mut self) {
        self.containers.clear();
    }

    /// Iterate over all containers.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &ContainerInfo)> {
        self.containers.iter().map(|(&k, v)| (k, v))
    }

    /// Remove a deleted object from the container store.
    ///
    /// If `serial` matches a container serial, the entire container entry
    /// is removed.  Otherwise, all containers are searched linearly and
    /// the item is removed from the first container that holds it.
    ///
    /// Returns `true` if anything was removed.
    pub fn remove(&mut self, serial: u32) -> bool {
        remove_from_container_map(&mut self.containers, serial)
    }

    /// Mark a container as opened.
    ///
    /// This is a no-op if the container doesn't exist — the container
    /// entry is created by [`ingest_open`](Self::ingest_open).
    pub fn mark_opened(&mut self, _serial: u32) {
        // Currently no extra state to set beyond creating the entry.
        // Reserved for future use.
    }

    /// Ingest a pre-parsed container open event.
    ///
    /// Creates or updates the container entry for the given serial and
    /// gump model.  Returns the container serial.
    pub fn ingest_open(&mut self, serial: u32, gump_model: u16) -> u32 {
        self.containers
            .entry(serial)
            .and_modify(|existing| {
                existing.gump_model = gump_model;
            })
            .or_insert(ContainerInfo {
                serial,
                gump_model,
                items: Vec::new(),
            });
        serial
    }

    /// Ingest a full content replacement for a container.
    ///
    /// Replaces all items in the container identified by
    /// `container_serial`.  If the container is not yet tracked, a new
    /// entry is created with a default gump model of `0x003C`.
    ///
    /// Items that previously appeared in *other* containers are removed
    /// from those containers first (prevents duplicates when replaying
    /// logs).
    pub fn ingest_content(&mut self, container_serial: u32, items: Vec<ContainerItem>) -> u32 {
        ingest_content_inner(container_serial, items, &mut self.containers, None)
    }

    /// Ingest a single item upsert into a container.
    ///
    /// If the item already exists in the target container, its fields are
    /// updated.  If it exists in a *different* container, it is removed
    /// from there first.
    pub fn ingest_item_upsert(&mut self, container_serial: u32, item: ContainerItem) -> u32 {
        ingest_item_upsert_inner(container_serial, item, &mut self.containers, None)
    }

    /// Read-only access to the underlying container map.
    pub fn containers(&self) -> &HashMap<u32, ContainerInfo> {
        &self.containers
    }
}

// ── ZoneContainers trait ─────────────────────────────────────────────────

/// Trait for container storage inside a [`Zone`](super::zone::Zone).
///
/// The default type parameter on `Zone` is [`NoContainers`] — a no-op
/// stub that stores nothing.  Use [`HashContainerStore`] when you need
/// actual container tracking.
pub trait ZoneContainers: Send + Default {
    fn get(&self, serial: u32) -> Option<&ContainerInfo>;
    fn get_mut(&mut self, serial: u32) -> Option<&mut ContainerInfo>;
    fn insert(&mut self, serial: u32, info: ContainerInfo);
    fn remove(&mut self, serial: u32);
    fn clear(&mut self);
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool { self.len() == 0 }

    /// O(1) lookup: which container holds this item?
    ///
    /// Returns the container serial if the item is tracked, `None` otherwise.
    /// Implementations without a reverse index should return `None`.
    fn find_container_of_item(&self, item_serial: u32) -> Option<u32> {
        let _ = item_serial;
        None
    }

    /// Remove a container entry and return it (for cross-zone transfer).
    ///
    /// Like [`remove`](Self::remove) but returns the [`ContainerInfo`]
    /// so it can be re-inserted into another zone.
    fn remove_entry(&mut self, serial: u32) -> Option<ContainerInfo> {
        // Default implementation: no-op.
        let _ = serial;
        None
    }
}

/// No-op container storage — containers are not supported.
#[derive(Debug, Clone, Default)]
pub struct NoContainers;

impl ZoneContainers for NoContainers {
    fn get(&self, _: u32) -> Option<&ContainerInfo> { None }
    fn get_mut(&mut self, _: u32) -> Option<&mut ContainerInfo> { None }
    fn insert(&mut self, _: u32, _: ContainerInfo) {}
    fn remove(&mut self, _: u32) {}
    fn clear(&mut self) {}
    fn len(&self) -> usize { 0 }
}

/// Full container storage backed by a [`HashMap`], with a reverse index
/// from item serials to their containing container serial.
///
/// The reverse index (`item_index`) enables O(1) lookups for "which
/// container holds item X?" — replacing linear scans over all containers.
///
/// Both fields are private; access the container map via
/// [`containers()`](Self::containers) / [`containers_mut()`](Self::containers_mut),
/// and use [`find_container_of_item()`](Self::find_container_of_item) for
/// indexed lookups.
#[derive(Debug, Clone)]
pub struct HashContainerStore {
    /// Container serial -> container info.
    containers: HashMap<u32, ContainerInfo>,
    /// Reverse index: item serial -> container serial that holds it.
    item_index: HashMap<u32, u32>,
}

impl Default for HashContainerStore {
    fn default() -> Self {
        Self {
            containers: HashMap::new(),
            item_index: HashMap::new(),
        }
    }
}

impl HashContainerStore {
    pub fn new() -> Self { Self::default() }

    /// Read-only access to the underlying container map.
    pub fn containers(&self) -> &HashMap<u32, ContainerInfo> {
        &self.containers
    }

    /// Mutable access to the underlying container map.
    ///
    /// **Warning:** mutations through this reference bypass the reverse
    /// index.  Prefer the dedicated methods ([`remove_item`](Self::remove_item),
    /// [`remove_item_from`](Self::remove_item_from), etc.) when possible.  Call
    /// [`rebuild_index()`](Self::rebuild_index) after bulk mutations.
    pub fn containers_mut(&mut self) -> &mut HashMap<u32, ContainerInfo> {
        &mut self.containers
    }

    /// Ingest a pre-parsed container open event.
    pub fn ingest_open(&mut self, serial: u32, gump_model: u16) -> u32 {
        self.containers
            .entry(serial)
            .and_modify(|existing| {
                existing.gump_model = gump_model;
            })
            .or_insert(ContainerInfo {
                serial,
                gump_model,
                items: Vec::new(),
            });
        serial
    }

    /// Ingest a full content replacement for a container.
    pub fn ingest_content(&mut self, container_serial: u32, items: Vec<ContainerItem>) -> u32 {
        ingest_content_inner(
            container_serial,
            items,
            &mut self.containers,
            Some(&mut self.item_index),
        )
    }

    /// Ingest a single item upsert into a container.
    pub fn ingest_item_upsert(&mut self, container_serial: u32, item: ContainerItem) -> u32 {
        ingest_item_upsert_inner(
            container_serial,
            item,
            &mut self.containers,
            Some(&mut self.item_index),
        )
    }

    /// O(1) lookup: which container holds this item?
    pub fn find_container_of_item(&self, item_serial: u32) -> Option<u32> {
        self.item_index.get(&item_serial).copied()
    }

    /// Remove an item from whichever container holds it (O(1) lookup).
    ///
    /// Returns `true` if the item was found and removed.
    pub fn remove_item(&mut self, item_serial: u32) -> bool {
        if let Some(cs) = self.item_index.remove(&item_serial) {
            if let Some(info) = self.containers.get_mut(&cs) {
                info.remove_item(item_serial);
            }
            true
        } else {
            false
        }
    }

    /// Remove a specific item from a known container.
    ///
    /// Returns `true` if the item was found and removed.
    pub fn remove_item_from(&mut self, container_serial: u32, item_serial: u32) -> bool {
        self.item_index.remove(&item_serial);
        if let Some(info) = self.containers.get_mut(&container_serial) {
            info.remove_item(item_serial)
        } else {
            false
        }
    }

    /// Rebuild the reverse index from scratch.
    ///
    /// Call this after wholesale replacement of the container map
    /// (e.g. `ResetZone`, `RestoreSnapshot`).
    pub fn rebuild_index(&mut self) {
        self.item_index.clear();
        for (&cs, info) in &self.containers {
            for item in &info.items {
                self.item_index.insert(item.serial, cs);
            }
        }
    }

    /// Construct from a raw `HashMap` and immediately build the reverse index.
    pub fn from_map(containers: HashMap<u32, ContainerInfo>) -> Self {
        let mut store = Self {
            containers,
            item_index: HashMap::new(),
        };
        store.rebuild_index();
        store
    }
}

impl From<ContainerStore> for HashContainerStore {
    fn from(store: ContainerStore) -> Self {
        Self::from_map(store.containers)
    }
}

impl From<HashMap<u32, ContainerInfo>> for ContainerStore {
    fn from(map: HashMap<u32, ContainerInfo>) -> Self {
        Self { containers: map }
    }
}

impl ZoneContainers for HashContainerStore {
    fn get(&self, serial: u32) -> Option<&ContainerInfo> { self.containers.get(&serial) }
    fn get_mut(&mut self, serial: u32) -> Option<&mut ContainerInfo> { self.containers.get_mut(&serial) }

    fn find_container_of_item(&self, item_serial: u32) -> Option<u32> {
        self.item_index.get(&item_serial).copied()
    }

    fn insert(&mut self, serial: u32, info: ContainerInfo) {
        // Index all items in the new container.
        for item in &info.items {
            self.item_index.insert(item.serial, serial);
        }
        self.containers.insert(serial, info);
    }

    fn remove(&mut self, serial: u32) {
        // Remove all item index entries for this container.
        if let Some(info) = self.containers.get(&serial) {
            for item in &info.items {
                self.item_index.remove(&item.serial);
            }
        }
        self.containers.remove(&serial);
    }

    fn remove_entry(&mut self, serial: u32) -> Option<ContainerInfo> {
        // Remove all item index entries for this container.
        if let Some(info) = self.containers.get(&serial) {
            for item in &info.items {
                self.item_index.remove(&item.serial);
            }
        }
        self.containers.remove(&serial)
    }

    fn clear(&mut self) {
        self.containers.clear();
        self.item_index.clear();
    }

    fn len(&self) -> usize { self.containers.len() }
}

// ── ContainerInfo API ────────────────────────────────────────────────────

impl ContainerInfo {
    /// Create a new container with the given serial and gump model.
    pub fn new(serial: u32, gump_model: u16) -> Self {
        Self { serial, gump_model, items: Vec::new() }
    }

    /// Container serial.
    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// Gump model (determines the visual frame).
    pub fn gump_model(&self) -> u16 {
        self.gump_model
    }

    /// Number of items currently in the container.
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Replace all items.
    pub fn set_items(&mut self, items: Vec<ContainerItem>) {
        self.items = items;
    }

    /// Upsert a single item — update if it exists (by serial), otherwise
    /// append.
    pub fn upsert_item(&mut self, item: ContainerItem) {
        if let Some(existing) = self.items.iter_mut().find(|i| i.serial == item.serial) {
            *existing = item;
        } else {
            self.items.push(item);
        }
    }

    /// Remove an item from this container by serial.
    ///
    /// Returns `true` if the item was found and removed.
    pub fn remove_item(&mut self, serial: u32) -> bool {
        let len_before = self.items.len();
        self.items.retain(|i| i.serial != serial);
        self.items.len() != len_before
    }

    /// Collect all item serials in this container.
    pub fn item_serials(&self) -> Vec<u32> {
        self.items.iter().map(|i| i.serial).collect()
    }

    /// Find an item by serial.
    pub fn find_item(&self, serial: u32) -> Option<&ContainerItem> {
        self.items.iter().find(|i| i.serial == serial)
    }

    /// Find an item by serial (mutable).
    pub fn find_item_mut(&mut self, serial: u32) -> Option<&mut ContainerItem> {
        self.items.iter_mut().find(|i| i.serial == serial)
    }
}

// ── Private helpers ──────────────────────────────────────────────────────

/// Remove a deleted object from a container map.
///
/// If `serial` matches a container serial, the entire entry is removed.
/// Otherwise, all containers are searched linearly and the item is removed
/// from the first container that holds it.
///
/// Returns `true` if anything was removed.
fn remove_from_container_map(
    containers: &mut HashMap<u32, ContainerInfo>,
    serial: u32,
) -> bool {
    // 1. Is it a container itself?
    if containers.remove(&serial).is_some() {
        return true;
    }
    // 2. Linear scan: is it an item inside some container?
    for info in containers.values_mut() {
        if info.remove_item(serial) {
            return true;
        }
    }
    false
}

/// Ingest a full content replacement for a container.
///
/// When `item_index` is `Some`, the reverse index is maintained in
/// lockstep.  Pass `None` for client-side stores that don't need it.
fn ingest_content_inner(
    container_serial: u32,
    items: Vec<ContainerItem>,
    containers: &mut HashMap<u32, ContainerInfo>,
    mut item_index: Option<&mut HashMap<u32, u32>>,
) -> u32 {
    let new_serials: Vec<u32> = items.iter().map(|i| i.serial).collect();

    // Remove every incoming item from any OTHER container to prevent
    // duplicates when replaying logs.
    if let Some(ref mut idx) = item_index {
        // O(1) per item via reverse index.
        for &s in &new_serials {
            if let Some(&old_cs) = idx.get(&s) {
                if old_cs != container_serial {
                    if let Some(info) = containers.get_mut(&old_cs) {
                        info.remove_item(s);
                    }
                    idx.remove(&s);
                }
            }
        }
    } else {
        // No index — fall back to linear scan.
        for (&other_cs, info) in containers.iter_mut() {
            if other_cs != container_serial {
                for &s in &new_serials {
                    info.remove_item(s);
                }
            }
        }
    }

    // Remove old item index entries for the target container
    // (its content is about to be fully replaced).
    if let Some(ref mut idx) = item_index {
        if let Some(existing) = containers.get(&container_serial) {
            for item in &existing.items {
                idx.remove(&item.serial);
            }
        }
    }

    containers
        .entry(container_serial)
        .and_modify(|existing| {
            existing.items = items.clone();
        })
        .or_insert_with(|| ContainerInfo {
            serial: container_serial,
            gump_model: 0x003C,
            items: items.clone(),
        });

    // Add new item index entries.
    if let Some(ref mut idx) = item_index {
        for s in new_serials {
            idx.insert(s, container_serial);
        }
    }

    container_serial
}

/// Ingest a single item upsert into a container.
fn ingest_item_upsert_inner(
    container_serial: u32,
    item: ContainerItem,
    containers: &mut HashMap<u32, ContainerInfo>,
    mut item_index: Option<&mut HashMap<u32, u32>>,
) -> u32 {
    let item_serial = item.serial;

    // Remove from any OTHER container to prevent duplicates
    // when replaying logs where an item moved between containers.
    if let Some(ref mut idx) = item_index {
        // O(1) via reverse index.
        if let Some(&old_cs) = idx.get(&item_serial) {
            if old_cs != container_serial {
                if let Some(info) = containers.get_mut(&old_cs) {
                    info.remove_item(item_serial);
                }
                idx.remove(&item_serial);
            }
        }
    } else {
        for (&other_cs, info) in containers.iter_mut() {
            if other_cs != container_serial {
                info.remove_item(item_serial);
            }
        }
    }

    containers
        .entry(container_serial)
        .and_modify(|existing| {
            existing.upsert_item(item.clone());
        })
        .or_insert_with(|| {
            ContainerInfo {
                serial: container_serial,
                gump_model: 0x003C,
                items: vec![item.clone()],
            }
        });

    // Update reverse index for the upserted item.
    if let Some(ref mut idx) = item_index {
        idx.insert(item_serial, container_serial);
    }

    container_serial
}
