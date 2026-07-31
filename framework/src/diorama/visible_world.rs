//! Session-local visible world — entities, equipment index, and container
//! cache in a single structure.
//!
//! [`VisibleWorld`] tracks which objects the client of a particular session
//! currently has on screen, together with container contents observed via
//! S→C packets.  It is owned by a single session and is **not** shared
//! between sessions.
//!
//! Each entity is stored as a [`WorldEntity`] — a thin wrapper around the
//! full deserialised packet structure ([`ObjectInfo`], [`ObjectInfoSA`], or
//! [`DrawMobile`]), enriched with session-level metadata (e.g.
//! `is_container`).  This preserves **all** fields from the original server
//! packet (hue, equipment list, status flags, notoriety, etc.) so that
//! bootstrap can reconstruct the world faithfully for newly-connected
//! clients.
//!
//! Primary responsibilities:
//! - Track objects the server has sent to this session — with full fidelity.
//! - Compute the set-difference when the player moves to determine which
//!   new items to send from the shard-level object store.
//! - Act as a per-session object source for walkability checks (via
 //!   [`CompositeTileProvider`](super::composite_tiles::CompositeTileProvider)).
//! - Maintain a reverse index from equipment item serials to their owner
//!   mobile serial for efficient lookup and deletion.
//! - Cache container contents observed by the session (0x24, 0x25, 0x3C).

use std::collections::{HashMap, HashSet};

use log::trace;

use packets::character::UpdateMobile;
use packets::interaction::{DeleteObject, EquipItem};
use packets::layer::Layer;
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::{
    DrawMobile, DrawMobileExtended, EquippedItem, ObjectDataType, ObjectInfo,
    ObjectInfoSA, PacketList,
};

use crate::continuum::container::ContainerStore;
use crate::ecumene::TileRect;

// ── VisibleKind ──────────────────────────────────────────────────────────

/// Object kind discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleKind {
    /// Regular item (weapon, furniture, wall, etc.).
    Item,
    /// Multi-object root (house, boat, castle, etc.).
    Multi,
    /// Mobile (player, NPC, creature).
    Mobile,
}

// ── EntityData ───────────────────────────────────────────────────────────

/// Discriminated payload — the full deserialised packet structure with
/// variant-specific metadata.
#[derive(Debug, Clone)]
pub enum EntityData {
    /// Pre-SA item or multi (from `0x1A ObjectInfo`).
    ItemClassic {
        /// The deserialised packet.
        packet: ObjectInfo,
        /// `true` if this item was opened as a container (via `0x24`).
        is_container: bool,
    },
    /// SA+ item or multi (from `0xF3 ObjectInfoSA` / `0xF7 PacketList`).
    ItemSA {
        /// The deserialised packet.
        packet: ObjectInfoSA,
        /// `true` if this item was opened as a container (via `0x24`).
        is_container: bool,
    },
    /// Mobile with full equipment (from `0x78 DrawMobile` /
    /// `0xD3 DrawMobileExtended`).
    Mobile {
        /// The deserialised packet (including equipment list).
        packet: DrawMobile,
    },
}

// ── WorldEntity ──────────────────────────────────────────────────────────

/// A world object with full packet data and session-level metadata.
#[derive(Debug, Clone)]
pub struct WorldEntity {
    /// Object serial — duplicated at the top level for O(1) access
    /// without matching on [`EntityData`].
    pub serial: u32,

    /// Full deserialised packet payload.
    pub data: EntityData,
}

impl WorldEntity {
    // ── Accessor helpers ─────────────────────────────────────────────

    /// Tile X coordinate.
    #[inline]
    pub fn x(&self) -> u16 {
        match &self.data {
            EntityData::ItemClassic { packet, .. } => packet.x,
            EntityData::ItemSA { packet, .. } => packet.x,
            EntityData::Mobile { packet } => packet.x,
        }
    }

    /// Tile Y coordinate.
    #[inline]
    pub fn y(&self) -> u16 {
        match &self.data {
            EntityData::ItemClassic { packet, .. } => packet.y,
            EntityData::ItemSA { packet, .. } => packet.y,
            EntityData::Mobile { packet } => packet.y,
        }
    }

