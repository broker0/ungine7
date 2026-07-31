//! Save / restore implementation for the UO shadow continuum.
//!
//! Implements [`ZoneSnapshot`] and [`WorldSnapshot`] from the framework,
//! using JSON as the on-disk format via `serde_json`.
//!
//! # What is saved
//!
//! - All entities (`DemoEntity`) with their full state.
//! - Container inventory (`HashMap<u32, ContainerInfo>`).
//! - Per-item properties (`HashMap<u32, ItemProps>`): names, tooltips, metadata.
//! - Zone `map_id`.
//!
//! # What is NOT saved (and why)
//!
//! - **Static data** (`Arc<StaticWorldData>`) — loaded from game files.
//! - **Spatial indices / collision snapshot** — rebuilt by `Zone::spawn`.
//! - **AI controllers / scheduler** — recreated from scratch on restore.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use framework::continuum::container::{ContainerInfo, HashContainerStore};
use framework::continuum::item_props::ZoneItemProps;
use framework::continuum::snapshot::{WorldSnapshot, ZoneSnapshot};
use framework::continuum::{EntityStore, Zone};
use framework::vessel::traits::StaticDataProvider;

use super::entity::DemoEntity;
use super::item_props::ItemProps;
use super::store::DemoStore;

// ── Serialisable data types ─────────────────────────────────────────────

/// Snapshot of a single zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneSaveData {
    /// Map id (0 = Felucca, 1 = Trammel, …).
    pub map_id: u8,
    /// Every entity in the zone: `(serial, entity)`.
    pub entities: Vec<(u32, DemoEntity)>,
    /// Container contents keyed by container serial.
    pub containers: HashMap<u32, ContainerInfo>,
    /// Per-item properties (names, tooltips, metadata).
    ///
    /// Defaults to empty when loading snapshots saved before this field
    /// existed (`#[serde(default)]`).
    #[serde(default)]
    pub item_props: HashMap<u32, ItemProps>,
}

/// Snapshot of the entire world (all zones + session metadata).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSaveData {
    /// Per-zone snapshots.
    pub zones: Vec<ZoneSaveData>,
    /// Serial of the player character (from `CharLocaleAndBody`).
    pub player_serial: u32,
    /// Current map id the player is on.
    pub player_world: u8,
}

// ── ZoneSnapshot implementation ─────────────────────────────────────────

/// Marker type that carries the [`ZoneSnapshot`] and [`WorldSnapshot`]
/// implementations for the UO engine.
pub struct DemoSnapshot;

impl<P: ZoneItemProps> ZoneSnapshot<DemoEntity, HashContainerStore, P> for DemoSnapshot
where
    P::Value: Into<ItemProps> + From<ItemProps>,
{
    type SaveData = ZoneSaveData;
    type Error = String;

    fn save(zone: &Zone<DemoEntity, HashContainerStore, P>) -> ZoneSaveData {
        ZoneSaveData {
            map_id: zone.map_id,
            entities: zone.collect_entities(),
            containers: zone.containers.containers().clone(),
            item_props: zone.item_props.to_map()
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        }
    }

    fn restore(
        data: ZoneSaveData,
        static_data: Option<Arc<dyn StaticDataProvider>>,
    ) -> Result<Zone<DemoEntity, HashContainerStore, P>, String> {
        let mut zone: Zone<DemoEntity, HashContainerStore, P> = Zone::new(
            data.map_id,
            static_data,
            Box::new(DemoStore::new()),
            896,
            512,
        );

        for (serial, entity) in data.entities {
            zone.spawn(serial, entity);
        }

        zone.containers = HashContainerStore::from_map(data.containers);

        // Restore item properties.
        for (serial, props) in data.item_props {
            zone.item_props.insert(serial, props.into());
        }

        Ok(zone)
    }
}

// ── WorldSnapshot implementation ────────────────────────────────────────

impl<P: ZoneItemProps> WorldSnapshot<DemoEntity, HashContainerStore, P> for DemoSnapshot
where
    P::Value: Into<ItemProps> + From<ItemProps>,
{
    type SaveData = WorldSaveData;
    type Error = String;

    fn save(zones: &HashMap<u8, Zone<DemoEntity, HashContainerStore, P>>) -> WorldSaveData {
        let zone_saves = zones
            .values()
            .map(|z| <DemoSnapshot as ZoneSnapshot<DemoEntity, HashContainerStore, P>>::save(z))
            .collect();

        WorldSaveData {
            zones: zone_saves,
            // Player metadata is set by the caller after save().
            player_serial: 0,
            player_world: 0,
        }
    }

    fn restore(
        data: WorldSaveData,
        static_data: Option<Arc<dyn StaticDataProvider>>,
    ) -> Result<HashMap<u8, Zone<DemoEntity, HashContainerStore, P>>, String> {
        let mut zones = HashMap::new();
        for zone_data in data.zones {
            let map_id = zone_data.map_id;
            let zone = <DemoSnapshot as ZoneSnapshot<DemoEntity, HashContainerStore, P>>::restore(
                zone_data,
                static_data.clone(),
            )?;
            zones.insert(map_id, zone);
        }
        Ok(zones)
    }
}

// ── File I/O (JSON) ─────────────────────────────────────────────────────

/// Save a [`WorldSaveData`] to a JSON file.
pub fn save_to_file(data: &WorldSaveData, path: &Path) -> io::Result<()> {
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// Load a [`WorldSaveData`] from a JSON file.
pub fn load_from_file(path: &Path) -> io::Result<WorldSaveData> {
    let json = std::fs::read_to_string(path)?;
    serde_json::from_str(&json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ── Per-account character persistence ───────────────────────────────────

/// Per-account character-selection records: `account name → characters`.
///
/// This is the disk representation of the demo-server's in-memory
/// `account_characters` map.  Persisting it lets the character-selection
/// screen (and the world each character is in) survive a server restart.
pub type AccountCharacters =
    HashMap<String, Vec<crate::spawn::CharacterRecord>>;

/// Save the per-account character map to a JSON file.
pub fn save_accounts_to_file(data: &AccountCharacters, path: &Path) -> io::Result<()> {
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// Load the per-account character map from a JSON file.
pub fn load_accounts_from_file(path: &Path) -> io::Result<AccountCharacters> {
    let json = std::fs::read_to_string(path)?;
    serde_json::from_str(&json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
