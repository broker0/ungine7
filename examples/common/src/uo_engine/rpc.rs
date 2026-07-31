//! RPC helpers for communicating with the UO engine worker.
//!
//! ## Generic API
//!
//! The [`EngineRpc`] type alias and [`EngineProxy`] are generic
//! over the command type `C` via the [`WrapEngineCommand`] trait.  This
//! lets the same helpers work with:
//!
//! - `ShadowTx` (replay-proxy, rpc-proxy) — sends `EngineCommand` directly
//! - `DemoWorkerTx` (demo-server) — wraps in `DemoCommand::Engine(...)`
//! - `PathServerWorkerTx` (path-server) — wraps in `PathServerCommand::Engine(...)`
//!
//! Each server just needs one line:
//!
//! ```ignore
//! impl WrapEngineCommand for MyCommand {
//!     fn wrap(cmd: EngineCommand) -> Self { Self::Engine(cmd) }
//! }
//! ```

use bytes::Bytes;
use u_core::{Facing, Heading};
use framework::continuum::container::ContainerInfo;
use framework::continuum::WorkerCommand;
use framework::ecumene::TileRect;
use super::handler::{
    MobileStepResult, EngineCommand, ArmorProfile, DamageResult, HouseTerrainResult,
    ShipTerrainResult,
    PickUpResult, DropResult, DropTarget, HeldItemInfo, EquipResult, ConsumeResult,
    LootItem, KillResult, ResolvedItemName,
};
use super::entity::DemoEntity;
use super::entity::{SkillLock, SkillValue};
use super::item_props::ItemProps;
use super::snapshot::ZoneSaveData;

// ── Trait ────────────────────────────────────────────────────────────────

/// Trait for command enums that can wrap an [`EngineCommand`].
///
/// Implementing this trait allows the [`EngineProxy`] to work with any
/// server's command type (e.g. `DemoCommand`, `PathServerCommand`, or
/// `EngineCommand` itself).
pub trait WrapEngineCommand: Send + 'static {
    fn wrap(cmd: EngineCommand) -> Self;
}

/// Identity implementation — used by `ShadowTx` (replay-proxy, rpc-proxy)
/// where the worker accepts `EngineCommand` directly.
impl WrapEngineCommand for EngineCommand {
    fn wrap(cmd: EngineCommand) -> Self { cmd }
}

// ── Type aliases ─────────────────────────────────────────────────────────

/// Channel sender for a worker that accepts `EngineCommand` directly
/// (shadow continuum used by replay-proxy and rpc-proxy).
pub type ShadowTx = tokio::sync::mpsc::Sender<WorkerCommand<DemoEntity, EngineCommand>>;

/// Generic engine RPC sender — works with any command type that implements
/// [`WrapEngineCommand`].
pub type EngineRpc<C> = tokio::sync::mpsc::Sender<WorkerCommand<DemoEntity, C>>;

// ── EngineProxy ──────────────────────────────────────────────────────────

/// Ergonomic async proxy to a worker command channel.
///
/// Holds a sender and a `world` (map id) so every method only needs
/// the operation-specific arguments.
///
/// ```ignore
/// let engine = EngineProxy::new(tx.clone(), map_id);
/// let entity = engine.get_entity(serial).await;
/// engine.teleport(serial, x, y, z, None).await;
/// ```
pub struct EngineProxy<C: WrapEngineCommand> {
    tx: EngineRpc<C>,
    pub world: u8,
}

impl<C: WrapEngineCommand> Clone for EngineProxy<C> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), world: self.world }
    }
}

impl<C: WrapEngineCommand> EngineProxy<C> {
    /// Create a new proxy targeting the given map.
    pub fn new(tx: EngineRpc<C>, world: u8) -> Self {
        Self { tx, world }
    }

    /// Get a reference to the underlying sender.
    pub fn tx(&self) -> &EngineRpc<C> {
        &self.tx
    }

    /// Create a proxy targeting a different map but sharing the same sender.
    pub fn for_map(&self, world: u8) -> Self {
        Self { tx: self.tx.clone(), world }
    }