    /// Tile Z coordinate.
    #[inline]
    pub fn z(&self) -> i8 {
        match &self.data {
            EntityData::ItemClassic { packet, .. } => packet.z,
            EntityData::ItemSA { packet, .. } => packet.z,
            EntityData::Mobile { packet } => packet.z,
        }
    }

    /// Primary graphic (body type for mobiles, item/multi graphic for objects).
    #[inline]
    pub fn graphic(&self) -> u16 {
        match &self.data {
            EntityData::ItemClassic { packet, .. } => {
                packet.multi_id().unwrap_or(packet.graphic)
            }
            EntityData::ItemSA { packet, .. } => packet.graphic,
            EntityData::Mobile { packet } => packet.graphic,
        }
    }

    /// Object kind.
    #[inline]
    pub fn kind(&self) -> VisibleKind {
        match &self.data {
            EntityData::ItemClassic { packet, .. } => {
                if packet.is_multi() { VisibleKind::Multi } else { VisibleKind::Item }
            }
            EntityData::ItemSA { packet, .. } => {
                if packet.data_type == ObjectDataType::Multi {
                    VisibleKind::Multi
                } else {
                    VisibleKind::Item
                }
            }
            EntityData::Mobile { .. } => VisibleKind::Mobile,
        }
    }

    /// Whether this object is a mobile (player / NPC).
    #[inline]
    pub fn is_mobile(&self) -> bool {
        matches!(self.data, EntityData::Mobile { .. })
    }

    /// Whether this object is a multi (house, boat, etc.).
    #[inline]
    pub fn is_multi(&self) -> bool {
        match &self.data {
            EntityData::ItemClassic { packet, .. } => packet.is_multi(),
            EntityData::ItemSA { packet, .. } => {
                packet.data_type == ObjectDataType::Multi
            }
            EntityData::Mobile { .. } => false,
        }
    }

    /// Whether this object is a regular item (not multi, not mobile).
    #[inline]
    pub fn is_item(&self) -> bool {
        !self.is_mobile() && !self.is_multi()
    }

    /// Whether this item has been observed as a container (via `0x24
    /// DrawContainer`).  Always returns `false` for mobiles.
    #[inline]
    pub fn is_container(&self) -> bool {
        match &self.data {
            EntityData::ItemClassic { is_container, .. } => *is_container,
            EntityData::ItemSA { is_container, .. } => *is_container,
            EntityData::Mobile { .. } => false,
        }
    }

    /// Set the `is_container` flag on an item entity.
    ///
    /// Has no effect on mobiles.
    #[inline]
    pub fn set_container(&mut self, value: bool) {
        match &mut self.data {
            EntityData::ItemClassic { is_container, .. } => *is_container = value,
            EntityData::ItemSA { is_container, .. } => *is_container = value,
            EntityData::Mobile { .. } => {}
        }
    }
}

// ── VisibleWorld ──────────────────────────────────────────────────────────

/// Per-session visible world: entities, equipment index, container cache,
/// and view management in a single structure.
#[derive(Debug, Clone)]
pub struct VisibleWorld {
    /// Entities by serial.
    entities: HashMap<u32, WorldEntity>,

    /// Reverse index: equipment item serial → owner mobile serial.
    ///
    /// Populated automatically when a [`DrawMobile`] is upserted.  Used to
    /// efficiently handle `DeleteObject` packets that target an equipped
    /// item (the item must be removed from the owner mobile's equipment
    /// list, not from the top-level entity map).
    equipment_index: HashMap<u32, u32>,

    /// Container cache: containers whose contents were observed by this
    /// session.  Populated from S→C packets `0x24`, `0x3C`, `0x25`.
    containers: ContainerStore,

    /// Current view rectangle.
    view_rect: TileRect,

    /// View range (Chebyshev radius).
    view_range: u16,
}

impl VisibleWorld {
    /// Create an empty visible world centred at `(cx, cy)` with the given range.
    pub fn new(cx: u16, cy: u16, range: u16) -> Self {
        Self {
            entities: HashMap::new(),
            equipment_index: HashMap::new(),
            containers: ContainerStore::new(),
            view_rect: TileRect::from_view(cx, cy, range),
            view_range: range,
        }
    }

    // ── Low-level entity access ──────────────────────────────────────

