//! [`Entity`] — base trait for all UO world entities.
//!
//! This trait captures the minimal interface shared by every object in the
//! game world: serial, position, graphic, and kind discriminators.  It is
//! implemented by [`WorldEntity`](crate::diorama::visible_world::WorldEntity)
//! (client-side packet-backed entity) and by replay-proxy's own `DemoEntity`
//! enum (server-side lightweight entity).
//!
//! The trait is defined here in `vessel/` so that [`EntityRegistry`](crate::ecumene::EntityRegistry) and
//! `continuum/zone` can be generic over the entity type without depending on
//! `diorama/` or `packets`.

use bytes::Bytes;
use u_core::Pos3D;

use super::tile_shape::TileShape;
use super::traits::StaticDataProvider;

/// Snapshot of an entity's state at the time a [`WorldEvent`](crate::continuum::WorldEvent) was emitted.
///
/// Carried inside entity events so that observers can build S→C packets
/// without an additional RPC round-trip to the worker.
///
/// `raw` contains the pre-serialized S→C packet (e.g. DrawMobile 0x78,
/// ObjectInfo 0x1A) ready to be sent to the client as-is.  The scalar
/// fields (`graphic`, `hue`, etc.) provide quick access to common
/// properties needed for lightweight packets like UpdateMobile (0x77).
#[derive(Debug, Clone)]
pub struct EntitySnapshot {
    /// Primary graphic id (body for mobiles, item graphic otherwise).
    pub graphic: u16,
    /// Hue / colour.
    pub hue: u16,
    /// Status flags byte (`MobileFlags` value for mobiles, 0 otherwise).
    pub status_flags: u8,
    /// Notoriety value (1=innocent .. 7=translucent, 0 for non-mobiles).
    pub notoriety: u8,
    /// Pre-serialized S→C packet representation of the entity.
    ///
    /// For mobiles this is a DrawMobile (0x78) packet with full equipment.
    /// For items/multis this is the original raw packet (ObjectInfo 0x1A
    /// or similar).  `Bytes` is reference-counted, so cloning is cheap.
    pub raw: Bytes,
    /// Per-viewer reputation data, used by the session render path to colour
    /// mobiles relative to each observer (see [`NotorietyContext`]).
    ///
    /// `None` for non-mobiles or entities without reputation data.
    pub notoriety_ctx: Option<NotorietyContext>,
}

/// Reputation data carried alongside a mobile [`EntitySnapshot`] so that the
/// per-session world-event render path can compute a per-viewer notoriety
/// colour without an RPC round-trip to the worker.
///
/// The `class` field is an opaque code defined by the game layer (the
/// `common` crate's `NotorietyClass`); the framework only stores and forwards
/// it.  Guild support is minimal: two mobiles sharing `guild_id` are allies.
#[derive(Debug, Clone, Default)]
pub struct NotorietyContext {
    /// Opaque intrinsic-class code (game-layer defined).
    pub class: u8,
    /// Guild id; same id between two players ⇒ allies.
    pub guild_id: Option<u32>,
    /// Whether this mobile is a player character.
    pub is_player: bool,
    /// Active aggressor relationships: `(other_serial, expiry_epoch_ms)`.
    /// A viewer present here may attack this mobile freely (shown gray).
    pub aggressors: Vec<(u32, u64)>,
}

