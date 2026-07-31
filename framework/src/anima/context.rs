use std::any::Any;

use u_core::{Facing, Heading, Pos3D};

use crate::ecumene::tile_rect::TileRect;

use super::error::ControllerError;
use super::scheduler::Scheduler;
use super::traits::AccessLevel;

/// Information about an entity, visible through the context.
///
/// Type-erased wrapper — the controller does not know the concrete `E: Entity`.
#[derive(Debug, Clone)]
pub struct EntityInfo {
    pub serial: u32,
    pub pos: Pos3D,
    pub graphic: u16,
    pub is_mobile: bool,
    pub is_multi: bool,
    /// Combat stats — populated only for mobiles.
    pub hits: Option<u16>,
    pub hits_max: Option<u16>,
    pub mana: Option<u16>,
    pub mana_max: Option<u16>,
    pub stamina: Option<u16>,
    pub stamina_max: Option<u16>,
    /// Notoriety byte (0-7).
    pub notoriety: Option<u8>,
    /// Mobile name (empty for non-mobiles).
    pub name: Option<String>,
    /// Facing direction (raw wire byte).
    pub direction: Option<u8>,
    /// Whether the mobile is currently mounted.
    pub is_mounted: bool,
    /// Whether this mobile is a player character (has a session).
    pub is_player: bool,
}

/// Trait that erases the zone's generic types.
///
/// Implemented by an adapter (`ZoneAdapter`) for `Zone<E, C>` in `host.rs`.
/// Allows `ControlContext` to work without generic parameters.
pub trait ZoneAccess {
    /// Get entity info by serial.
    fn get_entity(&self, serial: u32) -> Option<EntityInfo>;

    /// Query all entities in a rectangular area.
    fn query_area(&self, area: &TileRect) -> Vec<EntityInfo>;

    /// Test if a step from (x, y, z) in the given direction is passable.
    /// Returns new Z on success.
    fn test_step(&self, x: u16, y: u16, z: i8, direction: Heading) -> Option<i8>;

    /// Resolve standing Z for a position.
    fn resolve_standing_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8>;

    /// Teleport entity to new position (no validation).
    /// Returns error if entity not found.
    fn teleport_entity(&mut self, serial: u32, x: u16, y: u16, z: i8)
        -> Result<(), ControllerError>;

    /// Move entity one step in direction (with passability validation).
    ///
    /// Checks `test_step`, updates position and direction.
    /// The `Facing` carries both the compass heading (bits 0–2) and the
    /// running flag (bit 7) so callers can distinguish walk from run.
    /// Returns new position on success.
    fn move_entity(&mut self, serial: u32, direction: Facing)
        -> Result<Pos3D, ControllerError>;

    /// Change an entity's facing direction without moving it.
    ///
    /// Updates the stored direction byte (heading bits 0–2; the running
    /// flag is irrelevant for a stationary turn) and broadcasts an
    /// `EntityMoved` event (as a teleport-style update) so observers
    /// re-render the new facing.  Returns error if the entity is not found.
    fn set_direction(&mut self, serial: u32, direction: Facing)
        -> Result<(), ControllerError>;

    /// Get map_id of the current zone.
    fn map_id(&self) -> u8;

    // ── Broadcast events (sound, effects, animation, speech) ───────────

    /// Play a sound effect at world coordinates.
    fn play_sound(&self, sound_id: u16, x: u16, y: u16, z: i16);

    /// Play a graphical effect (projectile, lightning, stationary).
    #[allow(clippy::too_many_arguments)]
    fn play_effect(
        &self,
        direction_type: u8,
        source_serial: u32,
        target_serial: u32,
        graphic: u16,
        x: u16,
        y: u16,
        z: i8,
        target_x: u16,
        target_y: u16,
        target_z: i8,
        speed: u8,
        duration: u8,
        fixed_direction: bool,
        explode: bool,
    );