    /// Get a reference to an entity by serial (top-level entities only).
    pub fn get(&self, serial: u32) -> Option<&WorldEntity> {
        self.entities.get(&serial)
    }

    /// Get a mutable reference to an entity by serial (top-level entities only).
    pub fn get_mut(&mut self, serial: u32) -> Option<&mut WorldEntity> {
        self.entities.get_mut(&serial)
    }

    /// Lookup any serial — returns the owner mobile serial if the serial
    /// belongs to an equipped item, or the serial itself if it is a
    /// top-level entity, or `None` if unknown.
    pub fn lookup_serial(&self, serial: u32) -> Option<u32> {
        if self.entities.contains_key(&serial) {
            Some(serial)
        } else {
            self.equipment_index.get(&serial).copied()
        }
    }

    // ── Equipment index maintenance ─────────────────────────────────

    /// Rebuild the equipment index entries for a mobile.
    fn index_equipment(&mut self, mob_serial: u32, items: &[EquippedItem]) {
        for eq in items {
            self.equipment_index.insert(eq.serial, mob_serial);
        }
    }

    /// Remove all equipment index entries for a mobile.
    fn unindex_equipment(&mut self, items: &[EquippedItem]) {
        for eq in items {
            self.equipment_index.remove(&eq.serial);
        }
    }

    // ── Typed upserts ────────────────────────────────────────────────

    /// Insert or update a world entity.
    ///
    /// If the entity is a mobile, the equipment index is updated.
    /// If a previous entity with the same serial existed and was a mobile,
    /// its old equipment index entries are cleaned up first.
    fn upsert_entity(&mut self, entity: WorldEntity) {
        let serial = entity.serial;

        // Clean up old equipment index if replacing a mobile.
        if let Some(old) = self.entities.get(&serial) {
            if let EntityData::Mobile { packet: ref old_mob } = old.data {
                // Collect serials to remove to avoid borrow conflict.
                let old_eq_serials: Vec<u32> = old_mob.items.iter()
                    .map(|eq| eq.serial)
                    .collect();
                for eq_serial in old_eq_serials {
                    self.equipment_index.remove(&eq_serial);
                }
            }
        }

        // Index new equipment if this is a mobile.
        if let EntityData::Mobile { packet: ref mob } = entity.data {
            self.index_equipment(serial, &mob.items);
        }

        self.entities.insert(serial, entity);
    }

    /// Insert / update from a pre-parsed `ObjectInfo (0x1A)`.
    pub fn upsert_object_info(&mut self, obj: &ObjectInfo) {
        trace!(
            "[visible] 0x1A upsert serial={:#010X} ({},{})",
            obj.object_id, obj.x, obj.y,
        );
        // Preserve is_container flag if entity already exists as an item.
        let was_container = self.entities
            .get(&obj.object_id)
            .map_or(false, |e| e.is_container());
        self.upsert_entity(WorldEntity {
            serial: obj.object_id,
            data: EntityData::ItemClassic {
                packet: obj.clone(),
                is_container: was_container,
            },
        });
    }

    /// Insert / update from a pre-parsed `ObjectInfoSA (0xF3)`.
    pub fn upsert_object_info_sa(&mut self, obj: &ObjectInfoSA) {
        trace!(
            "[visible] 0xF3 upsert serial={:#010X} ({},{})",
            obj.serial, obj.x, obj.y,
        );
        let was_container = self.entities
            .get(&obj.serial)
            .map_or(false, |e| e.is_container());
        self.upsert_entity(WorldEntity {
            serial: obj.serial,
            data: EntityData::ItemSA {
                packet: obj.clone(),
                is_container: was_container,
            },
        });
    }

    /// Insert / update each sub-item from a pre-parsed `PacketList (0xF7)`.
    pub fn upsert_packet_list(&mut self, list: &PacketList) {
        for obj in &list.items {
            self.upsert_object_info_sa(obj);
        }
    }

    /// Insert / update from a pre-parsed `DrawMobile (0x78)`.
    pub fn upsert_draw_mobile(&mut self, mob: &DrawMobile) {
        trace!(
            "[visible] 0x78 upsert serial={:#010X} ({},{}) items={}",
            mob.serial, mob.x, mob.y, mob.items.len(),
        );
        self.upsert_entity(WorldEntity {
            serial: mob.serial,
            data: EntityData::Mobile { packet: mob.clone() },
        });
    }