    // ── internal helpers ────────────────────────────────────────────────

    /// Send a command that expects a reply (request/response).
    async fn request<R: Send + 'static>(
        &self,
        build: impl FnOnce(tokio::sync::oneshot::Sender<R>) -> EngineCommand,
    ) -> Option<R> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = WorkerCommand::MapCommand(self.world, C::wrap(build(reply_tx)));
        if self.tx.send(cmd).await.is_ok() {
            reply_rx.await.ok()
        } else {
            None
        }
    }

    /// Send a fire-and-forget command (no reply).
    async fn fire(&self, engine_cmd: EngineCommand) {
        let cmd = WorkerCommand::MapCommand(self.world, C::wrap(engine_cmd));
        let _ = self.tx.send(cmd).await;
    }

    // ── entity queries ──────────────────────────────────────────────────

    /// Query a single entity by serial.
    pub async fn get_entity(&self, serial: u32) -> Option<DemoEntity> {
        self.request(|reply| EngineCommand::GetEntity { entity_id: serial, reply })
            .await
            .flatten()
    }

    /// Query all entities within a rectangular area.
    pub async fn query_area(&self, area: TileRect) -> Vec<DemoEntity> {
        self.request(|reply| EngineCommand::QueryAreaEntities { area, reply })
            .await
            .unwrap_or_default()
    }

    /// Query all entities in a rectangular area, returning raw bytes.
    pub async fn items_in_area(&self, area: TileRect) -> Vec<Bytes> {
        let entities = self.query_area(area).await;
        entities.into_iter().map(|e| e.to_raw_bytes()).collect()
    }

    /// Count all entities in the zone.
    pub async fn count_entities(&self) -> usize {
        let area = TileRect { x_min: 0, y_min: 0, x_max: 896 * 8 - 1, y_max: 512 * 8 - 1 };
        self.query_area(area).await.len()
    }

    // ── spatial queries ─────────────────────────────────────────────────

    /// Resolve the standing Z at a tile using a [`Heading`].
    pub async fn resolve_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8> {
        self.request(|reply| EngineCommand::ResolveZ { x, y, z_hint, direction, reply })
            .await
            .flatten()
    }

    /// Resolve the standing Z at a tile using a [`Facing`].
    pub async fn resolve_z_facing(&self, x: u16, y: u16, z_hint: i8, direction: Facing) -> Option<i8> {
        self.resolve_z(x, y, z_hint, direction.heading()).await
    }

    /// Test passability at a tile (step validation without moving).
    pub async fn validate_step(&self, x: u16, y: u16, z: i8, direction: Heading) -> Option<i8> {
        self.request(|reply| EngineCommand::TestStep { x, y, z, direction, reply })
            .await
            .flatten()
    }

    /// Check line of sight between two 3D points.
    pub async fn check_los(
        &self, x1: u16, y1: u16, z1: i16, x2: u16, y2: u16, z2: i16,
    ) -> bool {
        self.request(|reply| EngineCommand::CheckLos { x1, y1, z1, x2, y2, z2, reply })
            .await
            .unwrap_or(true) // default to visible if RPC fails
    }

    /// Validate that the terrain under a house footprint is buildable.
    ///
    /// Inspects the static land + statics over the inclusive `area`
    /// rectangle.  Returns [`HouseTerrainResult::NoData`] if the RPC fails or
    /// no static data is loaded.
    pub async fn validate_house_footprint(&self, area: TileRect) -> HouseTerrainResult {
        self.request(|reply| EngineCommand::ValidateHouseFootprint {
            x_min: area.x_min,
            y_min: area.y_min,
            x_max: area.x_max,
            y_max: area.y_max,
            reply,
        })
        .await
        .unwrap_or(HouseTerrainResult::NoData)
    }

    /// Validate that the water under a ship footprint is open and clear.
    ///
    /// Inspects the static land + statics over the inclusive `area` rectangle.
    /// Returns [`ShipTerrainResult::NoData`] if the RPC fails or no static
    /// data is loaded.
    pub async fn validate_ship_footprint(&self, area: TileRect) -> ShipTerrainResult {
        self.request(|reply| EngineCommand::ValidateShipFootprint {
            x_min: area.x_min,
            y_min: area.y_min,
            x_max: area.x_max,
            y_max: area.y_max,
            reply,
        })
        .await
        .unwrap_or(ShipTerrainResult::NoData)
    }

    // ── ship operations ─────────────────────────────────────────────────

    /// Move a ship multi by `(dx, dy)` tiles (atomically moves passengers too).
    ///
    /// Returns `Ok(())` on success, `Err(reason)` if the move is blocked.
    pub async fn move_ship(&self, serial: u32, dx: i32, dy: i32) -> Result<(), String> {
        self.request(|reply| EngineCommand::MoveShip { serial, dx, dy, reply })
            .await
            .unwrap_or_else(|| Err("RPC failed".into()))
    }

    /// Turn a ship to a new facing by swapping its multi graphic.
    ///
    /// `quarter_turns_cw` is the clockwise 90° rotation applied to the hull
    /// (`1` = right, `-1` = left, `2` = about-face), used to rotate
    /// passengers around the ship origin so they keep their deck spot.
    ///
    /// Returns `Ok(new_graphic)` on success.
    pub async fn turn_ship(
        &self,
        serial: u32,
        new_graphic: u16,
        quarter_turns_cw: i8,
    ) -> Result<u16, String> {
        self.request(|reply| EngineCommand::TurnShip {
            serial,
            new_graphic,
            quarter_turns_cw,
            reply,
        })
        .await
        .unwrap_or_else(|| Err("RPC failed".into()))
    }

    // ── mobile operations ───────────────────────────────────────────────

    /// Move a mobile entity by one tile.
    pub async fn mobile_step(&self, serial: u32, direction: Facing) -> Option<MobileStepResult> {
        self.request(|reply| EngineCommand::MobileStep { serial, direction, reply })
            .await
            .flatten()
    }

    /// Teleport a mobile entity to a new position (fire-and-forget).
    pub async fn teleport(&self, serial: u32, x: u16, y: u16, z: i8, direction: Option<u8>) {
        self.fire(EngineCommand::TeleportEntity { serial, x, y, z, direction }).await;
    }

    // ── entity CRUD ─────────────────────────────────────────────────────

    /// Spawn a new entity in the zone (fire-and-forget).
    pub async fn spawn_entity(&self, serial: u32, entity: DemoEntity) {
        self.fire(EngineCommand::SpawnEntity { entity_id: serial, data: entity }).await;
    }

    /// Remove an entity from the zone (fire-and-forget).
    pub async fn remove_entity(&self, serial: u32) {
        self.fire(EngineCommand::RemoveEntity { entity_id: serial }).await;
    }

    /// Update an existing entity in the zone (fire-and-forget).
    pub async fn update_entity(&self, serial: u32, entity: DemoEntity) {
        self.fire(EngineCommand::UpdateEntity { entity_id: serial, data: entity }).await;
    }

    // ── combat stats ────────────────────────────────────────────────────

    /// Deal damage to a mobile entity.
    ///
    /// Returns [`DamageResult`] on success.  If the target's HP reaches 0,
    /// the engine automatically creates a lootable corpse — see
    /// [`DamageResult::kill`] for the corpse serial.
    pub async fn deal_damage(&self, serial: u32, amount: u16, source_serial: u32) -> Option<DamageResult> {
        self.request(|reply| EngineCommand::DealDamage { serial, amount, source_serial, reply })
            .await
            .flatten()
    }

    /// Heal a mobile entity. Returns `Some(new_hits)` on success.
    pub async fn heal(&self, serial: u32, amount: u16) -> Option<u16> {
        self.request(|reply| EngineCommand::HealEntity { serial, amount, reply })
            .await
            .flatten()
    }

    /// Atomically consume mana. Returns `Some(new_mana)` on success,
    /// `None` if insufficient mana or entity not found.
    pub async fn consume_mana(&self, serial: u32, amount: u16) -> Option<u16> {
        self.request(|reply| EngineCommand::ConsumeMana { serial, amount, reply })
            .await
            .flatten()
    }

    /// Modify a mobile's mana pool. Returns `Some(new_mana)` on success.
    pub async fn modify_mana(&self, serial: u32, delta: i32) -> Option<u16> {
        self.request(|reply| EngineCommand::ModifyMana { serial, delta, reply })
            .await
            .flatten()
    }

    /// Modify a mobile's stamina pool. Returns `Some(new_stamina)` on success.
    pub async fn modify_stamina(&self, serial: u32, delta: i32) -> Option<u16> {
        self.request(|reply| EngineCommand::ModifyStamina { serial, delta, reply })
            .await
            .flatten()
    }

    /// Modify a mobile's strength. Returns `Some(new_str)` on success.
    pub async fn modify_str(&self, serial: u32, delta: i32) -> Option<u16> {
        self.request(|reply| EngineCommand::ModifyStr { serial, delta, reply })
            .await
            .flatten()
    }

    /// Modify a mobile's dexterity. Returns `Some(new_dex)` on success.
    pub async fn modify_dex(&self, serial: u32, delta: i32) -> Option<u16> {
        self.request(|reply| EngineCommand::ModifyDex { serial, delta, reply })
            .await
            .flatten()
    }

    /// Modify a mobile's intelligence. Returns `Some(new_int)` on success.
    pub async fn modify_int(&self, serial: u32, delta: i32) -> Option<u16> {
        self.request(|reply| EngineCommand::ModifyInt { serial, delta, reply })
            .await
            .flatten()
    }

    /// Atomically kill a mobile and create a lootable corpse.
    ///
    /// Returns [`KillResult`] on success, `None` if the entity was not
    /// found or not a mobile.
    pub async fn kill_mobile(
        &self, serial: u32, extra_loot: Vec<LootItem>, corpse_name: Option<String>,
    ) -> Option<KillResult> {
        self.request(|reply| EngineCommand::KillMobile {
            serial, extra_loot, corpse_name, reply,
        }).await.flatten()
    }

    /// Atomically kill a *player* and turn them into a ghost.
    ///
    /// The player mobile is kept in the world (becomes a ghost); non-newbie
    /// equipment is moved to a corpse.  Returns the corpse [`KillResult`], or
    /// `None` if the entity was missing, not a mobile, or already dead.
    pub async fn kill_player(&self, serial: u32) -> Option<KillResult> {
        self.request(|reply| EngineCommand::KillPlayer { serial, reply })
            .await
            .flatten()
    }

    /// Resurrect a dead player (ghost) back to a living body.
    ///
    /// Returns `true` if the player was resurrected, `false` if not found or
    /// not currently dead.
    pub async fn resurrect(&self, serial: u32) -> bool {
        self.request(|reply| EngineCommand::Resurrect { serial, reply })
            .await
            .unwrap_or(false)
    }

    /// Set whether a dead player (ghost) is visible to other observers.
    ///
    /// Fire-and-forget: the ghost's own session always sees its own body, so
    /// no reply is needed.
    pub async fn set_ghost_visible(&self, serial: u32, visible: bool) {
        self.fire(EngineCommand::SetGhostVisible { serial, visible }).await;
    }

    /// Mark a mobile as a player character (`is_player = true`).
    ///
    /// Fire-and-forget.  Ensures the engine routes the mobile's death through
    /// `handle_kill_player` (ghost) rather than `handle_kill_mobile` (NPC).
    pub async fn mark_player(&self, serial: u32) {
        self.fire(EngineCommand::MarkPlayer { serial }).await;
    }

    // ── poison ──────────────────────────────────────────────────────────

    /// Apply poison to a mobile.
    ///
    /// Fire-and-forget.  The engine sets the poison state, flips the
    /// poisoned status flag (green health bar) and delivers periodic damage
    /// via its poison sweep.  Works for both players and NPCs.
    pub async fn apply_poison(
        &self,
        serial: u32,
        level: u8,
        duration_ms: u64,
        damage_per_tick: u16,
        tick_interval_ms: u64,
        source_serial: u32,
    ) {
        self.fire(EngineCommand::ApplyPoison {
            serial, level, duration_ms, damage_per_tick, tick_interval_ms, source_serial,
        }).await;
    }

    /// Cure poison on a mobile.  Returns `true` if the mobile was poisoned.
    pub async fn cure_poison(&self, serial: u32) -> bool {
        self.request(|reply| EngineCommand::CurePoison { serial, reply })
            .await
            .unwrap_or(false)
    }

    // ── reputation / notoriety ──────────────────────────────────────────

    /// Record an act of aggression by `attacker` against `victim`.
    ///
    /// Fire-and-forget.  Establishes a mutual aggressor relationship and
    /// flags the attacker criminal on an unprovoked attack against an
    /// innocent player.
    pub async fn flag_aggression(&self, attacker: u32, victim: u32) {
        self.fire(EngineCommand::FlagAggression { attacker, victim }).await;
    }

    /// Record that `killer` killed `victim`, updating murder counts.
    /// Fire-and-forget.
    pub async fn record_kill(&self, killer: u32, victim: u32) {
        self.fire(EngineCommand::RecordKill { killer, victim }).await;
    }

    /// GM override of reputation fields.  Any `Some` field is applied.
    /// Fire-and-forget.
    pub async fn set_reputation(
        &self,
        serial: u32,
        murders: Option<u16>,
        karma: Option<i32>,
        fame: Option<i32>,
        guild_id: Option<Option<u32>>,
        criminal: Option<bool>,
    ) {
        self.fire(EngineCommand::SetReputation {
            serial, murders, karma, fame, guild_id, criminal,
        }).await;
    }

    // ── containers ──────────────────────────────────────────────────────

    /// Query a container's contents.
    pub async fn get_container(&self, serial: u32) -> Option<ContainerInfo> {
        self.request(|reply| EngineCommand::GetContainer { serial, reply })
            .await
            .flatten()
    }

    /// Send a container-related packet (0x24, 0x25, 0x3C) to the engine (fire-and-forget).
    pub async fn ingest_container(&self, data: Bytes) {
        self.fire(EngineCommand::IngestContainerPacket { data }).await;
    }

    /// Find an item's info (serial, graphic, color, amount) by serial,
    /// searching all container stores in the zone.
    pub async fn find_item_info(&self, serial: u32) -> Option<(u32, u16, u16, u16)> {
        self.request(|reply| EngineCommand::FindItemInfo { serial, reply })
            .await
            .flatten()
    }

    /// Find which container holds a given item.
    pub async fn find_container_of_item(&self, item_serial: u32) -> Option<u32> {
        self.request(|reply| EngineCommand::FindContainerOfItem { item_serial, reply })
            .await
            .flatten()
    }

    /// Remove an item from whichever container holds it.
    pub async fn remove_container_item(&self, item_serial: u32) -> bool {
        self.request(|reply| EngineCommand::RemoveContainerItem { item_serial, reply })
            .await
            .unwrap_or(false)
    }

    /// Update the amount of an item inside a container.
    pub async fn update_container_item_amount(&self, item_serial: u32, new_amount: u16) -> bool {
        self.request(|reply| EngineCommand::UpdateContainerItemAmount { item_serial, new_amount, reply })
            .await
            .unwrap_or(false)
    }

    // ── equipment ───────────────────────────────────────────────────────

    /// Equip an item on a mobile.
    pub async fn equip_on_mobile(&self, mobile_serial: u32, item: packets::world::EquippedItem) -> bool {
        self.request(|reply| EngineCommand::EquipOnMobile { mobile_serial, item, reply })
            .await
            .unwrap_or(false)
    }

    /// Unequip an item from a mobile by item serial.
    pub async fn unequip_from_mobile(&self, mobile_serial: u32, item_serial: u32) -> Option<packets::world::EquippedItem> {
        self.request(|reply| EngineCommand::UnequipFromMobile { mobile_serial, item_serial, reply })
            .await
            .flatten()
    }

    // ── high-level atomic item operations ───────────────────────────────

    /// Atomically pick up an item from any source (ground, container, equipment).
    pub async fn pick_up_item(
        &self, player_serial: u32, item_serial: u32, requested_amount: u16,
        max_range: u16, accessible_containers: Option<std::collections::HashSet<u32>>,
    ) -> PickUpResult {
        self.request(|reply| EngineCommand::PickUpItem {
            player_serial, item_serial, requested_amount, max_range,
            accessible_containers, reply,
        }).await.unwrap_or(PickUpResult::Rejected(super::handler::PickUpReject::NotFound))
    }

    /// Atomically drop a held item onto the ground or into a container.
    pub async fn drop_item(
        &self, player_serial: u32, item: HeldItemInfo, target: DropTarget,
        accessible_containers: Option<std::collections::HashSet<u32>>,
    ) -> DropResult {
        self.request(|reply| EngineCommand::DropItem {
            player_serial, item, target, accessible_containers, reply,
        }).await.unwrap_or(DropResult::Rejected)
    }

    /// Atomically consume `amount` units of an item.
    pub async fn consume_item(
        &self, item_serial: u32, amount: u16, expected_graphic: Option<u16>,
    ) -> Option<ConsumeResult> {
        self.request(|reply| EngineCommand::ConsumeItem {
            item_serial, amount, expected_graphic, reply,
        }).await.flatten()
    }

    /// Atomically equip a held item onto a mobile.
    pub async fn equip_from_held(
        &self, mobile_serial: u32, item: HeldItemInfo, layer: packets::layer::Layer,
    ) -> EquipResult {
        self.request(|reply| EngineCommand::EquipFromHeld {
            mobile_serial, item, layer, reply,
        }).await.unwrap_or(EquipResult::NotAMobile)
    }

    // ── serial allocation ───────────────────────────────────────────────

    /// Allocate a fresh unique item serial from the engine.
    pub async fn allocate_serial(&self) -> u32 {
        self.request(|reply| EngineCommand::AllocateSerial { reply })
            .await
            .unwrap_or(0)
    }

    /// Allocate a fresh unique mobile serial from the engine.
    pub async fn allocate_mobile_serial(&self) -> u32 {
        self.request(|reply| EngineCommand::AllocateMobileSerial { reply })
            .await
            .unwrap_or(0)
    }

    /// Add loot items to an existing container (fire-and-forget).
    ///
    /// Used to inject loot-table drops into a corpse after auto-kill.
    pub async fn add_container_items(&self, container_serial: u32, items: Vec<LootItem>) {
        self.fire(EngineCommand::AddContainerItems { container_serial, items }).await;
    }

    // ── item properties ─────────────────────────────────────────────────

    /// Get the [`ItemProps`] for an item serial.
    pub async fn get_item_props(&self, serial: u32) -> Option<ItemProps> {
        self.request(|reply| EngineCommand::GetItemProps { serial, reply })
            .await
            .flatten()
    }

    /// Set (or remove) the [`ItemProps`] for an item serial (fire-and-forget).
    pub async fn set_item_props(&self, serial: u32, props: Option<ItemProps>) {
        self.fire(EngineCommand::SetItemProps { serial, props }).await;
    }

    /// Resolve the display name of an item by serial (searches all storage
    /// tiers, so backpack/equipped items resolve correctly).
    ///
    /// Returns `None` only when the serial is not an item.
    pub async fn resolve_item_name(&self, serial: u32) -> Option<ResolvedItemName> {
        self.request(|reply| EngineCommand::ResolveItemName { serial, reply })
            .await
            .flatten()
    }

    // ── weight / armor ──────────────────────────────────────────────────

    /// Compute the total carried weight for a mobile entity.
    pub async fn compute_weight(&self, serial: u32, held_item: Option<(u32, u16, u16)>) -> Option<(u16, u16)> {
        self.request(|reply| EngineCommand::ComputeWeight { serial, held_item, reply })
            .await
            .flatten()
    }

    /// Query the per-zone armor profile for a mobile entity.
    pub async fn query_equipment_armor(&self, serial: u32) -> Option<ArmorProfile> {
        self.request(|reply| EngineCommand::QueryEquipmentArmor { serial, reply })
            .await
            .flatten()
    }

    // ── skills ──────────────────────────────────────────────────────────

    /// Query the full skill map (id → value/cap/lock) for a mobile entity.
    pub async fn query_skills(
        &self,
        serial: u32,
    ) -> Option<std::collections::BTreeMap<u16, SkillValue>> {
        self.request(|reply| EngineCommand::QuerySkills { serial, reply })
            .await
            .flatten()
    }

    /// Set the lock state of a single skill on a mobile.
    ///
    /// Returns the updated [`SkillValue`] on success (so the caller can
    /// send a 0x3A single-update), or `None` if the skill was not found.
    pub async fn set_skill_lock(
        &self,
        serial: u32,
        skill_id: u16,
        lock: SkillLock,
    ) -> Option<SkillValue> {
        self.request(|reply| EngineCommand::SetSkillLock { serial, skill_id, lock, reply })
            .await
            .flatten()
    }

    /// Query the total skill bonus (tenths) from a mobile's equipped
    /// "plus" items, keyed by skill id.  Empty if none.
    pub async fn query_skill_bonuses(
        &self,
        serial: u32,
    ) -> std::collections::BTreeMap<u16, u16> {
        self.request(|reply| EngineCommand::QuerySkillBonuses { serial, reply })
            .await
            .unwrap_or_default()
    }

    /// Count the total gold carried by a mobile (recursive backpack scan).
    ///
    /// `held_item` is `Some((serial, graphic, amount))` if the player has
    /// an item on the drag-and-drop cursor.
    pub async fn count_gold(&self, serial: u32, held_item: Option<(u32, u16, u16)>) -> u32 {
        self.request(|reply| EngineCommand::CountGold { serial, held_item, reply })
            .await
            .flatten()
            .unwrap_or(0)
    }

    // ── zone state ──────────────────────────────────────────────────────

    /// Reset a zone with given entities and containers (fire-and-forget).
    pub async fn reset_zone(
        &self, entities: Vec<DemoEntity>, containers: framework::continuum::HashContainerStore,
    ) {
        self.fire(EngineCommand::ResetZone { entities, containers }).await;
    }

    /// Save a zone snapshot.
    pub async fn save_snapshot(&self) -> Option<ZoneSaveData> {
        self.request(|reply| EngineCommand::SaveSnapshot { reply }).await
    }

    /// Restore a zone from a previously saved snapshot (fire-and-forget).
    ///
    /// `crash_recovery` should be `false` here (the default for API callers);
    /// pass `true` only at CLI `--load` startup where there are no active
    /// sessions and orphaned player characters need crash-recovery treatment.
    pub async fn restore_snapshot(&self, data: ZoneSaveData) {
        self.fire(EngineCommand::RestoreSnapshot {
            data,
            reset_alloc: true,
            crash_recovery: false,
        }).await;
    }

    // ── cross-zone ──────────────────────────────────────────────────────

    /// Atomically transfer an entity from one zone to another.
    pub async fn transfer_entity(
        &self, from_map: u8, to_map: u8, serial: u32,
        new_x: u16, new_y: u16, new_z: i8, new_direction: Option<u8>,
    ) -> Result<framework::continuum::TransferResult<DemoEntity>, framework::continuum::TransferError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = WorkerCommand::CrossZone(
            framework::continuum::CrossZoneOp::TransferEntity {
                from_map, to_map, serial,
                new_x, new_y, new_z, new_direction,
                reply: reply_tx,
            },
        );
        if self.tx.send(cmd).await.is_ok() {
            match reply_rx.await {
                Ok(result) => result,
                Err(_) => Err(framework::continuum::TransferError::EntityNotFound),
            }
        } else {
            Err(framework::continuum::TransferError::EntityNotFound)
        }
    }
}