    /// Play character animation.
    #[allow(clippy::too_many_arguments)]
    fn animate(
        &self,
        serial: u32,
        action: u16,
        frame_count: u8,
        repeat_count: u16,
        reverse: bool,
        repeat: bool,
        frame_delay: u8,
        x: u16,
        y: u16,
    );

    /// Say speech on behalf of an entity.
    #[allow(clippy::too_many_arguments)]
    fn say(
        &self,
        serial: u32,
        graphic: u16,
        speech_type: u8,
        color: u16,
        font: u16,
        name: String,
        message: String,
        x: u16,
        y: u16,
    );

    /// Deal damage to entity. Returns (new_hits, killed).
    /// If HP drops to 0, killed = true.
    /// `source_serial` — attacker serial (0 if unknown).
    fn deal_damage(&mut self, serial: u32, amount: u16, source_serial: u32) -> Result<(u16, bool), ControllerError>;

    /// Heal entity. Returns new HP.
    fn heal_entity(&mut self, serial: u32, amount: u16) -> Result<u16, ControllerError>;

    /// Modify entity's mana (delta may be negative).
    /// Returns new mana value.
    fn modify_mana(&mut self, serial: u32, delta: i32) -> Result<u16, ControllerError>;

    /// Modify entity's stamina.
    fn modify_stamina(&mut self, serial: u32, delta: i32) -> Result<u16, ControllerError>;

    /// Check line of sight between two 3D points.
    ///
    /// `z1` / `z2` — full Z coordinates (accounting for eye height, usually
    /// standing_z + 14 for humanoids).
    /// Returns `true` if the ray is not blocked.
    fn has_los(&self, x1: u16, y1: u16, z1: i16, x2: u16, y2: u16, z2: i16) -> bool;

    // ── Targeted events (to a specific player session) ────────────────

    /// Send a gump dialog to a specific player.
    ///
    /// `source_serial` is the serial of the object opening the gump
    /// (used by the session to route gump responses back).
    ///
    /// When `blocking` is `true`, the session marks this gump as blocking —
    /// the player cannot cast spells or use skills until the gump is
    /// closed or answered.  Bandages remain allowed.
    #[allow(clippy::too_many_arguments)]
    fn send_targeted_gump(
        &self,
        target_player: u32,
        source_serial: u32,
        gump_id: u32,
        gump_x: u32,
        gump_y: u32,
        layout: String,
        text_lines: Vec<String>,
        blocking: bool,
    );

    /// Send a system message to a specific player.
    fn send_targeted_message(
        &self,
        target_player: u32,
        message: String,
        color: u16,
    );

    /// Close a gump for a specific player.
    fn close_targeted_gump(
        &self,
        target_player: u32,
        gump_id: u32,
    );

    // ── Inventory / equipment access ──────────────────────────────────

    /// Get the backpack serial for a mobile entity.
    ///
    /// Returns the serial of the item equipped in the Backpack layer,
    /// or `None` if the entity is not a mobile or has no backpack.
    fn get_backpack_serial(&self, serial: u32) -> Option<u32> {
        let _ = serial;
        None
    }

    /// Find an item with the given graphic inside a container.
    ///
    /// Returns `(item_serial, amount)` of the first matching item,
    /// or `None` if no item with that graphic exists in the container.
    fn find_item_in_container(&self, container_serial: u32, graphic: u16) -> Option<(u32, u16)> {
        let _ = (container_serial, graphic);
        None
    }

    /// Atomically check and consume mana from a mobile.
    ///
    /// Returns the new mana value if the mobile had enough mana,
    /// `None` if insufficient mana or entity not found.
    fn consume_mana(&mut self, serial: u32, amount: u16) -> Option<u16> {
        let _ = (serial, amount);
        None
    }