    /// Insert / update from a pre-parsed `DrawMobileExtended (0xD3)`.
    ///
    /// The extended packet is converted to a standard [`DrawMobile`] for
    /// storage — the three extra `u16` fields are always zero in practice.
    pub fn upsert_draw_mobile_ext(&mut self, mob: &DrawMobileExtended) {
        trace!(
            "[visible] 0xD3 upsert serial={:#010X} ({},{}) items={}",
            mob.serial, mob.x, mob.y, mob.items.len(),
        );
        let dm = DrawMobile {
            serial: mob.serial,
            graphic: mob.graphic,
            x: mob.x,
            y: mob.y,
            z: mob.z,
            direction: mob.direction,
            color: mob.color,
            status: mob.status.clone(),
            notoriety: mob.notoriety,
            items: mob.items.clone(),
        };
        self.upsert_entity(WorldEntity {
            serial: mob.serial,
            data: EntityData::Mobile { packet: dm },
        });
    }

    /// Apply an `UpdateMobile (0x77)` packet to an existing mobile.
    ///
    /// Updates position, graphic, hue, direction, status flags, and
    /// notoriety.  Equipment list is preserved.  If the mobile is not
    /// in the visible set, the packet is ignored.
    pub fn apply_update_mobile(&mut self, upd: &UpdateMobile) {
        if let Some(entity) = self.entities.get_mut(&upd.serial) {
            if let EntityData::Mobile { ref mut packet } = entity.data {
                trace!(
                    "[visible] 0x77 update serial={:#010X} ({},{},{})",
                    upd.serial, upd.x, upd.y, upd.z,
                );
                packet.graphic = upd.model;
                packet.x = upd.x;
                packet.y = upd.y;
                packet.z = upd.z;
                packet.direction = upd.direction;
                packet.color = upd.hue;
                packet.status = upd.status_flags.clone();
                packet.notoriety = upd.notoriety;
            }
        }
    }

    /// Apply an `EquipItem (0x2E)` packet — upsert a single equipment
    /// slot on a mobile.
    ///
    /// If the target mobile is not in the visible set, the packet is
    /// ignored.
    pub fn apply_equip_item(&mut self, equip: &EquipItem) {
        if let Some(entity) = self.entities.get_mut(&equip.player_serial) {
            if let EntityData::Mobile { ref mut packet } = entity.data {
                trace!(
                    "[visible] 0x2E equip item={:#010X} on mobile={:#010X} layer={:?}",
                    equip.item_serial, equip.player_serial, equip.layer,
                );
                // Remove any previous item on the same layer.
                packet.items.retain(|eq| eq.layer != equip.layer);
                // Remove old index entry for the same serial if re-equipped.
                self.equipment_index.remove(&equip.item_serial);

                let eq_item = EquippedItem {
                    serial: equip.item_serial,
                    graphic: equip.graphic,
                    layer: equip.layer,
                    color: if equip.color != 0 {
                        Some(equip.color)
                    } else {
                        None
                    },
                };
                packet.items.push(eq_item);
                self.equipment_index.insert(equip.item_serial, equip.player_serial);
            }
        }
    }

    /// Remove an entity from the visible world.
    ///
    /// If `serial` belongs to an equipped item (found via the equipment
    /// index), the item is removed from the owner mobile's equipment list
    /// instead of the top-level entity map.
    ///
    /// Also removes the serial from the container cache (either as a
    /// container itself or as an item inside a container).
    pub fn remove(&mut self, serial: u32) {
        // Clean up container cache.
        self.containers.remove(serial);

        // Check if this is an equipped item first.
        if let Some(owner_serial) = self.equipment_index.remove(&serial) {
            if let Some(entity) = self.entities.get_mut(&owner_serial) {
                if let EntityData::Mobile { ref mut packet } = entity.data {
                    trace!(
                        "[visible] 0x1D remove equipped item={:#010X} from mobile={:#010X}",
                        serial, owner_serial,
                    );
                    packet.items.retain(|eq| eq.serial != serial);
                }
            }
            return;
        }

        // Top-level entity removal.
        if let Some(old) = self.entities.remove(&serial) {
            // Clean up equipment index if this was a mobile.
            if let EntityData::Mobile { ref packet } = old.data {
                self.unindex_equipment(&packet.items);
            }
        }
    }

