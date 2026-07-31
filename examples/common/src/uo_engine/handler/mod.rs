//! Engine command handler for the shadow worker.
//!
//! [`EngineCommand`] defines the set of commands that can be sent to a
//! [`Zone`] via the shadow worker channel.  [`EngineHandler`] implements
//! [`CommandHandler`] to process them.
//!
//! ## Module structure
//!
//! - [`item_ops`] — item storage, lookup, and manipulation (pick up, drop,
//!   equip, consume, container helpers, serial allocation)
//! - [`mobile_ops`] — mobile movement (step, teleport)
//! - [`combat_ops`] — combat stats (damage, healing, mana, stamina)
//! - [`ingest_ops`] — raw packet ingestion into the zone store

pub mod item_ops;
pub mod mobile_ops;
pub mod combat_ops;
pub mod ingest_ops;
pub mod kill_ops;
pub mod ship_ops;

// Re-export all public types so existing `use handler::{...}` paths
// continue to work without changes.
pub use item_ops::{
    ItemSource, RemainderInfo, PickedUpItem, PickUpResult, PickUpReject,
    DropTarget, DropResult, HeldItemInfo, EquipResult, DisplacedItem, ConsumeResult,
};
pub use mobile_ops::MobileStepResult;
pub use kill_ops::{LootItem, KillResult};

/// HP a player is resurrected with.
///
/// One HP, matching classic UO "you barely cling to life" behaviour.
pub const RESURRECT_HP: u16 = 1;

/// Maximum Chebyshev distance (tiles) between a resurrecting player and
/// their corpse for items to be automatically returned.
pub const RESURRECT_LOOT_RANGE: u16 = 3;

/// Item graphic of the death robe (burial shroud) equipped on a player when
/// they become a ghost.  Matches classic UO behaviour.
pub const DEATH_ROBE_GRAPHIC: u16 = 0x204E;

/// Map id of the offline-character storage zone.
///
/// This is a virtual zone that never maps to any real UO facet; the UO wire
/// protocol never sends this id to clients.  The zone is auto-created by
/// the worker's zone factory on first use.
///
/// Defined here (in `common`) so that `RestoreSnapshot` can exclude the
/// storage zone from crash-recovery orphan collection without needing to
/// know about demo-server–specific constants.
pub const LOGOUT_STORAGE_MAP: u8 = 0xFE;

/// Resolved display information for an item, produced by
/// [`EngineCommand::ResolveItemName`].
///
/// The resolver returns the **base** name (without any stack-count prefix)
/// plus the data a caller needs to format it for a specific transport:
///
/// - The `SingleClick` → `Speech` (0x1C) path prepends the amount for
///   stackable items (`"1543 gold coins"`).
/// - A future `MegaCliloc` (0xD6) path uses `base_name` as the first
///   property line and lets the client render the count itself.
///
/// Keeping the count *out* of `base_name` means both paths share one
/// resolver without double-counting.
#[derive(Debug, Clone)]
pub struct ResolvedItemName {
    /// Display name without any stack-count prefix (e.g. `"gold coins"`).
    pub base_name: String,
    /// The item's graphic id (resolved even when found inside a container).
    pub graphic: u16,
    /// The item's stack amount.
    pub amount: u16,
    /// Whether the graphic is a stackable type (per tiledata / overrides).
    ///
    /// Only stackable items get a leading count in the Speech path.
    pub stackable: bool,
    /// `true` when `base_name` came from an explicit per-instance name
    /// ([`ItemProps::name`]) rather than tiledata / the hardcoded table.
    ///
    /// Explicit names (crafted items, named loot, quest items) are treated
    /// as proper nouns and never get a stack-count prefix.
    pub explicit_name: bool,
}

/// Resolve the ghost body graphic for a given living body graphic.
///
/// Female body (`0x0191`) → female ghost (`0x0193`).
/// Everything else → male ghost (`0x0192`).
pub fn ghost_graphic_for(living_graphic: u16) -> u16 {
    match living_graphic {
        0x0191 => 0x0193, // female ghost
        _ => 0x0192,      // male ghost (default)
    }
}

/// Inspect the static map terrain under a house footprint.
///
/// Returns a [`HouseTerrainResult`] describing whether the rectangle
/// `[x_min..=x_max] × [y_min..=y_max]` is buildable.  Checks, per tile:
///
/// - land must not be `WET` (water) or `IMPASSABLE` (rock/void),
/// - land must be flat (a slope yields [`HouseTerrainResult::Uneven`]),
/// - all footprint tiles must share the same standing Z,
/// - no blocking static (`IMPASSABLE` / `FOLIAGE` / `WALL`, or any static
///   that rises into the foundation) may overlap the footprint.
fn validate_house_terrain<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    x_min: u16,
    y_min: u16,
    x_max: u16,
    y_max: u16,
) -> HouseTerrainResult
where
    P::Value: 'static,
{
    use files::tiledata::TileFlags;

    let Some(provider) = zone.static_data() else {
        return HouseTerrainResult::NoData;
    };
    let world = zone.map_id;

    // Bounds check against the loaded map dimensions.
    if let Some((w, h)) = provider.map_tile_dimensions(world) {
        if x_max >= w || y_max >= h {
            return HouseTerrainResult::OutOfBounds;
        }
    }

    // Statics that overlap the foundation by at most this many tiles of
    // height are tolerated (e.g. flat decorative ground statics).  Anything
    // taller, or carrying a blocking flag, rejects placement.
    const STATIC_CLEARANCE: i32 = 0;

    let mut foundation_z: Option<i8> = None;

    for y in y_min..=y_max {
        for x in x_min..=x_max {
            // ── Land tile ──────────────────────────────────────────────
            let Some((z_base, z_stand, z_top)) = provider.land_tile_z_range(world, x, y) else {
                return HouseTerrainResult::OutOfBounds;
            };
            let Some(land) = provider.land_tile_at(world, x, y) else {
                return HouseTerrainResult::OutOfBounds;
            };
            let land_flags = provider
                .land_tile_def(land.tile_id)
                .map(|d| d.flags)
                .unwrap_or(TileFlags(0));

            if land_flags.has(TileFlags::WET) {
                return HouseTerrainResult::Water;
            }
            if land_flags.has(TileFlags::IMPASSABLE) {
                return HouseTerrainResult::Impassable;
            }
            // A flat tile has all vertices at the same height; any vertical
            // spread (base != top) means the tile is sloped.
            if z_base != z_top {
                return HouseTerrainResult::Uneven;
            }
            // Every footprint tile must share the same ground height.
            match foundation_z {
                None => foundation_z = Some(z_stand),
                Some(fz) if fz != z_stand => return HouseTerrainResult::Uneven,
                Some(_) => {}
            }

            // ── Statics on this tile ───────────────────────────────────
            if let Some(statics) = provider.statics_at(world, x, y) {
                for s in statics {
                    let Some(def) = provider.static_tile_def(s.tile_id) else {
                        continue;
                    };
                    let flags = def.flags;
                    // Trees, walls, rocks, foliage, etc. block placement.
                    if flags.has(TileFlags::IMPASSABLE)
                        || flags.has(TileFlags::FOLIAGE)
                        || flags.has(TileFlags::WALL)
                        || flags.has(TileFlags::WET)
                    {
                        return HouseTerrainResult::Blocked;
                    }
                    // A static whose body rises into the foundation level
                    // also blocks (e.g. a low surface stacked on the ground).
                    let s_top = s.z as i32 + def.height as i32;
                    if s_top > z_stand as i32 + STATIC_CLEARANCE {
                        return HouseTerrainResult::Blocked;
                    }
                }
            }
        }
    }

    match foundation_z {
        Some(foundation_z) => HouseTerrainResult::Ok { foundation_z },
        None => HouseTerrainResult::NoData,
    }
}