    /// Consume `amount` units from a stacked item (in a container or on ground).
    ///
    /// If `expected_graphic` is `Some`, the operation fails if the item's
    /// graphic doesn't match.  Returns `Some((remaining, graphic))` on
    /// success — `remaining == 0` means the item was fully removed.
    /// Returns `None` if item not found or graphic mismatch.
    fn consume_item(
        &mut self,
        item_serial: u32,
        amount: u16,
        expected_graphic: Option<u16>,
    ) -> Option<(u16, u16)> {
        let _ = (item_serial, amount, expected_graphic);
        None
    }

    // ── Item properties (type-erased) ─────────────────────────────────

    /// Get per-item properties by serial, returned as a type-erased `Box<dyn Any>`.
    ///
    /// The concrete type inside the `Box` is `P::Value` from the zone's
    /// item property store.  Callers must downcast to the expected type.
    fn get_item_props_any(&self, serial: u32) -> Option<Box<dyn Any>> {
        let _ = serial;
        None
    }

    /// Set (or remove) per-item properties.
    ///
    /// Pass `Some(box)` to insert/replace, or `None` to remove.
    /// The `Box<dyn Any>` must contain a value of type `P::Value`.
    fn set_item_props_any(&mut self, serial: u32, props: Option<Box<dyn Any>>) {
        let _ = (serial, props);
    }

    /// Send a target cursor to a specific player.
    ///
    /// The player's session translates this into the S→C packet 0x6C.
    /// The cursor response comes back via `GameCommand::TargetResponse`.
    fn send_target_cursor(
        &self,
        target_player: u32,
        cursor_id: u32,
        cursor_type: u8,
    ) {
        let _ = (target_player, cursor_id, cursor_type);
    }

    /// Teleport a specific player to another world (map facet).
    ///
    /// Controllers are bound to a single zone and cannot perform a
    /// worker-level cross-map transfer, so this hands the move off to the
    /// target player's session (via `WorldEvent::TargetedCrossWorldTeleport`),
    /// which executes the atomic transfer.
    fn send_cross_world_teleport(
        &self,
        target_player: u32,
        map_id: u8,
        x: u16,
        y: u16,
        z: i8,
    ) {
        let _ = (target_player, map_id, x, y, z);
    }

    // ── World event subscription ──────────────────────────────────────

    /// Subscribe the controlled entity to world events within a
    /// Chebyshev radius around its current position.
    ///
    /// Events are delivered via [`EntityController::on_world_event`](super::EntityController::on_world_event) on
    /// each tick.  The subscription automatically follows the entity as
    /// it moves.
    ///
    /// Only one subscription per entity is supported; calling this again
    /// replaces the previous radius.
    fn subscribe_world_events(&mut self, entity_serial: u32, radius: u16) {
        let _ = (entity_serial, radius);
    }

    /// Remove the world-event subscription for the entity.
    fn unsubscribe_world_events(&mut self, entity_serial: u32) {
        let _ = entity_serial;
    }

    /// Remove an entity from the zone.
    ///
    /// Returns `Ok(())` on success or an error if the entity was not found.
    /// Emits [`WorldEvent::EntityRemoved`](crate::continuum::WorldEvent::EntityRemoved) when an event sender is present.
    fn remove_entity(&mut self, serial: u32) -> Result<(), ControllerError> {
        let _ = serial;
        Err(ControllerError::AccessDenied {
            action: "remove_entity",
            level: AccessLevel::ReadOnly,
        })
    }
}

/// Context passed to the controller on every call.
///
/// Provides access to the zone, scheduler, and info about the
/// controlled entity. One universal context for all access levels
/// (with future separation in mind).
pub struct ControlContext<'a> {
    /// Serial of the controlled entity.
    pub entity_serial: u32,

    /// Controller access level (informational for now, not enforced).
    pub access_level: AccessLevel,

    /// Access to the zone (type-erased).
    zone: &'a mut dyn ZoneAccess,

    /// Task scheduler.
    pub scheduler: &'a mut Scheduler,
}