    /// Mark an entity as a container and ingest the open event into the
    /// container cache.
    ///
    /// Called when a `0x24 DrawContainer` packet is observed for this
    /// serial.  Has no effect on the entity flag if the entity is not in
    /// the visible world or is a mobile.
    pub fn mark_container(&mut self, serial: u32, gump_model: u16) {
        if let Some(entity) = self.entities.get_mut(&serial) {
            entity.set_container(true);
        }
        self.containers.ingest_open(serial, gump_model);
    }

    // ── Convenience: raw-bytes ingestion ──────────────────────────────

    /// Feed a raw S→C packet and update the visible world accordingly.
    ///
    /// This is a convenience wrapper that parses the packet and delegates
    /// to the appropriate typed method.  Prefer the typed methods when
    /// the packet has already been parsed (e.g. inside
    /// [`ObserverPipeline`](super::pipeline::ObserverPipeline)).
    ///
    /// Handled packets:
    /// - `0x1A ObjectInfo` — upsert (items and multi roots).
    /// - `0xF3 ObjectInfoSA` — upsert (items and multi roots).
    /// - `0xF7 PacketList` — upsert each sub-item.
    /// - `0x78 DrawMobile` — upsert.
    /// - `0xD3 DrawMobileExtended` — upsert.
    /// - `0x77 UpdateMobile` — update existing mobile.
    /// - `0x2E EquipItem` — update equipment on mobile.
    /// - `0x1D DeleteObject` — remove (handles equipped items + containers).
    /// - `0x24 DrawContainer` — marks entity as container + caches.
    /// - `0x25 AddItemToContainer` — upserts item into container cache.
    /// - `0x3C ContainerContent` — replaces container content cache.
    /// - Everything else — ignored.
    pub fn ingest_packet(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        match data[0] {
            id if id == ObjectInfo::ID => {
                if let Ok(obj) = ObjectInfo::from_bytes(data) {
                    self.upsert_object_info(&obj);
                }
            }
            id if id == ObjectInfoSA::ID => {
                if let Ok(obj) = ObjectInfoSA::from_bytes(data) {
                    self.upsert_object_info_sa(&obj);
                }
            }
            id if id == PacketList::ID => {
                if let Ok(list) = PacketList::from_bytes(data) {
                    self.upsert_packet_list(&list);
                }
            }
            id if id == DrawMobile::ID => {
                if let Ok(mob) = DrawMobile::parse(data, false) {
                    self.upsert_draw_mobile(&mob);
                }
            }
            id if id == DrawMobileExtended::ID => {
                if let Ok(mob) = DrawMobileExtended::parse(data, false) {
                    self.upsert_draw_mobile_ext(&mob);
                }
            }
            id if id == UpdateMobile::ID => {
                if let Ok(upd) = UpdateMobile::from_bytes(data) {
                    self.apply_update_mobile(&upd);
                }
            }
            id if id == EquipItem::ID => {
                if let Ok(equip) = EquipItem::from_bytes(data) {
                    self.apply_equip_item(&equip);
                }
            }
            id if id == DeleteObject::ID => {
                if let Ok(d) = DeleteObject::from_bytes(data) {
                    trace!("[visible] 0x1D remove serial={:#010X}", d.serial);
                    self.remove(d.serial);
                }
            }
            // ── Container packets ───────────────────────────────────
            0x24 => {
                if data.len() >= 5 {
                    let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                    let gump_model = if data.len() >= 7 {
                        u16::from_be_bytes([data[5], data[6]])
                    } else {
                        0
                    };
                    self.mark_container(serial, gump_model);
                }
            }
            0x25 => {
                use packets::interaction::AddItemToContainer;
                use packets::traits::ManualPacket;
                if let Ok(add) = AddItemToContainer::from_bytes(data) {
                    let cs = add.container_serial();
                    let item = crate::continuum::ContainerItem {
                        serial: add.serial(),
                        graphic: add.graphic(),
                        amount: add.amount(),
                        x: add.x(),
                        y: add.y(),
                        color: add.color(),
                        grid_index: add.grid_index(),
                    };
                    self.containers.ingest_item_upsert(cs, item);
                }
            }
            0x3C => {
                use packets::interaction::ContainerContent;
                use packets::traits::ManualPacket;
                if let Ok(cc) = ContainerContent::from_bytes(data) {
                    if let Some(cs) = cc.container_serial() {
                        let items = crate::diorama::container_items_from_content(&cc);
                        self.containers.ingest_content(cs, items);
                    }
                }
            }
            _ => {}
        }
    }