/// Inspect the static map terrain under a ship footprint.
///
/// This is the **water** counterpart of [`validate_house_terrain`]: a ship
/// requires every footprint tile to be water (`WET`) and free of blocking
/// statics.  Returns a [`ShipTerrainResult`].
///
/// On success the ship sits at the water surface Z (the land `z_stand` of the
/// water tiles, which is `0` for open sea).
fn validate_ship_terrain<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    x_min: u16,
    y_min: u16,
    x_max: u16,
    y_max: u16,
) -> ShipTerrainResult
where
    P::Value: 'static,
{
    use files::tiledata::TileFlags;

    let Some(provider) = zone.static_data() else {
        return ShipTerrainResult::NoData;
    };
    let world = zone.map_id;

    // Bounds check against the loaded map dimensions.
    if let Some((w, h)) = provider.map_tile_dimensions(world) {
        if x_max >= w || y_max >= h {
            return ShipTerrainResult::OutOfBounds;
        }
    }

    let mut water_z: Option<i8> = None;

    for y in y_min..=y_max {
        for x in x_min..=x_max {
            // A tile counts as water if EITHER the land tile is WET, OR a
            // static water tile (e.g. coastal water drawn as statics) sits on
            // it.  Open sea is land-WET; water along the coast is frequently
            // a static `WET` tile on top of non-water land.
            let mut tile_is_water = false;
            let mut tile_water_z: Option<i8> = None;

            // ── Land tile ──────────────────────────────────────────────
            let Some((_z_base, land_z_stand, _z_top)) = provider.land_tile_z_range(world, x, y)
            else {
                return ShipTerrainResult::OutOfBounds;
            };
            let Some(land) = provider.land_tile_at(world, x, y) else {
                return ShipTerrainResult::OutOfBounds;
            };
            let land_flags = provider
                .land_tile_def(land.tile_id)
                .map(|d| d.flags)
                .unwrap_or(TileFlags(0));

            if land_flags.has(TileFlags::WET) {
                tile_is_water = true;
                tile_water_z = Some(land_z_stand);
            }

            // ── Statics on this tile ───────────────────────────────────
            if let Some(statics) = provider.statics_at(world, x, y) {
                for s in statics {
                    let Some(def) = provider.static_tile_def(s.tile_id) else {
                        continue;
                    };
                    let flags = def.flags;

                    if flags.has(TileFlags::WET) {
                        // Static water surface — this tile is sailable.
                        tile_is_water = true;
                        // Prefer the static water surface height.
                        tile_water_z = Some(s.z);
                        continue;
                    }

                    // A non-water static that blocks (rock, pier, wall,
                    // another structure) makes this tile unsailable.
                    if flags.has(TileFlags::IMPASSABLE) || flags.has(TileFlags::WALL) {
                        return ShipTerrainResult::Blocked;
                    }
                }
            }

            if !tile_is_water {
                return ShipTerrainResult::NotWater;
            }

            if water_z.is_none() {
                water_z = tile_water_z;
            }
        }
    }

    match water_z {
        Some(z) => ShipTerrainResult::Ok { water_z: z },
        None => ShipTerrainResult::NoData,
    }
}

/// Resolve the walkable deck Z of a specific ship multi at a world tile.
///
/// This is a **per-tile** test: `(x, y)` must coincide with at least one
/// multi part that is a walkable deck surface (`SURFACE` and not
/// `IMPASSABLE`).  If the tile only carries blocking parts (a hull side /
/// railing / mast — `IMPASSABLE` or `WALL`) and **no** walkable deck part,
/// the tile is *not* standable and `None` is returned — so a passenger
/// cannot step into the bulwarks or the mast.
///
/// Returns `Some(deck_z)` only when the tile has a walkable deck part (the
/// highest such part's standing height is used).  Returns `None` if the
/// serial is not a multi, has no static parts, or the tile carries no
/// walkable deck part.
///
/// This is used to validate a passenger's step **relative to the deck of the
/// ship they are bound to**, so that walking around while the ship is moving
/// never gets rejected just because the ship's origin shifted a tile between
/// the client's view and the server's authoritative state — while still
/// honouring the per-tile passability of the hull.
pub fn ship_deck_z_at<P: ZoneItemProps>(
    zone: &Zone<DemoEntity, HashContainerStore, P>,
    ship_serial: u32,
    x: u16,
    y: u16,
) -> Option<i8>
where
    P::Value: 'static,
{
    use files::tiledata::TileFlags;

    let provider = zone.static_data()?;
    let (graphic, ox, oy, oz) = match zone.get(ship_serial)? {
        DemoEntity::Multi { graphic, x, y, z, .. } => (*graphic, *x, *y, *z),
        _ => return None,
    };

    let parts = provider.multi_parts(graphic);
    if parts.is_empty() {
        return None;
    }

    let xi = x as i32;
    let yi = y as i32;

    // Per-tile inspection: look only at parts that sit on the requested tile.
    let mut deck_rel_z: Option<i8> = None;
    for part in parts {
        let px = ox as i32 + part.x as i32;
        let py = oy as i32 + part.y as i32;
        if px != xi || py != yi {
            continue;
        }

        let Some(def) = provider.static_tile_def(part.tile_id) else {
            continue;
        };
        let flags = def.flags;

        // A walkable deck plank: SURFACE set, IMPASSABLE not set.
        if flags.has(TileFlags::SURFACE) && !flags.has(TileFlags::IMPASSABLE) {
            let part_z = part.z.clamp(i8::MIN as i16, i8::MAX as i16) as i8;
            let stand = part_z.saturating_add(def.height as i8);
            deck_rel_z = Some(deck_rel_z.map_or(stand, |d| d.max(stand)));
        }
    }

    // The tile is standable iff it carries at least one walkable deck part.
    // Hull sides / railings / the mast (IMPASSABLE / WALL parts with no deck
    // plank) therefore yield `None` and block the step.
    deck_rel_z.map(|rel| oz.saturating_add(rel))
}

/// Result of a [`DealDamage`](EngineCommand::DealDamage) command.
#[derive(Debug, Clone)]
pub struct DamageResult {
    /// HP remaining after damage.
    pub new_hp: u16,
    /// `true` if HP reached 0.
    pub killed: bool,
    /// If killed, the result of the automatic corpse creation.
    /// `None` if the entity was not killed, or if kill failed.
    pub kill: Option<KillResult>,
}

/// Per-zone armor ratings resolved for a specific mobile.
///
/// Returned by `QueryEquipmentArmor` engine command so combat code
/// can pick a random zone and immediately read the AR for that zone.
#[derive(Debug, Clone, Default)]
pub struct ArmorProfile {
    /// AR for each zone: Head, Neck, Chest, Arms, Legs, Shield.
    pub head: u16,
    pub neck: u16,
    pub chest: u16,
    pub arms: u16,
    pub legs: u16,
    pub shield: u16,
    /// `true` if the mobile has a shield equipped (for messaging).
    pub has_shield: bool,
}

impl ArmorProfile {
    /// Get the AR for a given body zone index.
    ///
    /// Zone indices: 0=Head, 1=Neck, 2=Chest, 3=Arms, 4=Legs, 5=Shield.
    pub fn zone_ar_by_index(&self, index: u8) -> u16 {
        match index {
            0 => self.head,
            1 => self.neck,
            2 => self.chest,
            3 => self.arms,
            4 => self.legs,
            5 => self.shield,
            _ => 0,
        }
    }

    /// Total AR across all zones (for status bar display).
    pub fn total(&self) -> u16 {
        self.head + self.neck + self.chest + self.arms + self.legs + self.shield
    }
}

/// Result of a [`ValidateHouseFootprint`](EngineCommand::ValidateHouseFootprint)
/// terrain check.
///
/// Reports whether the land under a prospective house footprint is suitable
/// for placement and, on success, the foundation Z the house should sit at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HouseTerrainResult {
    /// The footprint is buildable; `foundation_z` is the common ground height.
    Ok { foundation_z: i8 },
    /// At least one tile is water (`WET`).
    Water,
    /// At least one land tile is impassable (rock, void, cave, etc.).
    Impassable,
    /// The ground is sloped or the footprint tiles are at differing heights.
    Uneven,
    /// A blocking static (tree, boulder, foliage, wall, …) overlaps the
    /// footprint.
    Blocked,
    /// The footprint extends outside the loaded map bounds.
    OutOfBounds,
    /// No static map data is loaded for this zone — cannot validate terrain.
    NoData,
}

/// Result of a [`ValidateShipFootprint`](EngineCommand::ValidateShipFootprint)
/// terrain check.
///
/// Reports whether the water under a prospective ship footprint is suitable
/// for placement and, on success, the water-surface Z the ship should sit at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipTerrainResult {
    /// The footprint is all clear water; `water_z` is the water surface height.
    Ok { water_z: i8 },
    /// At least one tile is not water (dry land / coast).
    NotWater,
    /// A blocking static (rock, pier, bridge, another structure) overlaps the
    /// footprint.
    Blocked,
    /// The footprint extends outside the loaded map bounds.
    OutOfBounds,
    /// No static map data is loaded for this zone — cannot validate terrain.
    NoData,
}

use bytes::Bytes;

use framework::continuum::{CommandHandler, ContainerContentChange, Zone, WorldEvent};
use framework::continuum::item_props::ZoneItemProps;
use framework::ecumene::{TileBlock, TileRect, Entity as EngineEntity};
use framework::continuum::container::{ContainerInfo, HashContainerStore, ZoneContainers};
use super::entity::DemoEntity;
use super::item_props::ItemProps;
use super::snapshot::ZoneSaveData;
use u_core::{BlockKey, Facing, Heading, MobilePos, Pos3D};

use std::collections::HashMap;
use std::sync::Arc;

use super::serial_alloc::SerialAllocator;