/// Base trait for a world entity.
///
/// Implementations must be `Clone + Send + Sync + 'static` so that they
/// can be stored in shared registries and sent across threads.
pub trait Entity: Clone + Send + Sync + 'static {
    /// Unique serial number.
    fn serial(&self) -> u32;

    /// World position `(x, y, z)`.
    fn pos(&self) -> Pos3D;

    /// Primary graphic id (item/multi graphic, body id for mobiles).
    fn graphic(&self) -> u16;

    /// Whether this entity is a mobile (player / NPC / creature).
    fn is_mobile(&self) -> bool;

    /// Whether this entity is a multi-object (house, boat, castle, etc.).
    fn is_multi(&self) -> bool;

    /// Whether this entity is a container (observed via `0x24 DrawContainer`).
    ///
    /// Default implementation returns `false`.
    fn is_container(&self) -> bool { false }

    /// Update world position.
    fn set_pos(&mut self, pos: Pos3D);

    /// Update facing direction (meaningful only for mobiles; default is no-op).
    fn set_direction(&mut self, _direction: u8) {}

    /// Create an [`EntitySnapshot`] capturing the current visual state.
    ///
    /// Implementations should return a snapshot with the pre-serialized
    /// S→C packet (DrawMobile 0x78, ObjectInfo 0x1A, etc.) and key
    /// scalar fields.  The default returns `None`.
    fn snapshot(&self) -> Option<EntitySnapshot> { None }

    /// Extract collision shapes for passability checks.
    ///
    /// Returns a list of `(tile_x, tile_y, TileShape)` tuples.
    /// - Mobiles: empty (they don't block movement).
    /// - Items: single tile shape from `static_tile_def`.
    /// - Multis: one shape per multi part from `multi_parts`.
    fn extract_shapes(
        &self,
        static_data: &(impl StaticDataProvider + ?Sized),
    ) -> Vec<(u16, u16, TileShape)>;

    // ── Combat stats (optional) ────────────────────────────────────────

    /// Get current hits, max hits. Returns `None` for non-mobiles.
    fn hits(&self) -> Option<(u16, u16)> { None }

    /// Apply damage. Returns new HP. Default: no-op.
    fn apply_damage(&mut self, _amount: u16) -> u16 { 0 }

    /// Apply healing. Returns new HP. Default: no-op.
    fn apply_heal(&mut self, _amount: u16) -> u16 { 0 }

    /// Modify mana by delta. Returns new mana. Default: no-op.
    fn modify_mana(&mut self, _delta: i32) -> u16 { 0 }

    /// Get current mana, max mana. Returns `None` for non-mobiles.
    fn mana(&self) -> Option<(u16, u16)> { None }

    /// Modify stamina by delta. Returns new stamina. Default: no-op.
    fn modify_stamina(&mut self, _delta: i32) -> u16 { 0 }

    /// Get current stamina, max stamina. Returns `None` for non-mobiles.
    fn stamina(&self) -> Option<(u16, u16)> { None }

    /// Modify strength by delta (clamped to 1..max). Returns new str. Default: no-op.
    fn modify_str(&mut self, _delta: i32) -> u16 { 0 }

    /// Get current str. Returns `None` for non-mobiles.
    fn str_val(&self) -> Option<u16> { None }

    /// Modify dexterity by delta (clamped to 1..max). Returns new dex. Default: no-op.
    fn modify_dex(&mut self, _delta: i32) -> u16 { 0 }

    /// Get current dex. Returns `None` for non-mobiles.
    fn dex_val(&self) -> Option<u16> { None }

    /// Modify intelligence by delta (clamped to 1..max). Returns new int. Default: no-op.
    fn modify_int(&mut self, _delta: i32) -> u16 { 0 }

    /// Get current int. Returns `None` for non-mobiles.
    fn int_val(&self) -> Option<u16> { None }

    /// Notoriety value (1=innocent .. 7=translucent). Returns `None` for non-mobiles.
    fn notoriety(&self) -> Option<u8> { None }

    /// Entity name. Returns `None` for non-mobiles / unnamed entities.
    fn name(&self) -> Option<String> { None }

    /// Facing direction (raw wire byte). Returns `None` for non-mobiles.
    fn direction(&self) -> Option<u8> { None }

    // ── Equipment / container helpers (for cross-zone transfer) ─────

    /// Return the serial of the entity's backpack container (if any).
    ///
    /// For mobiles this is typically the equipment item on the
    /// `Backpack` layer.  Default returns `None`.
    fn backpack_serial(&self) -> Option<u32> { None }

    /// Return serials of all equipped items on this entity.
    ///
    /// For mobiles this is the list of equipment layer item serials.
    /// Default returns an empty vec.
    fn equipment_serials(&self) -> Vec<u32> { Vec::new() }

    /// Whether this mobile is currently mounted.
    ///
    /// For game-specific entities, this typically checks whether an
    /// equipment item on the Mount layer is present.
    /// Default returns `false`.
    fn is_mounted(&self) -> bool { false }

    /// Whether this mobile is a player character.
    ///
    /// Used to distinguish player-controlled mobiles (which have a session)
    /// from NPCs / pets.  Default returns `false`.
    fn is_player(&self) -> bool { false }
}