    // ── View management ──────────────────────────────────────────────

    /// Move the view centre and return (new_rect, new_strips).
    ///
    /// This is pure geometry — **no entities are evicted**.  The caller
    /// (typically [`SessionView::update_view`](super::session_view::SessionView))
    /// is responsible for deciding which entities to remove from the
    /// visible set based on the new rectangle.
    pub fn set_view_center(&mut self, cx: u16, cy: u16) -> (TileRect, Vec<TileRect>) {
        let new_rect = TileRect::from_view(cx, cy, self.view_range);
        let strips = new_rect.difference(&self.view_rect);
        self.view_rect = new_rect;
        (new_rect, strips)
    }

    /// Serial numbers of entities whose origin `(x, y)` is outside `rect`.
    ///
    /// Used by [`SessionView::update_view`](super::session_view::SessionView)
    /// to determine which entities to evict from the visible set.
    pub fn serials_outside_rect(&self, rect: &TileRect) -> Vec<u32> {
        self.entities
            .iter()
            .filter(|(_, e)| {
                let x = e.x();
                let y = e.y();
                x < rect.x_min || x > rect.x_max
                    || y < rect.y_min || y > rect.y_max
            })
            .map(|(&serial, _)| serial)
            .collect()
    }

    /// Update the view centre (e.g. after a move) and return the new strips
    /// that were not visible before.
    pub fn update_view(&mut self, cx: u16, cy: u16) -> Vec<TileRect> {
        let new_rect = TileRect::from_view(cx, cy, self.view_range);
        let strips = new_rect.difference(&self.view_rect);
        self.view_rect = new_rect;

        // Collect serials of entities that are now outside the view rectangle
        // so we can clean up their equipment indices after removal.
        let to_remove: Vec<u32> = self.entities
            .iter()
            .filter(|(_, e)| {
                let x = e.x();
                let y = e.y();
                x < new_rect.x_min || x > new_rect.x_max
                    || y < new_rect.y_min || y > new_rect.y_max
            })
            .map(|(&serial, _)| serial)
            .collect();

        for serial in to_remove {
            if let Some(old) = self.entities.remove(&serial) {
                if let EntityData::Mobile { ref packet } = old.data {
                    self.unindex_equipment(&packet.items);
                }
            }
        }

        strips
    }

    /// Update the view range (e.g. from `0xC8 ClientViewRange`).
    pub fn set_view_range(&mut self, range: u16) {
        self.view_range = range;
    }

    /// Clear all entities and containers (e.g. on world change).
    pub fn clear(&mut self) {
        self.entities.clear();
        self.equipment_index.clear();
        self.containers.clear();
    }

    // ── Queries ───────────────────────────────────────────────────────

    /// Check whether a mobile has a mount equipped (`Layer::Mount`).
    ///
    /// Returns `false` if the serial is not found or is not a mobile.
    pub fn is_mounted(&self, serial: u32) -> bool {
        if let Some(entity) = self.entities.get(&serial) {
            if let EntityData::Mobile { ref packet } = entity.data {
                return packet.items.iter().any(|eq| eq.layer == Layer::Mount);
            }
        }
        false
    }

    /// Current view rectangle.
    pub fn view_rect(&self) -> &TileRect {
        &self.view_rect
    }

    /// Current view range.
    pub fn view_range(&self) -> u16 {
        self.view_range
    }

    /// Number of visible entities.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Read-only access to the container cache.
    pub fn containers(&self) -> &ContainerStore {
        &self.containers
    }

    /// Mutable access to the container cache.
    pub fn containers_mut(&mut self) -> &mut ContainerStore {
        &mut self.containers
    }