impl<'a> ControlContext<'a> {
    /// Create new context. Called from `ControllerHost`.
    pub(crate) fn new(
        entity_serial: u32,
        access_level: AccessLevel,
        zone: &'a mut dyn ZoneAccess,
        scheduler: &'a mut Scheduler,
    ) -> Self {
        Self {
            entity_serial,
            access_level,
            zone,
            scheduler,
        }
    }

    // ── Read (available at all levels) ──────────────────────────

    /// Get info about the controlled entity.
    pub fn me(&self) -> Option<EntityInfo> {
        self.zone.get_entity(self.entity_serial)
    }

    /// Get info about any entity.
    pub fn get_entity(&self, serial: u32) -> Option<EntityInfo> {
        self.zone.get_entity(serial)
    }

    /// Query all entities in the area.
    pub fn query_area(&self, area: &TileRect) -> Vec<EntityInfo> {
        self.zone.query_area(area)
    }

    /// Test step passability.
    pub fn test_step(&self, x: u16, y: u16, z: i8, direction: Heading) -> Option<i8> {
        self.zone.test_step(x, y, z, direction)
    }

    /// Resolve standing Z position.
    pub fn resolve_standing_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8> {
        self.zone.resolve_standing_z(x, y, z_hint, direction)
    }

    /// Map ID of current zone.
    pub fn map_id(&self) -> u8 {
        self.zone.map_id()
    }

    /// Check line of sight between two 3D points.
    ///
    /// Usually called with `z + 14` (humanoid eye height).
    pub fn has_los(&self, x1: u16, y1: u16, z1: i16, x2: u16, y2: u16, z2: i16) -> bool {
        self.zone.has_los(x1, y1, z1, x2, y2, z2)
    }

    // ── Mutation (Safe: with validation, Full: direct) ────────────────