pub enum EngineCommand {
    SpawnEntity {
        entity_id: u32,
        data: DemoEntity,
    },
    RemoveEntity {
        entity_id: u32,
    },
    UpdateEntity {
        entity_id: u32,
        data: DemoEntity,
    },
    GetEntity {
        entity_id: u32,
        reply: tokio::sync::oneshot::Sender<Option<DemoEntity>>,
    },
    QueryAreaEntities {
        area: TileRect,
        reply: tokio::sync::oneshot::Sender<Vec<DemoEntity>>,
    },
    /// Return *all* entities in the zone (for HTTP Query / WorldSave).
    QueryAllEntities {
        reply: tokio::sync::oneshot::Sender<Vec<(u32, DemoEntity)>>,
    },
    TestStep {
        x: u16,
        y: u16,
        z: i8,
        direction: Heading,
        reply: tokio::sync::oneshot::Sender<Option<i8>>,
    },
    ResolveZ {
        x: u16,
        y: u16,
        z_hint: i8,
        direction: Heading,
        reply: tokio::sync::oneshot::Sender<Option<i8>>,
    },
    /// Check line of sight between two 3D points.
    ///
    /// Z values are full coordinates (standing Z + eye offset, typically +14).
    CheckLos {
        x1: u16,
        y1: u16,
        z1: i16,
        x2: u16,
        y2: u16,
        z2: i16,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Move a mobile entity by one tile in the given direction.
    ///
    /// The zone looks up the entity by `serial`, applies UO movement
    /// rules (turn-in-place if heading differs, step if heading matches),
    /// validates passability via `MovementValidator`, and -- if successful
    /// -- updates the entity's position directly in the store.
    ///
    /// Returns `Some(MobileStepResult)` with the new position, or `None`
    /// if the entity was not found or the step was blocked.
    MobileStep {
        serial: u32,
        direction: Facing,
        reply: tokio::sync::oneshot::Sender<Option<MobileStepResult>>,
    },
    /// Teleport a mobile entity to a new position.
    ///
    /// Updates the entity's x/y/z directly in the store without
    /// passability checks.  Used for `.tele` / `.mtele` commands.
    /// If `direction` is `Some`, the entity's facing is also updated.
    TeleportEntity {
        serial: u32,
        x: u16,
        y: u16,
        z: i8,
        direction: Option<u8>,
    },
    /// Reset the zone and repopulate it with the given entities.
    ///
    /// Used during replay seeks: after replaying the log to the target
    /// moment the zone is brought in line with the computed world state.
    ResetZone {
        entities: Vec<DemoEntity>,
        /// Container inventory to restore (empty = clear containers too).
        containers: HashContainerStore,
    },
    /// Mirror a raw S->C packet into the zone's entity store.
    ///
    /// Uses the same parsing logic as `ingest_into_entity_map` to
    /// spawn/update/remove entities.
    ///
    /// When `emit_events` is `false` (default for replay playback),
    /// this is fire-and-forget -- the zone is updated silently.
    ///
    /// When `emit_events` is `true` (used by live mirror streaming),
    /// `WorldEvent::EntitySpawned` / `EntityUpdated` / `EntityRemoved`
    /// events are emitted so that connected UO clients see the changes
    /// in real time.
    IngestPacket {
        data: Bytes,
        emit_events: bool,
    },
    /// Ingest a container-related S->C packet (0x24, 0x25, 0x3C) into
    /// the zone's container store.
    ///
    /// Also marks the corresponding entity as `is_container = true`
    /// in the entity store (for 0x24 packets).
    IngestContainerPacket {
        data: Bytes,
    },
    /// Query a container's contents from the zone.
    GetContainer {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<ContainerInfo>>,
    },
    /// Deal damage to a mobile entity.
    ///
    /// Reduces HP by `amount`, publishes `DamageDealt` event.
    /// If HP reaches 0, automatically kills the mobile and creates a
    /// lootable corpse (equipment is transferred; extra loot can be
    /// injected afterwards via container commands).
    DealDamage {
        serial: u32,
        amount: u16,
        /// Serial of the entity that dealt the damage (0 if unknown).
        source_serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<DamageResult>>,
    },
    /// Heal a mobile entity.
    ///
    /// Increases HP by `amount` (capped at max_hp), publishes `MobileHealed` event.
    HealEntity {
        serial: u32,
        amount: u16,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },
    /// Consume mana from a mobile entity.
    ///
    /// Returns new mana value, or None if entity not found / insufficient mana.
    ConsumeMana {
        serial: u32,
        amount: u16,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },
    /// Modify mana by a delta (can be negative).
    ///
    /// Unlike `ConsumeMana`, this always succeeds (clamped to 0..max).
    /// Returns new mana value, or None if entity not found.
    ModifyMana {
        serial: u32,
        delta: i32,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },
    /// Modify stamina by a delta (can be negative).
    ///
    /// Returns new stamina value, or None if entity not found.
    ModifyStamina {
        serial: u32,
        delta: i32,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },
    /// Modify strength by a delta (can be negative).
    ///
    /// Clamped to 1..u16::MAX. Returns new str value, or None if entity not found.
    ModifyStr {
        serial: u32,
        delta: i32,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },
    /// Modify dexterity by a delta (can be negative).
    ///
    /// Clamped to 1..u16::MAX. Returns new dex value, or None if entity not found.
    ModifyDex {
        serial: u32,
        delta: i32,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },
    /// Modify intelligence by a delta (can be negative).
    ///
    /// Clamped to 1..u16::MAX. Returns new int value, or None if entity not found.
    ModifyInt {
        serial: u32,
        delta: i32,
        reply: tokio::sync::oneshot::Sender<Option<u16>>,
    },

    /// Atomically kill a mobile and create a lootable corpse.
    ///
    /// 1. Reads the mobile's data (graphic, position, equipment).
    /// 2. Allocates serials for the corpse item and all loot items.
    /// 3. Removes the living mobile from the zone.
    /// 4. Spawns the corpse as a `DemoEntity::Item` with `is_container: true`.
    /// 5. Registers the corpse in the container store with equipment + extra loot.
    /// 6. Emits `WorldEvent::MobileKilled` (death animation + corpse clothing)
    ///    and `WorldEvent::EntitySpawned` (persistent corpse for new observers).
    ///
    /// Returns [`KillResult`] on success, `None` if the entity is not found
    /// or not a mobile.
    KillMobile {
        serial: u32,
        /// Additional loot items to place in the corpse (gold, drops, etc.).
        /// Equipment items are automatically transferred from the mobile.
        extra_loot: Vec<LootItem>,
        /// Name to display on the corpse (e.g. "a corpse of an orc").
        corpse_name: Option<String>,
        reply: tokio::sync::oneshot::Sender<Option<KillResult>>,
    },
    /// Atomically kill a *player* and turn them into a ghost.
    ///
    /// Unlike [`KillMobile`](Self::KillMobile), the player mobile is **not**
    /// removed from the world.  Instead:
    /// 1. A corpse item is created carrying all non-newbie equipment.
    ///    Newbie items (see `ItemProps.meta["newbie"]`) stay on the player.
    /// 2. The player's body graphic is swapped to a ghost graphic.
    /// 3. `MobileData::dead` is set and `living_graphic` records the old body.
    /// 4. Emits `WorldEvent::PlayerDied` and `EntitySpawned` (the corpse).
    ///
    /// Returns [`KillResult`] (corpse info) on success, `None` if the entity
    /// is not found, not a mobile, or already dead.
    KillPlayer {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<KillResult>>,
    },
    /// Resurrect a dead player (ghost) back to a living body.
    ///
    /// 1. Restores the living body graphic and clears the dead flag.
    /// 2. Sets HP to [`RESURRECT_HP`].
    /// 3. If a corpse owned by this player is within
    ///    [`RESURRECT_LOOT_RANGE`]
    ///    tiles, all items are returned (equipment re-equipped, loot to backpack)
    ///    and the corpse is removed.
    /// 4. Emits `WorldEvent::PlayerResurrected`.
    ///
    /// Returns `true` if the player was resurrected, `false` if not found or
    /// not currently dead.
    Resurrect {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Set whether a dead player (ghost) is visible to other observers.
    ///
    /// Flips the hidden bit (`0x80`) on the mobile's status flags and emits
    /// `WorldEvent::GhostVisibilityChanged` so observers draw or delete the
    /// ghost.  The ghost's own session always sees its own body.
    SetGhostVisible {
        serial: u32,
        visible: bool,
    },
    /// Apply poison to a mobile.
    ///
    /// Sets the mobile's poison state (level, expiry, per-tick damage and
    /// interval), flips the `MobileFlags` poisoned bit, and emits
    /// `WorldEvent::EntityUpdated` so observers redraw the green health bar.
    /// Periodic damage is delivered by the engine poison sweep (see
    /// `BaseHandler::tick`).  Fire-and-forget.
    ApplyPoison {
        serial: u32,
        /// Poison level (`1..=4` = Lesser..Deadly).
        level: u8,
        /// Total poison duration in milliseconds.
        duration_ms: u64,
        /// Damage applied per tick.
        damage_per_tick: u16,
        /// Interval between ticks in milliseconds.
        tick_interval_ms: u64,
        /// Serial of the mobile that applied the poison (`0` if ambient).
        source_serial: u32,
    },
    /// Cure poison on a mobile.
    ///
    /// Clears the poison state, clears the `MobileFlags` poisoned bit, and
    /// emits `WorldEvent::EntityUpdated`.  Replies `true` if the mobile was
    /// actually poisoned, `false` otherwise.
    CurePoison {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Mark a mobile as a player character (`MobileData::is_player = true`).
    ///
    /// Players become ghosts when killed (see `handle_kill_player`) instead of
    /// being removed like NPCs.  Entities loaded from a `.uolog` default to
    /// `is_player: false`, so playable characters must be promoted explicitly
    /// at login.  No event is emitted — the flag is purely server-side.
    MarkPlayer {
        serial: u32,
    },
    /// Capture a snapshot of the zone's current state.
    ///
    /// Returns a [`ZoneSaveData`] that can be serialised to JSON.
    SaveSnapshot {
        reply: tokio::sync::oneshot::Sender<ZoneSaveData>,
    },
    /// Restore a zone from a previously saved snapshot.
    ///
    /// Equivalent to `ResetZone` but accepts a [`ZoneSaveData`] directly.
    ///
    /// `reset_alloc`: when `true` (the default for an in-game `.load`) the
    /// serial allocator is fully reset and re-seeded from the snapshot data.
    /// Pass `false` when restoring multiple zones at startup (`--load`) where
    /// the allocator was already pre-seeded by
    /// `create_serial_allocator_from_snapshot` — only `mark_occupied` is
    /// called in that case so earlier zones are not wiped.
    RestoreSnapshot {
        data: ZoneSaveData,
        /// `true`  → full reset (in-game `.load`, single zone).
        /// `false` → mark-only (startup `--load`, alloc already correct).
        reset_alloc: bool,
        /// `true`  → CLI `--load` at startup: collect orphaned player
        ///            characters for crash-recovery transfer to storage.
        /// `false` → in-game `.load`: active sessions exist, do not disturb
        ///            currently online players.
        crash_recovery: bool,
    },
    /// Fetch the full collision [`TileBlock`] for a single 8x8 map block.
    ///
    /// The returned block merges static map data, the dynamic collision
    /// snapshot, and entity registry shapes (including houses/multis) --
    /// the same three layers used by `Zone::test_step`.
    ///
    /// Intended for pathfinder tasks that run outside the worker: request
    /// the block, cache it, run A* without holding any zone borrow.
    GetCollisionBlock {
        block: BlockKey,
        reply: tokio::sync::oneshot::Sender<TileBlock>,
    },
    /// Batch-fetch collision [`TileBlock`]s for every 8x8 block that
    /// overlaps the given tile rectangle (inclusive tile coordinates).
    ///
    /// Blocks are in row-major order (bx outer, by inner).
    GetCollisionBlocks {
        tile_left:   u16,
        tile_top:    u16,
        tile_right:  u16,
        tile_bottom: u16,
        reply: tokio::sync::oneshot::Sender<Vec<TileBlock>>,
    },
    /// Validate that the land under a house footprint is buildable.
    ///
    /// Inspects the static map data (land + statics) for every tile in the
    /// inclusive `[x_min..=x_max] × [y_min..=y_max]` rectangle and reports a
    /// [`HouseTerrainResult`].  This is the terrain half of house-placement
    /// validation (the dynamic-entity half is done separately via
    /// [`QueryAreaEntities`](EngineCommand::QueryAreaEntities)).
    ValidateHouseFootprint {
        x_min: u16,
        y_min: u16,
        x_max: u16,
        y_max: u16,
        reply: tokio::sync::oneshot::Sender<HouseTerrainResult>,
    },
    /// Inspect the static map terrain under a prospective **ship** footprint.
    ///
    /// The water counterpart of
    /// [`ValidateHouseFootprint`](EngineCommand::ValidateHouseFootprint):
    /// reports a [`ShipTerrainResult`] for the inclusive
    /// `[x_min..=x_max] × [y_min..=y_max]` rectangle.
    ValidateShipFootprint {
        x_min: u16,
        y_min: u16,
        x_max: u16,
        y_max: u16,
        reply: tokio::sync::oneshot::Sender<ShipTerrainResult>,
    },
    /// Move a ship (multi) by one tile in the given direction.
    ///
    /// Atomically:
    /// 1. Validates the new footprint is all-water and unblocked.
    /// 2. Moves the multi entity (updates `EntityRegistry` collision).
    /// 3. Teleports every mobile standing on the deck along with the ship.
    /// 4. Emits the appropriate `WorldEvent`s for observers.
    ///
    /// Returns `Ok(())` on success, `Err(reason)` if the move is blocked.
    MoveShip {
        serial: u32,
        dx: i32,
        dy: i32,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Turn a ship 90° by swapping its multi graphic to the new facing.
    ///
    /// Validates the rotated footprint, replaces the multi entity with
    /// the new facing graphic, and repositions passengers.
    ///
    /// `quarter_turns_cw` is the rotation applied to the hull, in clockwise
    /// 90° steps (`1` = port→starboard turn right, `-1` = turn left, `2` =
    /// about-face).  It is used to rotate each passenger's position around
    /// the ship origin so they stay on the same spot of the deck.
    ///
    /// Returns `Ok(new_graphic)` on success.
    TurnShip {
        serial: u32,
        new_graphic: u16,
        quarter_turns_cw: i8,
        reply: tokio::sync::oneshot::Sender<Result<u16, String>>,
    },
    /// Find basic item info (serial, graphic, color) by serial.
    ///
    /// Searches three locations in order:
    /// 1. Top-level entities in the zone store.
    /// 2. Equipped items on mobiles (`DemoEntity::Mobile::items`).
    /// 3. Items inside containers.
    ///
    /// Returns `None` if the serial is not found anywhere.
    FindItemInfo {
        serial: u32,
        /// Returns `(serial, graphic, color, amount)`.
        reply: tokio::sync::oneshot::Sender<Option<(u32, u16, u16, u16)>>,
    },
    /// Find which container holds an item.
    ///
    /// Scans all containers in the zone.  Returns the container serial
    /// if the item was found, `None` otherwise.
    FindContainerOfItem {
        item_serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<u32>>,
    },
    /// Remove an item from whichever container holds it.
    ///
    /// Scans all containers in the zone and removes the item with the
    /// given serial.  Returns `true` if the item was found and removed.
    RemoveContainerItem {
        item_serial: u32,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Add or replace an equipped item on a mobile.
    ///
    /// If the mobile already has an item on the same layer, it is replaced.
    /// Emits `EntityUpdated` so other players see the change.
    EquipOnMobile {
        mobile_serial: u32,
        item: packets::world::EquippedItem,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Remove an equipped item from a mobile by item serial.
    ///
    /// Returns the removed `EquippedItem` if found.
    /// Emits `EntityUpdated` so other players see the change.
    UnequipFromMobile {
        mobile_serial: u32,
        item_serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<packets::world::EquippedItem>>,
    },
    /// Update the amount of an item inside a container.
    ///
    /// Scans all containers, finds the item by serial, and sets its amount.
    /// Returns `true` if the item was found and updated.
    UpdateContainerItemAmount {
        item_serial: u32,
        new_amount: u16,
        reply: tokio::sync::oneshot::Sender<bool>,
    },

    // -- High-level atomic item operations --------------------------------

    /// Atomically pick up an item from any source (ground, container,
    /// equipment).
    PickUpItem {
        player_serial: u32,
        item_serial: u32,
        requested_amount: u16,
        /// Maximum Chebyshev distance to ground items (0 = no range check).
        max_range: u16,
        /// Container access policy.
        /// - `None` — GM bypass: all containers are accessible.
        /// - `Some(set)` — only listed container serials are accessible.
        accessible_containers: Option<std::collections::HashSet<u32>>,
        reply: tokio::sync::oneshot::Sender<PickUpResult>,
    },

    /// Atomically drop a held item onto the ground or into a container.
    DropItem {
        player_serial: u32,
        item: HeldItemInfo,
        target: DropTarget,
        /// Container access policy (same semantics as `PickUpItem`).
        accessible_containers: Option<std::collections::HashSet<u32>>,
        reply: tokio::sync::oneshot::Sender<DropResult>,
    },

    /// Atomically consume `amount` units of an item from any source
    /// (container or ground).
    ConsumeItem {
        item_serial: u32,
        amount: u16,
        /// If set, the item must have this graphic to be consumed.
        expected_graphic: Option<u16>,
        reply: tokio::sync::oneshot::Sender<Option<ConsumeResult>>,
    },

    /// Atomically equip a held item onto a mobile.
    EquipFromHeld {
        mobile_serial: u32,
        item: HeldItemInfo,
        layer: packets::layer::Layer,
        reply: tokio::sync::oneshot::Sender<EquipResult>,
    },

    /// Allocate a fresh unique item serial.
    AllocateSerial {
        reply: tokio::sync::oneshot::Sender<u32>,
    },

    /// Allocate a fresh unique mobile serial.
    AllocateMobileSerial {
        reply: tokio::sync::oneshot::Sender<u32>,
    },

    /// Add loot items to an existing container (e.g. a corpse).
    ///
    /// Allocates serials for each item and inserts them into the
    /// container store.  Emits `ContainerContentsUpdated` so that
    /// clients with the container open see the new items.
    ///
    /// Used by combat/magic code to inject loot-table drops into a
    /// corpse that was auto-created by `DealDamage`.
    AddContainerItems {
        container_serial: u32,
        items: Vec<LootItem>,
    },

    // -- Item properties --------------------------------------------------

    /// Get the [`ItemProps`] for an item serial.
    GetItemProps {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<ItemProps>>,
    },

    /// Set (insert or replace) the [`ItemProps`] for an item serial.
    ///
    /// If `props` is `None`, removes any existing properties.
    SetItemProps {
        serial: u32,
        props: Option<ItemProps>,
    },

    /// Resolve the display name of an item by serial.
    ///
    /// Searches all storage tiers (top-level, equipped, inside containers)
    /// so backpack items resolve correctly, then resolves the name through
    /// the chain `ItemProps::name` → tiledata → hardcoded table → hex.
    ///
    /// Returns `None` only when the serial does not refer to any item.
    /// The concrete name chain (which includes the demo-server's hardcoded
    /// table) is implemented by the demo handler; the base [`EngineHandler`]
    /// resolves `ItemProps` / tiledata only.
    ResolveItemName {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<ResolvedItemName>>,
    },

    // -- Weight -----------------------------------------------------------

    /// Compute the total carried weight for a mobile entity.
    ///
    /// Sums weight of all equipped items and all items in the backpack
    /// (recursively, including nested containers).  If `held_item` is
    /// provided, its weight is added too (item on the player's cursor
    /// that has been removed from the world but is still "carried").
    ///
    /// Weight is resolved per-item: first from `ItemProps::weight_override`,
    /// then from the server's weight override table (provided via the
    /// `weight_fn` callback), then from `tiledata.mul`.
    ///
    /// Returns `(current_weight_stones, max_weight_stones)`, or `None`
    /// if the entity is not found or not a mobile.
    ComputeWeight {
        serial: u32,
        /// Item currently on the player's drag-and-drop cursor.
        /// `(serial, graphic, amount)` — weight is resolved the same way
        /// as for any other item.  `None` if the cursor is empty.
        held_item: Option<(u32, u16, u16)>,
        reply: tokio::sync::oneshot::Sender<Option<(u16, u16)>>,
    },

    // -- Armor ------------------------------------------------------------

    /// Query the per-zone armor profile for a mobile entity.
    ///
    /// Iterates the mobile's equipped items, resolves each piece's AR
    /// (from `ItemProps.meta["armor_rating"]` first, then the static
    /// armor template table), and returns an [`ArmorProfile`] with AR
    /// values for every body zone.
    ///
    /// Returns `None` if the entity is not found or not a mobile.
    QueryEquipmentArmor {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<Option<ArmorProfile>>,
    },

    // -- Skills -----------------------------------------------------------

    /// Query the full skill map for a mobile entity.
    ///
    /// Returns a clone of the mobile's `skills` map (id → value/cap/lock),
    /// or `None` if the entity is not found or not a mobile.
    QuerySkills {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<
            Option<std::collections::BTreeMap<u16, super::entity::SkillValue>>,
        >,
    },

    /// Set the lock state of a single skill on a mobile.
    ///
    /// No-op if the entity is not found, is not a mobile, or does not have
    /// the given skill.  Replies with the updated [`SkillValue`](super::entity::SkillValue) on success
    /// (so the caller can send a 0x3A single-update to the client), or
    /// `None` if nothing changed.
    SetSkillLock {
        serial: u32,
        skill_id: u16,
        lock: super::entity::SkillLock,
        reply: tokio::sync::oneshot::Sender<Option<super::entity::SkillValue>>,
    },

    /// Query the total skill bonus (in tenths) granted by a mobile's
    /// equipped "plus" items, keyed by skill id.
    ///
    /// Resolved from each equipped item's `ItemProps.meta` skill-bonus keys.
    /// Returns an empty map if the entity is not found, is not a mobile, or
    /// has no skill-bonus items equipped.
    QuerySkillBonuses {
        serial: u32,
        reply: tokio::sync::oneshot::Sender<std::collections::BTreeMap<u16, u16>>,
    },

    // -- Gold count --------------------------------------------------------

    /// Count the total gold (graphic `0x0EED`) carried by a mobile,
    /// recursively scanning the backpack and all sub-containers.
    ///
    /// `held_item` is `Some((serial, graphic, amount))` if the player
    /// currently has an item on the drag-and-drop cursor.  If it happens
    /// to be gold (or a container holding gold), it will be included in
    /// the total.
    ///
    /// Returns the total gold amount as `u32`, or `None` if the entity
    /// is not found or not a mobile.
    CountGold {
        serial: u32,
        held_item: Option<(u32, u16, u16)>,
        reply: tokio::sync::oneshot::Sender<Option<u32>>,
    },

    // -- Reputation / notoriety -------------------------------------------

    /// Record an act of aggression by `attacker` against `victim`.
    ///
    /// Establishes a mutual aggressor relationship (so the victim may
    /// retaliate without becoming a criminal) and, when the attack is
    /// unprovoked against an *innocent player*, flags the attacker as a
    /// criminal for [`CRIMINAL_FLAG_MS`](crate::uo_engine::notoriety::CRIMINAL_FLAG_MS).
    ///
    /// Emits `EntityUpdated` for any mobile whose reputation changed so
    /// observers re-colour it.
    FlagAggression {
        attacker: u32,
        victim: u32,
    },

    /// Record that `killer` killed `victim`.
    ///
    /// If the victim was an innocent player and the killer is a player,
    /// increments the killer's long-term murder count (which may flip them
    /// to Murderer).  Emits `EntityUpdated` if the killer's reputation
    /// changed.
    RecordKill {
        killer: u32,
        victim: u32,
    },

    /// Set reputation fields directly (used by GM `.karma` / `.murders` /
    /// `.criminal` test commands).  Any `Some` field is applied; `None`
    /// leaves the current value untouched.  Emits `EntityUpdated`.
    SetReputation {
        serial: u32,
        murders: Option<u16>,
        karma: Option<i32>,
        fame: Option<i32>,
        guild_id: Option<Option<u32>>,
        criminal: Option<bool>,
    },
}

/// Result of a [`UseObject`](super::base_handler::BaseCommand::UseObject) command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseObjectResult {
    /// The entity has a controller — interaction was forwarded to it.
    /// Session should skip standard double-click handling.
    HandledByController,
    /// No controller attached — session should do standard handling
    /// (open container, paperdoll, description, etc.).
    NotScripted,
}

pub struct EngineHandler {
    pub serial_alloc: Arc<SerialAllocator>,
    /// Reverse index: equipped-item serial → mobile serial.
    ///
    /// Maintained by all code paths that mutate `DemoEntity::Mobile::items`
    /// (equip, unequip, spawn, remove, update, zone reset/restore).
    /// Replaces the O(N) full-store scan in `resolve_container_position_3d`
    /// and `find_item_info` with an O(1) lookup.
    pub equipment_index: HashMap<u32, u32>,
}

type DemoZone<P> = Zone<DemoEntity, HashContainerStore, P>;

impl EngineHandler {
    /// Deal damage to a mobile and, if it dies, run the appropriate kill
    /// path (ghost for players, corpse + removal for NPCs).
    ///
    /// Shared by the `DealDamage` command and the engine poison sweep so
    /// both reuse the same kill/loot/event logic.  Returns `None` if the
    /// target is not a mobile, otherwise a [`DamageResult`].
    pub fn deal_damage_with_kill<P: ZoneItemProps>(
        &mut self,
        zone: &mut DemoZone<P>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
        serial: u32,
        amount: u16,
        source_serial: u32,
    ) -> Option<DamageResult>
    where
        P::Value: 'static,
    {
        let result = combat_ops::handle_deal_damage(
            zone, event_tx, serial, amount, source_serial,
        );
        result.map(|(new_hp, killed)| {
            let kill = if killed {
                let is_player = zone.get(serial)
                    .and_then(|e| e.mobile())
                    .map(|m| m.is_player)
                    .unwrap_or(false);
                if is_player {
                    kill_ops::handle_kill_player(
                        zone, event_tx, &self.serial_alloc,
                        &mut self.equipment_index, serial,
                    )
                } else {
                    kill_ops::handle_kill_mobile(
                        zone, event_tx, &self.serial_alloc,
                        &mut self.equipment_index,
                        serial, vec![], None,
                    )
                }
            } else {
                None
            };
            DamageResult { new_hp, killed, kill }
        })
    }
}

impl<P: ZoneItemProps> CommandHandler<DemoEntity, HashContainerStore, P> for EngineHandler
where
    P::Value: 'static,
{
    type Command = EngineCommand;

    fn handle(
        &mut self,
        zone: &mut DemoZone<P>,
        cmd: EngineCommand,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) {
        match cmd {
            // -- Entity CRUD --------------------------------------------------

            EngineCommand::SpawnEntity { entity_id, data } => {
                let pos = EngineEntity::pos(&data);
                let map_id = zone.map_id;
                let snap = data.snapshot();
                // Index equipment before spawning.
                if let DemoEntity::Mobile(m) = &data {
                    for eq in &m.items {
                        self.equipment_index.insert(eq.serial, entity_id);
                    }
                }
                zone.spawn(entity_id, data);
                let _ = event_tx.send(WorldEvent::EntitySpawned {
                    map_id,
                    serial: entity_id,
                    pos,
                    entity: snap,
                });
            }
            EngineCommand::RemoveEntity { entity_id } => {
                // Un-index equipment before removing.
                if let Some(entity) = zone.get(entity_id) {
                    if let DemoEntity::Mobile(m) = entity {
                        for eq in &m.items {
                            self.equipment_index.remove(&eq.serial);
                        }
                    }
                }
                let last_pos = zone.get(entity_id)
                    .map(|e| EngineEntity::pos(e))
                    .unwrap_or(Pos3D::new(0, 0, 0));
                let map_id = zone.map_id;
                zone.remove(entity_id);
                zone.item_props.remove(entity_id);
                // Clean up container store (e.g. corpse containers).
                zone.containers.remove(entity_id);
                let _ = event_tx.send(WorldEvent::EntityRemoved {
                    map_id,
                    serial: entity_id,
                    last_pos,
                });
            }
            EngineCommand::UpdateEntity { entity_id, data } => {
                // Un-index old equipment, index new.
                if let Some(old) = zone.get(entity_id) {
                    if let DemoEntity::Mobile(m) = old {
                        for eq in &m.items {
                            self.equipment_index.remove(&eq.serial);
                        }
                    }
                }
                if let DemoEntity::Mobile(m) = &data {
                    for eq in &m.items {
                        self.equipment_index.insert(eq.serial, entity_id);
                    }
                }
                let pos = EngineEntity::pos(&data);
                let map_id = zone.map_id;
                let snap = data.snapshot();
                zone.update(entity_id, data);
                let _ = event_tx.send(WorldEvent::EntityUpdated {
                    map_id,
                    serial: entity_id,
                    pos,
                    entity: snap,
                });
            }
            EngineCommand::GetEntity { entity_id, reply } => {
                let _ = reply.send(zone.get(entity_id).cloned());
            }
            EngineCommand::QueryAreaEntities { area, reply } => {
                let _ = reply.send(zone.query_area(&area));
            }
            EngineCommand::QueryAllEntities { reply } => {
                let _ = reply.send(zone.collect_entities());
            }

            // -- Spatial queries ----------------------------------------------

            EngineCommand::TestStep { x, y, z, direction, reply } => {
                let _ = reply.send(zone.test_step(x, y, z, direction));
            }
            EngineCommand::ResolveZ { x, y, z_hint, direction, reply } => {
                let _ = reply.send(zone.resolve_standing_z(x, y, z_hint, direction));
            }
            EngineCommand::CheckLos { x1, y1, z1, x2, y2, z2, reply } => {
                let _ = reply.send(zone.has_los(x1, y1, z1, x2, y2, z2));
            }
            EngineCommand::GetCollisionBlock { block, reply } => {
                let _ = reply.send(zone.query_collision_block(block));
            }
            EngineCommand::GetCollisionBlocks {
                tile_left, tile_top, tile_right, tile_bottom, reply,
            } => {
                let blocks = zone.query_collision_blocks(
                    tile_left, tile_top, tile_right, tile_bottom,
                );
                let _ = reply.send(blocks);
            }
            EngineCommand::ValidateHouseFootprint {
                x_min, y_min, x_max, y_max, reply,
            } => {
                let result = validate_house_terrain(
                    zone, x_min, y_min, x_max, y_max,
                );
                let _ = reply.send(result);
            }
            EngineCommand::ValidateShipFootprint {
                x_min, y_min, x_max, y_max, reply,
            } => {
                let result = validate_ship_terrain(
                    zone, x_min, y_min, x_max, y_max,
                );
                let _ = reply.send(result);
            }

            // -- Ship movement ------------------------------------------------

            EngineCommand::MoveShip { serial, dx, dy, reply } => {
                let _ = reply.send(
                    ship_ops::handle_move_ship(zone, event_tx, serial, dx, dy),
                );
            }
            EngineCommand::TurnShip { serial, new_graphic, quarter_turns_cw, reply } => {
                let _ = reply.send(
                    ship_ops::handle_turn_ship(zone, event_tx, serial, new_graphic, quarter_turns_cw),
                );
            }

            // -- Mobile movement (mobile_ops) ---------------------------------

            EngineCommand::MobileStep { serial, direction, reply } => {
                let old_mpos = zone.get(serial).and_then(|e| {
                    if let DemoEntity::Mobile(m) = e {
                        Some(MobilePos::new(m.x, m.y, m.z, Facing::new(m.direction)))
                    } else {
                        None
                    }
                });
                let result = mobile_ops::handle_mobile_step(zone, serial, direction);
                if let (Some(step), Some(old)) = (&result, old_mpos) {
                    let new = MobilePos::new(step.x, step.y, step.z, Facing::new(step.direction));
                    let snap = zone.get(serial).and_then(|e| e.snapshot());
                    let _ = event_tx.send(WorldEvent::EntityMoved {
                        map_id: zone.map_id,
                        serial,
                        old_pos: old,
                        new_pos: new,
                        entity: snap,
                        is_teleport: false,
                    });
                }
                let _ = reply.send(result);
            }
            EngineCommand::TeleportEntity { serial, x, y, z, direction } => {
                mobile_ops::handle_teleport(zone, event_tx, serial, x, y, z, direction);
            }

            // -- Combat stats (combat_ops) ------------------------------------

            EngineCommand::DealDamage { serial, amount, source_serial, reply } => {
                let damage_result = self.deal_damage_with_kill(
                    zone, event_tx, serial, amount, source_serial,
                );
                let _ = reply.send(damage_result);
            }
            EngineCommand::HealEntity { serial, amount, reply } => {
                let _ = reply.send(combat_ops::handle_heal(zone, event_tx, serial, amount));
            }
            EngineCommand::ConsumeMana { serial, amount, reply } => {
                let _ = reply.send(combat_ops::handle_consume_mana(
                    zone, event_tx, serial, amount,
                ));
            }
            EngineCommand::ModifyMana { serial, delta, reply } => {
                let _ = reply.send(combat_ops::handle_modify_mana(
                    zone, event_tx, serial, delta,
                ));
            }
            EngineCommand::ModifyStamina { serial, delta, reply } => {
                let _ = reply.send(combat_ops::handle_modify_stamina(
                    zone, event_tx, serial, delta,
                ));
            }
            EngineCommand::ModifyStr { serial, delta, reply } => {
                let _ = reply.send(combat_ops::handle_modify_str(
                    zone, event_tx, serial, delta,
                ));
            }
            EngineCommand::ModifyDex { serial, delta, reply } => {
                let _ = reply.send(combat_ops::handle_modify_dex(
                    zone, event_tx, serial, delta,
                ));
            }
            EngineCommand::ModifyInt { serial, delta, reply } => {
                let _ = reply.send(combat_ops::handle_modify_int(
                    zone, event_tx, serial, delta,
                ));
            }
            EngineCommand::KillMobile { serial, extra_loot, corpse_name, reply } => {
                let _ = reply.send(kill_ops::handle_kill_mobile(
                    zone, event_tx, &self.serial_alloc, &mut self.equipment_index,
                    serial, extra_loot, corpse_name,
                ));
            }
            EngineCommand::KillPlayer { serial, reply } => {
                let _ = reply.send(kill_ops::handle_kill_player(
                    zone, event_tx, &self.serial_alloc, &mut self.equipment_index,
                    serial,
                ));
            }
            EngineCommand::Resurrect { serial, reply } => {
                let _ = reply.send(kill_ops::handle_resurrect(
                    zone, event_tx, &self.serial_alloc, &mut self.equipment_index,
                    serial,
                ));
            }
            EngineCommand::SetGhostVisible { serial, visible } => {
                if let Some(m) = zone.store.get_mut(serial).and_then(|e| e.mobile_mut()) {
                    let new_status = if visible {
                        packets::mobile_flags::MobileFlags(m.status.0 & !0x80)
                    } else {
                        packets::mobile_flags::MobileFlags(m.status.0 | 0x80)
                    };
                    m.status = new_status;
                    let (mx, my) = (m.x, m.y);
                    let snap = zone.get(serial).and_then(|e| e.snapshot());
                    let _ = event_tx.send(WorldEvent::GhostVisibilityChanged {
                        map_id: zone.map_id,
                        serial,
                        visible,
                        x: mx,
                        y: my,
                        entity: snap,
                    });
                }
            }
            EngineCommand::MarkPlayer { serial } => {
                let changed = if let Some(m) =
                    zone.store.get_mut(serial).and_then(|e| e.mobile_mut())
                {
                    m.is_player = true;
                    // Normalize stats to 100/100/100 regardless of what a
                    // replay (.uolog) or creation packet provided, keeping
                    // current resources in sync with their caps (no desync
                    // such as dex=25 but stamina_max=100).
                    m.str_ = 100;
                    m.dex = 100;
                    m.int = 100;
                    m.hits_max = 100;
                    m.mana_max = 100;
                    m.stamina_max = 100;
                    m.hits = m.hits.min(m.hits_max);
                    m.mana = m.mana.min(m.mana_max);
                    m.stamina = m.stamina.min(m.stamina_max);
                    true
                } else {
                    false
                };
                if changed {
                    combat_ops::emit_base_stat_changed(zone, event_tx, serial);
                }
            }
            EngineCommand::ApplyPoison {
                serial, level, duration_ms, damage_per_tick, tick_interval_ms, source_serial,
            } => {
                if level == 0 {
                    return;
                }
                if let Some(m) = zone.store.get_mut(serial).and_then(|e| e.mobile_mut()) {
                    let now = crate::uo_engine::entity::MobileData::now_epoch_ms();
                    m.poison_level = level;
                    m.poison_until_ms = now + duration_ms;
                    m.poison_next_tick_ms = now + tick_interval_ms;
                    m.poison_damage_per_tick = damage_per_tick;
                    m.poison_tick_interval_ms = tick_interval_ms;
                    m.poison_source = source_serial;
                    m.status = m.status.with_poisoned(true);
                    let pos = EngineEntity::pos(zone.store.get(serial).unwrap());
                    let snap = zone.get(serial).and_then(|e| e.snapshot());
                    let _ = event_tx.send(WorldEvent::EntityUpdated {
                        map_id: zone.map_id,
                        serial,
                        pos,
                        entity: snap,
                    });
                }
            }
            EngineCommand::CurePoison { serial, reply } => {
                let mut was_poisoned = false;
                if let Some(m) = zone.store.get_mut(serial).and_then(|e| e.mobile_mut()) {
                    if m.poison_level > 0 {
                        was_poisoned = true;
                        m.poison_level = 0;
                        m.poison_until_ms = 0;
                        m.poison_next_tick_ms = 0;
                        m.poison_damage_per_tick = 0;
                        m.poison_tick_interval_ms = 0;
                        m.poison_source = 0;
                        m.status = m.status.with_poisoned(false);
                        let pos = EngineEntity::pos(zone.store.get(serial).unwrap());
                        let snap = zone.get(serial).and_then(|e| e.snapshot());
                        let _ = event_tx.send(WorldEvent::EntityUpdated {
                            map_id: zone.map_id,
                            serial,
                            pos,
                            entity: snap,
                        });
                    }
                }
                let _ = reply.send(was_poisoned);
            }

            // -- Packet ingestion (ingest_ops) --------------------------------

            EngineCommand::IngestPacket { data, emit_events } => {
                ingest_ops::handle_ingest_packet(zone, event_tx, &data, emit_events, &mut self.equipment_index);
            }
            EngineCommand::IngestContainerPacket { data } => {
                ingest_ops::handle_ingest_container_packet(zone, &data);
            }

            // -- Zone state ---------------------------------------------------

            EngineCommand::ResetZone { entities, mut containers } => {
                zone.clear_all();
                self.equipment_index.clear();

                // Reset the serial allocator and re-mark all entity
                // serials so future allocations don't collide.
                self.serial_alloc.reset();
                for entity in &entities {
                    let serial = EngineEntity::serial(entity);
                    self.serial_alloc.mark_occupied(serial);
                    if let DemoEntity::Mobile(m) = entity {
                        for eq in &m.items {
                            self.serial_alloc.mark_occupied(eq.serial);
                        }
                    }
                }
                // Mark container item serials as occupied.
                for ci in containers.containers().values() {
                    for item in &ci.items {
                        self.serial_alloc.mark_occupied(item.serial);
                    }
                }

                for entity in entities {
                    let serial = EngineEntity::serial(&entity);
                    if let DemoEntity::Mobile(m) = &entity {
                        for eq in &m.items {
                            self.equipment_index.insert(eq.serial, serial);
                        }
                    }
                    zone.spawn(serial, entity);
                }
                containers.rebuild_index();
                zone.containers = containers;
            }
            EngineCommand::GetContainer { serial, reply } => {
                let _ = reply.send(zone.containers.get(serial).cloned());
            }
            EngineCommand::SaveSnapshot { reply } => {
                let data = ZoneSaveData {
                    map_id: zone.map_id,
                    entities: zone.collect_entities(),
                    containers: zone.containers.containers().clone(),
                    item_props: {
                        if std::any::TypeId::of::<P::Value>() == std::any::TypeId::of::<ItemProps>() {
                            zone.item_props.to_map()
                                .into_iter()
                                .filter_map(|(k, v)| {
                                    let any: Box<dyn std::any::Any> = Box::new(v);
                                    any.downcast::<ItemProps>().ok().map(|ip| (k, *ip))
                                })
                                .collect()
                        } else {
                            HashMap::new()
                        }
                    },
                };
                let _ = reply.send(data);
            }
            EngineCommand::RestoreSnapshot { data, reset_alloc, crash_recovery } => {
                // Collect controller IDs before consuming props.
                let controller_metas: Vec<(u32, String)> = data.item_props.iter()
                    .filter_map(|(&serial, props)| {
                        props.get_meta_str("controller")
                            .map(|s| (serial, s.to_string()))
                    })
                    .collect();

                // Collect pending-logout characters before consuming props.
                // These were mid-logout when the snapshot was taken; the
                // restore task will arm the reaper with delay=0 so they are
                // transferred to the storage zone immediately on startup.
                let logout_pending: Vec<(u32, String)> = data.item_props.iter()
                    .filter_map(|(&serial, props)| {
                        props.get_meta_str("logout_pending")
                            .map(|s| (serial, s.to_string()))
                    })
                    .collect();

                // Collect orphaned player characters for crash-recovery.
                //
                // Only at CLI `--load` startup (`crash_recovery == true`) and
                // only for live-world zones (not the storage zone 0xFE, where
                // players are *expected* to sit offline).  Characters already
                // covered by `logout_pending` are excluded to avoid double-arm.
                let logout_pending_serials: std::collections::HashSet<u32> =
                    logout_pending.iter().map(|(s, _)| *s).collect();

                let player_serials: Vec<(u32, String)> = if crash_recovery
                    && zone.map_id != LOGOUT_STORAGE_MAP
                {
                    data.entities.iter()
                        .filter_map(|(serial, ent)| {
                            if let crate::uo_engine::entity::DemoEntity::Mobile(m) = ent {
                                if m.is_player
                                    && !logout_pending_serials.contains(serial)
                                {
                                    let addr = format!(
                                        "{}|{}|{}|{}|{}",
                                        zone.map_id, m.x, m.y, m.z, m.direction,
                                    );
                                    return Some((*serial, addr));
                                }
                            }
                            None
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                zone.clear_all();
                self.equipment_index.clear();

                // Reset the serial allocator and re-mark all restored entity
                // serials as occupied so future allocations don't collide.
                // When `reset_alloc` is false (startup `--load`, multiple
                // zones) the allocator was already pre-seeded by
                // `create_serial_allocator_from_snapshot`; we only add
                // mark_occupied calls so earlier zones are not wiped.
                if reset_alloc {
                    self.serial_alloc.reset();
                }
                for (serial, entity) in &data.entities {
                    self.serial_alloc.mark_occupied(*serial);
                    if let DemoEntity::Mobile(m) = entity {
                        for eq in &m.items {
                            self.serial_alloc.mark_occupied(eq.serial);
                        }
                    }
                }
                // Mark container item serials as occupied.
                for ci in data.containers.values() {
                    for item in &ci.items {
                        self.serial_alloc.mark_occupied(item.serial);
                    }
                }

                for (serial, entity) in data.entities {
                    if let DemoEntity::Mobile(m) = &entity {
                        for eq in &m.items {
                            self.equipment_index.insert(eq.serial, serial);
                        }
                    }
                    zone.spawn(serial, entity);
                }
                zone.containers = HashContainerStore::from_map(data.containers);
                if std::any::TypeId::of::<P::Value>() == std::any::TypeId::of::<ItemProps>() {
                    for (serial, props) in data.item_props {
                        let any: Box<dyn std::any::Any> = Box::new(props);
                        if let Ok(val) = any.downcast::<P::Value>() {
                            zone.item_props.insert(serial, *val);
                        }
                    }
                }

                // Notify listeners so they can re-attach controllers,
                // complete interrupted logout transfers, and handle
                // crash-recovery orphaned players.
                if !controller_metas.is_empty()
                    || !logout_pending.is_empty()
                    || !player_serials.is_empty()
                {
                    let _ = event_tx.send(WorldEvent::SnapshotRestored {
                        map_id: zone.map_id,
                        controller_metas,
                        logout_pending,
                        player_serials,
                    });
                }
            }

            // -- Item lookup (item_ops) ---------------------------------------

            EngineCommand::FindItemInfo { serial, reply } => {
                let _ = reply.send(item_ops::find_item_info(zone, serial, &self.equipment_index));
            }
            EngineCommand::FindContainerOfItem { item_serial, reply } => {
                let _ = reply.send(item_ops::find_container_of_item(zone, item_serial));
            }
            EngineCommand::RemoveContainerItem { item_serial, reply } => {
                let found = zone.containers.remove_item(item_serial);
                let _ = reply.send(found);
            }

            // -- Equipment (inline, short) ------------------------------------

            EngineCommand::EquipOnMobile { mobile_serial, item, reply } => {
                let ok = if let Some(DemoEntity::Mobile(m)) = zone.store.get_mut(mobile_serial) {
                    // Un-index displaced item on the same layer.
                    for eq in m.items.iter().filter(|eq| eq.layer == item.layer) {
                        self.equipment_index.remove(&eq.serial);
                    }
                    m.items.retain(|eq| eq.layer != item.layer);
                    // Index new item.
                    self.equipment_index.insert(item.serial, mobile_serial);
                    m.items.push(item);
                    true
                } else {
                    false
                };
                if ok {
                    if let Some(entity) = zone.store.get(mobile_serial) {
                        let pos = EngineEntity::pos(entity);
                        let snap = entity.snapshot();
                        let _ = event_tx.send(WorldEvent::EntityUpdated {
                            map_id: zone.map_id, serial: mobile_serial, pos, entity: snap,
                        });
                    }
                }
                let _ = reply.send(ok);
            }
            EngineCommand::UnequipFromMobile { mobile_serial, item_serial, reply } => {
                let removed = if let Some(DemoEntity::Mobile(m)) = zone.store.get_mut(mobile_serial) {
                    m.items.iter().position(|eq| eq.serial == item_serial).map(|idx| {
                        self.equipment_index.remove(&item_serial);
                        m.items.remove(idx)
                    })
                } else {
                    None
                };
                if removed.is_some() {
                    if let Some(entity) = zone.store.get(mobile_serial) {
                        let pos = EngineEntity::pos(entity);
                        let snap = entity.snapshot();
                        let _ = event_tx.send(WorldEvent::EntityUpdated {
                            map_id: zone.map_id, serial: mobile_serial, pos, entity: snap,
                        });
                    }
                }
                let _ = reply.send(removed);
            }
            EngineCommand::UpdateContainerItemAmount { item_serial, new_amount, reply } => {
                // O(1) lookup: find the item's container and metadata.
                let mut hit: Option<(u32, u16, u16, u16, u16)> = None;
                if let Some(cs) = zone.containers.find_container_of_item(item_serial) {
                    if let Some(info) = zone.containers.get(cs) {
                        if let Some(item) = info.find_item(item_serial) {
                            hit = Some((cs, item.graphic, item.color, item.x, item.y));
                        }
                    }
                }

                // Second pass: mutate and emit.
                let mut found = false;
                if let Some((container_serial, graphic, color, cx, cy)) = hit {
                    if let Some(info) = zone.containers.get_mut(container_serial) {
                        if let Some(item) = info.find_item_mut(item_serial) {
                            item.amount = new_amount;
                            found = true;
                        }
                    }

                    if found {
                        item_ops::emit_container_event(zone, event_tx, &self.equipment_index, container_serial, vec![
                            ContainerContentChange::ItemUpdated {
                                item_serial,
                                graphic,
                                amount: new_amount,
                                x: cx,
                                y: cy,
                                color,
                            },
                        ]);
                    }
                }

                let _ = reply.send(found);
            }

            // -- High-level atomic item operations (item_ops) -----------------

            EngineCommand::PickUpItem {
                player_serial, item_serial, requested_amount, max_range,
                accessible_containers, reply,
            } => {
                let result = item_ops::handle_pick_up_item(
                    zone, event_tx, &self.serial_alloc,
                    &mut self.equipment_index,
                    player_serial, item_serial,
                    requested_amount, max_range, accessible_containers.as_ref(),
                );
                let _ = reply.send(result);
            }
            EngineCommand::DropItem { player_serial, item, target, accessible_containers, reply } => {
                let _ = reply.send(item_ops::handle_drop_item(
                    zone, event_tx, player_serial, &item, target, accessible_containers.as_ref(),
                    &self.equipment_index,
                ));
            }
            EngineCommand::ConsumeItem { item_serial, amount, expected_graphic, reply } => {
                let _ = reply.send(item_ops::handle_consume_item(
                    zone, event_tx, item_serial, amount, expected_graphic,
                    &self.equipment_index,
                ));
            }
            EngineCommand::EquipFromHeld { mobile_serial, item, layer, reply } => {
                let _ = reply.send(item_ops::handle_equip_from_held(
                    zone, event_tx, &mut self.equipment_index,
                    mobile_serial, &item, layer,
                ));
            }
            EngineCommand::AllocateSerial { reply } => {
                let serial = self.serial_alloc.alloc_item()
                    .expect("item serial space exhausted");
                let _ = reply.send(serial);
            }
            EngineCommand::AllocateMobileSerial { reply } => {
                let serial = self.serial_alloc.alloc_mobile()
                    .expect("mobile serial space exhausted");
                let _ = reply.send(serial);
            }
            EngineCommand::AddContainerItems { container_serial, items } => {
                kill_ops::handle_add_container_items(
                    zone, event_tx, &self.serial_alloc, &self.equipment_index,
                    container_serial, items,
                );
            }

            // -- Item properties (handled by concrete DemoHandler) ------------
            // If they reach EngineHandler, reply with None (not intercepted).

            EngineCommand::GetItemProps { reply, .. } => {
                let _ = reply.send(None);
            }
            EngineCommand::SetItemProps { .. } => {}

            // Name resolution requires the concrete ItemProps type and the
            // demo-server's hardcoded name table, so the concrete DemoHandler
            // intercepts this.  Reply None if it reaches the base handler.
            EngineCommand::ResolveItemName { reply, .. } => {
                let _ = reply.send(None);
            }

            // -- Weight (handled by concrete DemoHandler) ------------------
            // Stub: EngineHandler doesn't know about weight overrides
            // or the concrete ItemProps type, so it replies None.
            EngineCommand::ComputeWeight { reply, .. } => {
                let _ = reply.send(None);
            }

            // -- Armor (handled by concrete DemoHandler) -------------------
            EngineCommand::QueryEquipmentArmor { reply, .. } => {
                let _ = reply.send(None);
            }

            // -- Skills (handled by concrete DemoHandler) ------------------
            // The base EngineHandler has no skill model; the concrete
            // DemoHandler intercepts these.  Reply None if they reach here.
            EngineCommand::QuerySkills { reply, .. } => {
                let _ = reply.send(None);
            }
            EngineCommand::SetSkillLock { reply, .. } => {
                let _ = reply.send(None);
            }
            EngineCommand::QuerySkillBonuses { reply, .. } => {
                let _ = reply.send(std::collections::BTreeMap::new());
            }

            // -- Gold count (handled by concrete DemoHandler) --------------
            EngineCommand::CountGold { reply, .. } => {
                let _ = reply.send(None);
            }

            // -- Reputation / notoriety (combat_ops) -----------------------
            EngineCommand::FlagAggression { attacker, victim } => {
                combat_ops::handle_flag_aggression(zone, event_tx, attacker, victim);
            }
            EngineCommand::RecordKill { killer, victim } => {
                combat_ops::handle_record_kill(zone, event_tx, killer, victim);
            }
            EngineCommand::SetReputation {
                serial, murders, karma, fame, guild_id, criminal,
            } => {
                combat_ops::handle_set_reputation(
                    zone, event_tx, serial, murders, karma, fame, guild_id, criminal,
                );
            }
        }
    }
}