    /// Entities at a specific tile (for walkability checks).
    pub fn items_at(&self, x: u16, y: u16) -> impl Iterator<Item = &WorldEntity> {
        self.entities
            .values()
            .filter(move |e| e.x() == x && e.y() == y)
    }

    /// Entities within a block (for block-level queries).
    ///
    /// Returns an iterator of `(ox, oy, &WorldEntity)` where `ox` and `oy`
    /// are offsets within the block (0..8).
    pub fn items_in_block(&self, block: u_core::BlockKey) -> impl Iterator<Item = (u8, u8, &WorldEntity)> {
        let origin = block.origin();
        let x_min = origin.x;
        let y_min = origin.y;
        let x_max = x_min + 7;
        let y_max = y_min + 7;
        self.entities.values().filter_map(move |e| {
            let ex = e.x();
            let ey = e.y();
            if ex >= x_min && ex <= x_max && ey >= y_min && ey <= y_max {
                let ox = (ex - x_min) as u8;
                let oy = (ey - y_min) as u8;
                Some((ox, oy, e))
            } else {
                None
            }
        })
    }

    /// Set of serial numbers currently in the visible set.
    pub fn serials(&self) -> HashSet<u32> {
        self.entities.keys().copied().collect()
    }

    /// Iterate over all visible entities.
    pub fn iter(&self) -> impl Iterator<Item = &WorldEntity> {
        self.entities.values()
    }
}

// ── Backward-compatible aliases ──────────────────────────────────────────

/// Backward-compatible alias for [`WorldEntity`].
pub type VisibleItem = WorldEntity;

/// Backward-compatible alias for [`VisibleWorld`].
pub type VisibleSet = VisibleWorld;

// ── Entity impl ────────────────────────────────────────────────────────

impl crate::vessel::Entity for WorldEntity {
    fn serial(&self) -> u32 {
        self.serial
    }

    fn pos(&self) -> u_core::Pos3D {
        u_core::Pos3D::new(self.x(), self.y(), self.z())
    }

    fn graphic(&self) -> u16 {
        self.graphic()
    }

    fn is_mobile(&self) -> bool {
        self.is_mobile()
    }

    fn is_multi(&self) -> bool {
        self.is_multi()
    }

    fn is_container(&self) -> bool {
        self.is_container()
    }

    fn set_pos(&mut self, pos: u_core::Pos3D) {
        match &mut self.data {
            EntityData::ItemClassic { packet, .. } => {
                packet.x = pos.x;
                packet.y = pos.y;
                packet.z = pos.z;
            }
            EntityData::ItemSA { packet, .. } => {
                packet.x = pos.x;
                packet.y = pos.y;
                packet.z = pos.z;
            }
            EntityData::Mobile { packet } => {
                packet.x = pos.x;
                packet.y = pos.y;
                packet.z = pos.z;
            }
        }
    }

    fn set_direction(&mut self, direction: u8) {
        if let EntityData::Mobile { packet } = &mut self.data {
            packet.direction = direction;
        }
    }

    fn extract_shapes(
        &self,
        sd: &(impl crate::vessel::StaticDataProvider + ?Sized),
    ) -> Vec<(u16, u16, crate::vessel::TileShape)> {
        use crate::vessel::TileShape;

        if self.is_mobile() {
            return vec![];
        }

        if self.is_multi() {
            let parts = sd.multi_parts(self.graphic());
            let mut result = Vec::new();
            for part in parts {
                if part.flags == 0 {
                    continue;
                }
                if let Some(def) = sd.static_tile_def(part.tile_id) {
                    let px = (self.x() as i32 + part.x as i32).clamp(0, u16::MAX as i32) as u16;
                    let py = (self.y() as i32 + part.y as i32).clamp(0, u16::MAX as i32) as u16;
                    let pz = self.z().saturating_add(
                        part.z.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                    );
                    result.push((px, py, TileShape::from_static(pz, def)));
                }
            }
            result
        } else {
            // Item: single tile shape
            if let Some(def) = sd.static_tile_def(self.graphic()) {
                vec![(self.x(), self.y(), TileShape::from_static(self.z(), def))]
            } else {
                vec![]
            }
        }
    }
}