    /// Move the controlled entity one step in the given direction.
    ///
    /// `direction` is a `Facing` byte: bits 0–2 are the compass heading
    /// (0=N … 7=NW) and bit 7 is the running flag (set = running).
    ///
    /// In Safe mode — checks passability via `test_step`.
    /// In Full mode — also checks (single context for now).
    /// In ReadOnly — returns `AccessDenied`.
    pub fn step(&mut self, direction: Facing) -> Result<Pos3D, ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "step",
                level: self.access_level,
            });
        }

        self.zone.move_entity(self.entity_serial, direction)
    }

    /// Teleport the controlled entity (no passability validation).
    ///
    /// Requires Safe or Full access level.
    pub fn teleport(&mut self, x: u16, y: u16, z: i8) -> Result<(), ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "teleport",
                level: self.access_level,
            });
        }

        self.zone.teleport_entity(self.entity_serial, x, y, z)
    }

    /// Turn the controlled entity to face the given direction without moving.
    ///
    /// In Safe/Full mode — updates the facing and broadcasts the change.
    /// In ReadOnly — returns `AccessDenied`.
    pub fn set_direction(&mut self, direction: Facing) -> Result<(), ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "set_direction",
                level: self.access_level,
            });
        }
        self.zone.set_direction(self.entity_serial, direction)
    }

    // ── Broadcast events ─────────────────────────────────────────────

    /// Play sound effect at world coordinates.
    pub fn play_sound(&self, sound_id: u16, x: u16, y: u16, z: i16) {
        self.zone.play_sound(sound_id, x, y, z);
    }

    /// Play graphical effect.
    #[allow(clippy::too_many_arguments)]
    pub fn play_effect(
        &self,
        direction_type: u8,
        source_serial: u32,
        target_serial: u32,
        graphic: u16,
        x: u16,
        y: u16,
        z: i8,
        target_x: u16,
        target_y: u16,
        target_z: i8,
        speed: u8,
        duration: u8,
        fixed_direction: bool,
        explode: bool,
    ) {
        self.zone.play_effect(
            direction_type, source_serial, target_serial, graphic,
            x, y, z, target_x, target_y, target_z,
            speed, duration, fixed_direction, explode,
        );
    }

    /// Play character animation.
    #[allow(clippy::too_many_arguments)]
    pub fn animate(
        &self,
        serial: u32,
        action: u16,
        frame_count: u8,
        repeat_count: u16,
        reverse: bool,
        repeat: bool,
        frame_delay: u8,
        x: u16,
        y: u16,
    ) {
        self.zone.animate(serial, action, frame_count, repeat_count, reverse, repeat, frame_delay, x, y);
    }

    /// Say speech on behalf of entity.
    #[allow(clippy::too_many_arguments)]
    pub fn say(
        &self,
        serial: u32,
        graphic: u16,
        speech_type: u8,
        color: u16,
        font: u16,
        name: String,
        message: String,
        x: u16,
        y: u16,
    ) {
        self.zone.say(serial, graphic, speech_type, color, font, name, message, x, y);
    }

    /// Teleport arbitrary entity. Requires `Full` access level.
    pub fn teleport_other(
        &mut self,
        serial: u32,
        x: u16,
        y: u16,
        z: i8,
    ) -> Result<(), ControllerError> {
        if self.access_level != AccessLevel::Full {
            return Err(ControllerError::AccessDenied {
                action: "teleport_other",
                level: self.access_level,
            });
        }

        self.zone.teleport_entity(serial, x, y, z)
    }

    // ── Combat actions (Safe+) ────────────────────────────────────────

    /// Deal damage to target entity. Returns (new_hits, killed).
    pub fn deal_damage(&mut self, serial: u32, amount: u16) -> Result<(u16, bool), ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "deal_damage",
                level: self.access_level,
            });
        }
        self.zone.deal_damage(serial, amount, self.entity_serial)
    }

    /// Heal entity. Returns new HP.
    pub fn heal_entity(&mut self, serial: u32, amount: u16) -> Result<u16, ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "heal_entity",
                level: self.access_level,
            });
        }
        self.zone.heal_entity(serial, amount)
    }

    /// Modify entity's mana (delta may be negative).
    pub fn modify_mana(&mut self, serial: u32, delta: i32) -> Result<u16, ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "modify_mana",
                level: self.access_level,
            });
        }
        self.zone.modify_mana(serial, delta)
    }

    /// Modify entity's stamina.
    pub fn modify_stamina(&mut self, serial: u32, delta: i32) -> Result<u16, ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "modify_stamina",
                level: self.access_level,
            });
        }
        self.zone.modify_stamina(serial, delta)
    }

    // ── Targeted events (to a specific player session) ────────────────

    /// Send a gump dialog to a specific player.
    ///
    /// The `source_serial` is automatically set to this controller's
    /// entity serial, so the session can route gump responses back.
    ///
    /// When `blocking` is `true`, the session marks this gump as blocking —
    /// the player cannot cast spells or use skills until the gump is
    /// closed or answered.  Bandages remain allowed.
    #[allow(clippy::too_many_arguments)]
    pub fn send_gump(
        &self,
        target_player: u32,
        gump_id: u32,
        gump_x: u32,
        gump_y: u32,
        layout: String,
        text_lines: Vec<String>,
        blocking: bool,
    ) {
        self.zone.send_targeted_gump(
            target_player,
            self.entity_serial,
            gump_id,
            gump_x,
            gump_y,
            layout,
            text_lines,
            blocking,
        );
    }

    /// Send a system message to a specific player.
    pub fn send_message(&self, target_player: u32, message: &str, color: u16) {
        self.zone.send_targeted_message(target_player, message.to_string(), color);
    }

    /// Close a gump for a specific player.
    pub fn close_gump(&self, target_player: u32, gump_id: u32) {
        self.zone.close_targeted_gump(target_player, gump_id);
    }

    // ── Inventory / equipment access ──────────────────────────────────

    /// Get the backpack serial for a mobile entity.
    pub fn get_backpack_serial(&self, serial: u32) -> Option<u32> {
        self.zone.get_backpack_serial(serial)
    }

    /// Find an item with the given graphic inside a container.
    ///
    /// Returns `(item_serial, amount)` or `None`.
    pub fn find_item_in_container(&self, container_serial: u32, graphic: u16) -> Option<(u32, u16)> {
        self.zone.find_item_in_container(container_serial, graphic)
    }

    /// Atomically check and consume mana. Returns new mana or `None`.
    ///
    /// Requires Safe or Full access level.
    pub fn consume_mana(&mut self, serial: u32, amount: u16) -> Result<Option<u16>, ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "consume_mana",
                level: self.access_level,
            });
        }
        Ok(self.zone.consume_mana(serial, amount))
    }

    /// Consume `amount` units from a stacked item.
    ///
    /// Returns `Some((remaining, graphic))` on success.
    /// Requires Safe or Full access level.
    pub fn consume_item(
        &mut self,
        item_serial: u32,
        amount: u16,
        expected_graphic: Option<u16>,
    ) -> Result<Option<(u16, u16)>, ControllerError> {
        if self.access_level == AccessLevel::ReadOnly {
            return Err(ControllerError::AccessDenied {
                action: "consume_item",
                level: self.access_level,
            });
        }
        Ok(self.zone.consume_item(item_serial, amount, expected_graphic))
    }

    /// Send a target cursor to a specific player.
    pub fn send_target_cursor(
        &self,
        target_player: u32,
        cursor_id: u32,
        cursor_type: u8,
    ) {
        self.zone.send_target_cursor(target_player, cursor_id, cursor_type);
    }

    /// Teleport a specific player to another world (map facet).
    ///
    /// Used by cross-world teleporter controllers when the destination map
    /// differs from the controller's zone.  Requires `Full` access level.
    pub fn send_cross_world_teleport(
        &self,
        target_player: u32,
        map_id: u8,
        x: u16,
        y: u16,
        z: i8,
    ) -> Result<(), ControllerError> {
        if self.access_level != AccessLevel::Full {
            return Err(ControllerError::AccessDenied {
                action: "send_cross_world_teleport",
                level: self.access_level,
            });
        }
        self.zone
            .send_cross_world_teleport(target_player, map_id, x, y, z);
        Ok(())
    }

    // ── World event subscription ──────────────────────────────────────

    /// Subscribe this entity to world events within a Chebyshev radius.
    ///
    /// Events are delivered to
    /// [`EntityController::on_world_event`](super::EntityController::on_world_event)
    /// on each tick.  The subscription automatically follows the entity
    /// as it moves.  Requires Safe or Full access level.
    pub fn subscribe_world_events(&mut self, radius: u16) {
        self.zone.subscribe_world_events(self.entity_serial, radius);
    }

    /// Remove the world-event subscription for this entity.
    pub fn unsubscribe_world_events(&mut self) {
        self.zone.unsubscribe_world_events(self.entity_serial);
    }

    /// Remove an entity from the zone.
    ///
    /// Requires `Full` access level.  Cannot remove the controlled entity
    /// itself (use a different mechanism for self-destruction).
    pub fn remove_entity(&mut self, serial: u32) -> Result<(), ControllerError> {
        if self.access_level != AccessLevel::Full {
            return Err(ControllerError::AccessDenied {
                action: "remove_entity",
                level: self.access_level,
            });
        }
        self.zone.remove_entity(serial)
    }

    // ── Item properties (type-erased) ─────────────────────────────────

    /// Get per-item properties by serial.
    ///
    /// The returned `Box<dyn Any>` contains the zone's `P::Value` type.
    /// Callers must downcast to the concrete type.
    pub fn get_item_props_any(&self, serial: u32) -> Option<Box<dyn Any>> {
        self.zone.get_item_props_any(serial)
    }

    /// Set (or remove) per-item properties.
    ///
    /// Pass `Some(box)` containing the zone's `P::Value` type to
    /// insert/replace, or `None` to remove.
    pub fn set_item_props_any(&mut self, serial: u32, props: Option<Box<dyn Any>>) {
        self.zone.set_item_props_any(serial, props);
    }
}


