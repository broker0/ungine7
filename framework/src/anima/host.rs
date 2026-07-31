use std::collections::HashMap;
use std::marker::PhantomData;
use std::time::Duration;

use tokio::time::Instant;

use log::warn;
use u_core::{Facing, Heading, MobilePos, Pos3D};

use crate::continuum::container::ZoneContainers;
use crate::continuum::item_props::ZoneItemProps;
use crate::continuum::observer::ObserverRegistry;
use crate::continuum::world_event::WorldEvent;
use crate::continuum::zone::Zone;
use crate::ecumene::tile_rect::TileRect;
use crate::vessel::objects::Entity;

use super::context::{ControlContext, EntityInfo, ZoneAccess};
use super::error::ControllerError;
use super::scheduler::{Scheduler, TaskAction};
use super::traits::{ControllerDef, EntityController};

/// Adapter that implements [`ZoneAccess`] for a specific `Zone<E, C>`.
///
/// Erases the zone's generic types, allowing `ControlContext` to
/// operate without generic parameters.
///
/// Optionally holds an `mpsc::UnboundedSender<WorldEvent>` so that
/// position-changing operations (`move_entity`, `teleport_entity`)
/// automatically publish world events.
///
/// Optionally holds a reference to [`ObserverRegistry`] so that
/// controllers can subscribe/unsubscribe to world events and the
/// subscription rect is auto-updated on movement.
struct ZoneAdapter<'a, E: Entity, C: ZoneContainers, P: ZoneItemProps> {
    zone: &'a mut Zone<E, C, P>,
    event_tx: Option<&'a tokio::sync::mpsc::UnboundedSender<WorldEvent>>,
    observer_registry: Option<&'a mut ObserverRegistry>,
}

impl<E: Entity, C: ZoneContainers, P: ZoneItemProps> ZoneAccess for ZoneAdapter<'_, E, C, P>
where
    P::Value: 'static,
{
    fn get_entity(&self, serial: u32) -> Option<EntityInfo> {
        self.zone.get(serial).map(|e| {
            let (hits, hits_max) = e.hits().map(|(h, m)| (Some(h), Some(m))).unwrap_or((None, None));
            let (mana, mana_max) = e.mana().map(|(m, mx)| (Some(m), Some(mx))).unwrap_or((None, None));
            let (stamina, stamina_max) = e.stamina().map(|(s, sx)| (Some(s), Some(sx))).unwrap_or((None, None));
            EntityInfo {
                serial: e.serial(),
                pos: e.pos(),
                graphic: e.graphic(),
                is_mobile: e.is_mobile(),
                is_multi: e.is_multi(),
                hits,
                hits_max,
                mana,
                mana_max,
                stamina,
                stamina_max,
                notoriety: e.notoriety(),
                name: e.name(),
                direction: e.direction(),
                is_mounted: e.is_mounted(),
                is_player: e.is_player(),
            }
        })
    }

    fn query_area(&self, area: &TileRect) -> Vec<EntityInfo> {
        self.zone.query_area(area)
            .into_iter()
            .map(|e| {
                let (hits, hits_max) = e.hits().map(|(h, m)| (Some(h), Some(m))).unwrap_or((None, None));
                let (mana, mana_max) = e.mana().map(|(m, mx)| (Some(m), Some(mx))).unwrap_or((None, None));
                let (stamina, stamina_max) = e.stamina().map(|(s, sx)| (Some(s), Some(sx))).unwrap_or((None, None));
                EntityInfo {
                    serial: e.serial(),
                    pos: e.pos(),
                    graphic: e.graphic(),
                    is_mobile: e.is_mobile(),
                    is_multi: e.is_multi(),
                    hits,
                    hits_max,
                    mana,
                    mana_max,
                    stamina,
                    stamina_max,
                    notoriety: e.notoriety(),
                    name: e.name(),
                    direction: e.direction(),
                    is_mounted: e.is_mounted(),
                    is_player: e.is_player(),
                }
            })
            .collect()
    }

    fn test_step(&self, x: u16, y: u16, z: i8, direction: Heading) -> Option<i8> {
        self.zone.test_step(x, y, z, direction)
    }

    fn resolve_standing_z(&self, x: u16, y: u16, z_hint: i8, direction: Heading) -> Option<i8> {
        self.zone.resolve_standing_z(x, y, z_hint, direction)
    }

    fn teleport_entity(
        &mut self,
        serial: u32,
        x: u16,
        y: u16,
        z: i8,
    ) -> Result<(), ControllerError> {
        let (old_pos, old_direction) = self.zone.get(serial)
            .map(|e| (e.pos(), e.direction().unwrap_or(0)))
            .ok_or(ControllerError::EntityNotFound(serial))?;

        let mut entity = self.zone.remove(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;

        let new_pos = Pos3D::new(x, y, z);
        entity.set_pos(new_pos);
        self.zone.spawn(serial, entity);

        if let Some(tx) = &self.event_tx {
            let facing = Facing::new(old_direction);
            let snap = self.zone.get(serial).and_then(|e| e.snapshot());
            let _ = tx.send(WorldEvent::EntityMoved {
                map_id: self.zone.map_id,
                serial,
                old_pos: MobilePos::new(old_pos.x, old_pos.y, old_pos.z, facing),
                new_pos: MobilePos::new(x, y, z, facing),
                entity: snap,
                is_teleport: true,
            });
        }

        // Auto-update controller subscription rect if entity is subscribed.
        if let Some(reg) = &mut self.observer_registry {
            if let Some(radius) = reg.get_controller_subscription_radius(serial) {
                let new_rect = TileRect::from_view(x, y, radius);
                reg.update_controller_watch(serial, new_rect);
            }
        }

        Ok(())
    }

    fn move_entity(
        &mut self,
        serial: u32,
        direction: Facing,
    ) -> Result<Pos3D, ControllerError> {
        let info = self.zone.get(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;

        let cur_pos = info.pos();
        let heading = direction.heading();
        let (dx, dy) = heading.delta();
        let new_x = (cur_pos.x as i32 + dx) as u16;
        let new_y = (cur_pos.y as i32 + dy) as u16;

        let new_z = self.zone.test_step(cur_pos.x, cur_pos.y, cur_pos.z, heading)
            .ok_or(ControllerError::MovementBlocked {
                serial,
                x: new_x,
                y: new_y,
            })?;

        let new_pos = Pos3D::new(new_x, new_y, new_z);

        // Preserve the running flag (bit 7) in the stored direction byte so
        // clients receive the correct MoveAck and play the running animation.
        if info.is_mobile() {
            self.zone.move_entity(serial, new_x, new_y, new_z, Some(direction.raw()));
        } else {
            let mut entity = self.zone.remove(serial)
                .ok_or(ControllerError::EntityNotFound(serial))?;
            entity.set_pos(new_pos);
            self.zone.spawn(serial, entity);
        }

        if let Some(tx) = &self.event_tx {
            let snap = self.zone.get(serial).and_then(|e| e.snapshot());
            let _ = tx.send(WorldEvent::EntityMoved {
                map_id: self.zone.map_id,
                serial,
                old_pos: MobilePos::new(cur_pos.x, cur_pos.y, cur_pos.z, direction),
                new_pos: MobilePos::new(new_x, new_y, new_z, direction),
                entity: snap,
                is_teleport: false,
            });
        }

        // Auto-update controller subscription rect if entity is subscribed.
        if let Some(reg) = &mut self.observer_registry {
            if let Some(radius) = reg.get_controller_subscription_radius(serial) {
                let new_rect = TileRect::from_view(new_x, new_y, radius);
                reg.update_controller_watch(serial, new_rect);
            }
        }

        Ok(new_pos)
    }

    fn set_direction(&mut self, serial: u32, direction: Facing) -> Result<(), ControllerError> {
        let cur_pos = self.zone.get(serial)
            .map(|e| e.pos())
            .ok_or(ControllerError::EntityNotFound(serial))?;

        // Re-write the same position but with the new facing byte.
        self.zone.move_entity(serial, cur_pos.x, cur_pos.y, cur_pos.z, Some(direction.raw()));

        if let Some(tx) = &self.event_tx {
            let snap = self.zone.get(serial).and_then(|e| e.snapshot());
            let mpos = MobilePos::new(cur_pos.x, cur_pos.y, cur_pos.z, direction);
            let _ = tx.send(WorldEvent::EntityMoved {
                map_id: self.zone.map_id,
                serial,
                old_pos: mpos,
                new_pos: mpos,
                entity: snap,
                is_teleport: true,
            });
        }

        Ok(())
    }

    fn map_id(&self) -> u8 {
        self.zone.map_id
    }

    fn has_los(&self, x1: u16, y1: u16, z1: i16, x2: u16, y2: u16, z2: i16) -> bool {
        self.zone.has_los(x1, y1, z1, x2, y2, z2)
    }

    fn play_sound(&self, sound_id: u16, x: u16, y: u16, z: i16) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::SoundPlayed {
                map_id: self.zone.map_id,
                sound_id,
                x,
                y,
                z,
            });
        }
    }

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
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::EffectPlayed {
                map_id: self.zone.map_id,
                direction_type,
                source_serial,
                target_serial,
                graphic,
                x,
                y,
                z,
                target_x,
                target_y,
                target_z,
                speed,
                duration,
                fixed_direction,
                explode,
            });
        }
    }

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
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::AnimationPlayed {
                map_id: self.zone.map_id,
                serial,
                action,
                frame_count,
                repeat_count,
                reverse,
                repeat,
                frame_delay,
                x,
                y,
            });
        }
    }

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
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::Speech {
                map_id: self.zone.map_id,
                serial,
                graphic,
                speech_type,
                color,
                font,
                name,
                message,
                x,
                y,
            });
        }
    }

    fn deal_damage(&mut self, serial: u32, amount: u16, source_serial: u32) -> Result<(u16, bool), ControllerError> {
        let entity = self.zone.store.get_mut(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;
        if !entity.is_mobile() {
            return Err(ControllerError::Custom(format!("0x{serial:08X} is not a mobile")));
        }
        let new_hp = entity.apply_damage(amount);
        let killed = new_hp == 0;
        let pos = entity.pos();
        let max_hp = entity.hits().map(|(_, m)| m).unwrap_or(0);
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::DamageDealt {
                map_id: self.zone.map_id,
                serial,
                source_serial,
                amount,
                new_hits: new_hp,
                max_hits: max_hp,
                x: pos.x,
                y: pos.y,
            });
        }
        Ok((new_hp, killed))
    }

    fn heal_entity(&mut self, serial: u32, amount: u16) -> Result<u16, ControllerError> {
        let entity = self.zone.store.get_mut(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;
        if !entity.is_mobile() {
            return Err(ControllerError::Custom(format!("0x{serial:08X} is not a mobile")));
        }
        let new_hp = entity.apply_heal(amount);
        let pos = entity.pos();
        let max_hp = entity.hits().map(|(_, m)| m).unwrap_or(0);
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::MobileHealed {
                map_id: self.zone.map_id,
                serial,
                amount,
                new_hits: new_hp,
                max_hits: max_hp,
                x: pos.x,
                y: pos.y,
            });
        }
        Ok(new_hp)
    }

    fn modify_mana(&mut self, serial: u32, delta: i32) -> Result<u16, ControllerError> {
        let entity = self.zone.store.get_mut(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;
        let new_mana = entity.modify_mana(delta);
        let pos = entity.pos();
        let (mana, max_mana) = entity.mana().unwrap_or((new_mana, new_mana));
        let (stamina, max_stamina) = entity.stamina().unwrap_or((0, 0));
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::ManaStaminaChanged {
                map_id: self.zone.map_id,
                serial,
                mana,
                max_mana,
                stamina,
                max_stamina,
                x: pos.x,
                y: pos.y,
            });
        }
        Ok(new_mana)
    }

    fn modify_stamina(&mut self, serial: u32, delta: i32) -> Result<u16, ControllerError> {
        let entity = self.zone.store.get_mut(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;
        let new_stamina = entity.modify_stamina(delta);
        let pos = entity.pos();
        let (mana, max_mana) = entity.mana().unwrap_or((0, 0));
        let (stamina, max_stamina) = entity.stamina().unwrap_or((new_stamina, new_stamina));
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::ManaStaminaChanged {
                map_id: self.zone.map_id,
                serial,
                mana,
                max_mana,
                stamina,
                max_stamina,
                x: pos.x,
                y: pos.y,
            });
        }
        Ok(new_stamina)
    }

    // ── Targeted events ──────────────────────────────────────────────

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
    ) {
        if let Some(tx) = &self.event_tx {
            // Use the controlled entity's position for spatial routing.
            let (px, py) = self.zone.get(source_serial)
                .map(|e| { let p = e.pos(); (p.x, p.y) })
                .unwrap_or((0, 0));
            let _ = tx.send(WorldEvent::TargetedGump {
                map_id: self.zone.map_id,
                target_player,
                source_serial,
                gump_id,
                gump_x,
                gump_y,
                layout,
                text_lines,
                pos_x: px,
                pos_y: py,
                blocking,
            });
        }
    }

    fn send_targeted_message(
        &self,
        target_player: u32,
        message: String,
        color: u16,
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::TargetedMessage {
                map_id: self.zone.map_id,
                target_player,
                message,
                color,
                pos_x: 0,
                pos_y: 0,
            });
        }
    }

    fn close_targeted_gump(
        &self,
        target_player: u32,
        gump_id: u32,
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::TargetedCloseGump {
                map_id: self.zone.map_id,
                target_player,
                gump_id,
                pos_x: 0,
                pos_y: 0,
            });
        }
    }

    // ── Inventory / equipment access ──────────────────────────────────

    fn get_backpack_serial(&self, serial: u32) -> Option<u32> {
        self.zone.get(serial)?.backpack_serial()
    }

    fn find_item_in_container(&self, container_serial: u32, graphic: u16) -> Option<(u32, u16)> {
        let info = self.zone.containers.get(container_serial)?;
        info.items.iter()
            .find(|i| i.graphic == graphic && i.amount > 0)
            .map(|i| (i.serial, i.amount))
    }

    fn consume_mana(&mut self, serial: u32, amount: u16) -> Option<u16> {
        let entity = self.zone.store.get_mut(serial)?;
        if !entity.is_mobile() {
            return None;
        }

        let (cur_mana, _max_mana) = entity.mana().unwrap_or((0, 0));
        if cur_mana < amount {
            return None; // insufficient mana
        }

        let new_mana = entity.modify_mana(-(amount as i32));
        let pos = entity.pos();
        let (mana, max_mana) = entity.mana().unwrap_or((new_mana, new_mana));
        let (stamina, max_stamina) = entity.stamina().unwrap_or((0, 0));

        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::ManaStaminaChanged {
                map_id: self.zone.map_id,
                serial,
                mana,
                max_mana,
                stamina,
                max_stamina,
                x: pos.x,
                y: pos.y,
            });
        }
        Some(new_mana)
    }

    fn consume_item(
        &mut self,
        item_serial: u32,
        amount: u16,
        expected_graphic: Option<u16>,
    ) -> Option<(u16, u16)> {
        use crate::continuum::world_event::ContainerContentChange;

        let amount = amount.max(1);

        // Find which container holds this item via the trait method.
        // HashContainerStore uses O(1) reverse index; NoContainers returns None.
        let cs = self.zone.containers.find_container_of_item(item_serial)?;

        // Read item info (graphic, amount, color, position) from the container.
        let (graphic, cur_amount, color, cx, cy) = {
            let info = self.zone.containers.get(cs)?;
            let item = info.find_item(item_serial)?;
            (item.graphic, item.amount, item.color, item.x, item.y)
        };

        // Graphic safety check.
        if let Some(expected) = expected_graphic {
            if graphic != expected {
                return None;
            }
        }

        let remaining = cur_amount.saturating_sub(amount);

        if remaining == 0 {
            // Remove item entirely from the container.
            if let Some(info) = self.zone.containers.get_mut(cs) {
                info.remove_item(item_serial);
            }

            // Emit container update event.
            self.emit_container_content_change(cs, vec![
                ContainerContentChange::ItemRemoved { item_serial },
            ]);
        } else {
            // Reduce amount in the container.
            if let Some(info) = self.zone.containers.get_mut(cs) {
                if let Some(item) = info.find_item_mut(item_serial) {
                    item.amount = remaining;
                }
            }

            self.emit_container_content_change(cs, vec![
                ContainerContentChange::ItemUpdated {
                    item_serial,
                    graphic,
                    amount: remaining,
                    x: cx,
                    y: cy,
                    color,
                },
            ]);
        }

        Some((remaining, graphic))
    }

    fn send_target_cursor(
        &self,
        target_player: u32,
        cursor_id: u32,
        cursor_type: u8,
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::TargetedTargetCursor {
                map_id: self.zone.map_id,
                target_player,
                cursor_id,
                cursor_type,
            });
        }
    }

    fn send_cross_world_teleport(
        &self,
        target_player: u32,
        map_id: u8,
        x: u16,
        y: u16,
        z: i8,
    ) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::TargetedCrossWorldTeleport {
                target_player,
                map_id,
                x,
                y,
                z,
            });
        }
    }

    fn get_item_props_any(&self, serial: u32) -> Option<Box<dyn std::any::Any>> {
        let value = self.zone.item_props.get(serial)?;
        Some(Box::new(value.clone()))
    }

    fn set_item_props_any(&mut self, serial: u32, props: Option<Box<dyn std::any::Any>>) {
        match props {
            Some(boxed) => {
                if let Ok(value) = boxed.downcast::<P::Value>() {
                    self.zone.item_props.insert(serial, *value);
                }
            }
            None => {
                self.zone.item_props.remove(serial);
            }
        }
    }

    fn subscribe_world_events(&mut self, entity_serial: u32, radius: u16) {
        if let Some(reg) = &mut self.observer_registry {
            let pos = self.zone.get(entity_serial).map(|e| e.pos());
            if let Some(pos) = pos {
                let watch_rect = TileRect::from_view(pos.x, pos.y, radius);
                reg.subscribe_controller(entity_serial, self.zone.map_id, watch_rect);
            }
        }
    }

    fn unsubscribe_world_events(&mut self, entity_serial: u32) {
        if let Some(reg) = &mut self.observer_registry {
            reg.unsubscribe_controller(entity_serial);
        }
    }

    fn remove_entity(&mut self, serial: u32) -> Result<(), ControllerError> {
        let pos = self.zone.get(serial)
            .map(|e| e.pos())
            .ok_or(ControllerError::EntityNotFound(serial))?;

        self.zone.remove(serial)
            .ok_or(ControllerError::EntityNotFound(serial))?;

        if let Some(tx) = &self.event_tx {
            let _ = tx.send(WorldEvent::EntityRemoved {
                map_id: self.zone.map_id,
                serial,
                last_pos: pos,
            });
        }

        Ok(())
    }
}

/// Private helpers for `ZoneAdapter`.
impl<E: Entity, C: ZoneContainers, P: ZoneItemProps> ZoneAdapter<'_, E, C, P> {
    /// Emit a [`WorldEvent::ContainerContentsUpdated`] for a container.
    ///
    /// Resolves the container's world position by walking up parent
    /// containers to find the root entity in the zone store.
    fn emit_container_content_change(
        &self,
        container_serial: u32,
        changes: Vec<crate::continuum::world_event::ContainerContentChange>,
    ) {
        let tx = match &self.event_tx {
            Some(tx) => tx,
            None => return,
        };
        if changes.is_empty() {
            return;
        }

        // Resolve container's world position: walk up the parent chain.
        let (x, y) = self.resolve_container_pos(container_serial).unwrap_or((0, 0));
        let _ = tx.send(WorldEvent::ContainerContentsUpdated {
            map_id: self.zone.map_id,
            container_serial,
            x,
            y,
            changes,
        });
    }

    /// Resolve a container's world position by walking up parent
    /// entities until we find one in the zone store.
    fn resolve_container_pos(&self, container_serial: u32) -> Option<(u16, u16)> {
        let mut current = container_serial;
        for _ in 0..16 {
            // Is it a world entity?
            if let Some(entity) = self.zone.get(current) {
                let pos = entity.pos();
                return Some((pos.x, pos.y));
            }
            // Is it inside another container? Walk up.
            match self.zone.containers.find_container_of_item(current) {
                Some(parent) => current = parent,
                None => {
                    // Check if it's equipped on a mobile.
                    for (_, entity) in self.zone.store.iter() {
                        if entity.equipment_serials().contains(&current) {
                            let pos = entity.pos();
                            return Some((pos.x, pos.y));
                        }
                    }
                    return None;
                }
            }
        }
        None
    }
}

/// Record for a controller: the controller plus metadata.
struct ControllerEntry<D: ControllerDef> {
    controller: Box<dyn EntityController<D>>,
    entity_serial: u32,
    /// Map the controlled entity currently lives in.
    ///
    /// The host is global across all zones but is ticked one zone at a
    /// time; this lets `tick_inner` skip controllers that do not belong to
    /// the zone being ticked, so deferred (buffered) work runs against the
    /// correct world.  Updated on cross-zone transfer via re-`attach`.
    map_id: u8,
}

/// Manages all controllers in one zone.
    ///
    /// Generic over `D: ControllerDef` — the consumer defines the
    /// event and command types through their implementation of `ControllerDef`.
    /// All controllers inside one host work with the same `D`.
    ///
    /// Stores controllers separately from the zone to allow simultaneous
    /// `&mut anima` and `&mut zone` during tick.
    ///
    /// Called from `CommandHandler::tick()` or directly.
pub struct ControllerHost<D: ControllerDef> {
    /// Controllers indexed by entity serial.
    controllers: HashMap<u32, ControllerEntry<D>>,

    /// Shared task scheduler.
    scheduler: Scheduler,

    /// Time of the last tick (for dt calculation).
    last_tick: Option<Instant>,

    _marker: PhantomData<D>,
}

impl<D: ControllerDef> ControllerHost<D> {
    /// Create empty host.
    pub fn new() -> Self {
        Self {
            controllers: HashMap::new(),
            scheduler: Scheduler::new(),
            last_tick: None,
            _marker: PhantomData,
        }
    }

    /// Attach controller for an entity.
    ///
    /// `map_id` is the zone the entity currently lives in; the host uses it
    /// to dispatch ticks and deferred timers against the correct zone.  On
    /// cross-zone transfer, re-`attach` with the new `map_id`.
    ///
    /// If a controller for this entity already existed — it is replaced.
    pub fn attach(
        &mut self,
        entity_serial: u32,
        controller: Box<dyn EntityController<D>>,
        map_id: u8,
    ) {
        // Re-stamp any pending timers for this entity so deferred/repeating
        // work follows it across zones (e.g. a pet that changed worlds).
        self.scheduler.reassign_entity_map(entity_serial, map_id);
        self.controllers.insert(entity_serial, ControllerEntry {
            controller,
            entity_serial,
            map_id,
        });
    }

    /// Remove controller for entity.
    ///
    /// Returns the controller if it existed.
    pub fn detach(&mut self, entity_serial: u32) -> Option<Box<dyn EntityController<D>>> {
        self.controllers.remove(&entity_serial).map(|e| e.controller)
    }

    /// Whether there is a controller for the given entity.
    pub fn has_controller(&self, entity_serial: u32) -> bool {
        self.controllers.contains_key(&entity_serial)
    }

    /// Number of registered controllers.
    pub fn controller_count(&self) -> usize {
        self.controllers.len()
    }

    /// Access to scheduler (for external code).
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Mutable access to scheduler.
    pub fn scheduler_mut(&mut self) -> &mut Scheduler {
        &mut self.scheduler
    }

    /// Return the earliest [`Instant`] at which the host needs a tick.
    ///
    /// Takes the minimum of:
    /// - `Scheduler::next_fire_at()` (nearest scheduled task)
    /// - `EntityController::next_tick_at()` across all attached controllers
    ///
    /// Returns `None` when there are no pending timers and no controller
    /// requests a tick.
    pub fn next_tick_at(&mut self) -> Option<Instant> {
        let sched = self.scheduler.next_fire_at();

        let ctrl = self.controllers.values()
            .filter_map(|entry| entry.controller.next_tick_at())
            .min();

        match (sched, ctrl) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Main tick: calls `tick()` on all controllers and processes scheduler.
    ///
    /// Called from game loop (via `CommandHandler::tick()` or directly).
    /// `zone` is a mutable reference to this host's zone.
    pub fn tick<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        now: Instant,
    ) where P::Value: 'static {
        self.tick_inner(zone, now, None, None);
    }

    /// Like [`tick`](Self::tick), but also publishes [`WorldEvent`]s for
    /// any entity movements triggered by controllers.
    pub fn tick_with_events<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        now: Instant,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where P::Value: 'static {
        self.tick_inner(zone, now, Some(event_tx), None);
    }

    /// Like [`tick_with_events`](Self::tick_with_events), but also passes
    /// an [`ObserverRegistry`] so controllers can subscribe to world events
    /// and subscription rects auto-update on movement.
    pub fn tick_with_observer<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        now: Instant,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
        observer_registry: &mut ObserverRegistry,
    ) where P::Value: 'static {
        self.tick_inner(zone, now, Some(event_tx), Some(observer_registry));
    }

    /// Extract the controller for `serial`, build a [`ControlContext`]
    /// against `zone`, invoke `f`, then return the controller to the map.
    ///
    /// Returns `None` if no controller is attached for `serial` (and `f` is
    /// not called).  This factors out the shared
    /// `remove → ZoneAdapter → ControlContext → insert` dance used by all
    /// event/command/tick dispatch paths.
    ///
    /// The `remove` is what frees `&mut self.scheduler` for the context:
    /// once the entry is owned locally it no longer borrows `self`.
    ///
    /// This helper does **not** filter by `map_id` — single-target dispatch
    /// is always invoked against the correct zone by the caller.  The
    /// per-zone filter for the broadcast/tick loops lives in those loops.
    fn with_controller<E, C, P, R>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        serial: u32,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<WorldEvent>>,
        observer_registry: Option<&mut ObserverRegistry>,
        f: impl FnOnce(&mut Box<dyn EntityController<D>>, &mut ControlContext) -> R,
    ) -> Option<R>
    where
        E: Entity,
        C: ZoneContainers,
        P: ZoneItemProps,
        P::Value: 'static,
    {
        let mut entry = self.controllers.remove(&serial)?;
        let access_level = entry.controller.access_level();
        let mut adapter = ZoneAdapter { zone, event_tx, observer_registry };
        let mut ctx = ControlContext::new(
            entry.entity_serial,
            access_level,
            &mut adapter,
            &mut self.scheduler,
        );
        let result = f(&mut entry.controller, &mut ctx);
        self.controllers.insert(serial, entry);
        Some(result)
    }

    fn tick_inner<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        now: Instant,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<WorldEvent>>,
        mut observer_registry: Option<&mut ObserverRegistry>,
    ) where P::Value: 'static {
        let dt = self.last_tick
            .map(|prev| now.duration_since(prev))
            .unwrap_or(Duration::ZERO);
        self.last_tick = Some(now);

        // 1. Process scheduler — collect fired timers for this zone's map.
        //    Tasks belonging to other maps stay queued for their own tick.
        let fired_actions = self.scheduler.tick(now, zone.map_id);

        // Process the fired actions.
        for action in fired_actions {
            match action {
                TaskAction::FireTimer { entity_serial, timer_id } => {
                    // Convert infrastructure FireTimer to D::Event
                    // through ControllerDef::timer_event().
                    let event = D::timer_event(entity_serial, timer_id);
                    self.send_event_internal(zone, entity_serial, event, event_tx);
                }
                TaskAction::Callback(Some(callback)) => {
                    callback();
                }
                TaskAction::Callback(None) => {}
            }
        }

        // 2. Tick all controllers.
        //
        // Borrow checker problem: we need &mut anima (from self.controllers)
        // and &mut zone at the same time.  `with_controller` solves this by
        // temporarily extracting the controller, building the context, then
        // re-inserting it.
        let serials: Vec<u32> = self.controllers.keys().copied().collect();

        for serial in serials {
            // Skip controllers that belong to another zone.  The host is
            // global but ticked per-zone; ticking a controller against the
            // wrong zone would resolve its buffered world events and
            // coroutines in the wrong world.  Peek without extracting.
            let in_zone = self.controllers.get(&serial)
                .is_some_and(|e| e.map_id == zone.map_id);
            if !in_zone {
                continue;
            }

            // Deliver any buffered world events before ticking.  The events
            // are drained from the registry up front, so the dispatch context
            // does not need the registry itself (hence `None`).
            if let Some(reg) = observer_registry.as_deref_mut() {
                let world_events = reg.drain_controller_events(serial);
                if !world_events.is_empty() {
                    self.with_controller(zone, serial, event_tx, None, |controller, ctx| {
                        for we in &world_events {
                            controller.on_world_event(ctx, we);
                        }
                    });
                }
            }

            let or_ref = observer_registry.as_deref_mut();
            self.with_controller(zone, serial, event_tx, or_ref, |controller, ctx| {
                controller.tick(ctx, dt);
            });
        }
    }

    /// Send an event to a specific entity's controller.
    pub fn send_event<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        entity_serial: u32,
        event: D::Event,
    ) where P::Value: 'static {
        self.send_event_internal(zone, entity_serial, event, None);
    }

    /// Send an event to a specific entity's controller, with WorldEvent
    /// publishing enabled.
    ///
    /// Unlike [`send_event`](Self::send_event), this variant passes the
    /// `event_tx` to the controller's context so that any world-mutating
    /// operations (movement, effects, targeted gumps) during
    /// `on_event()` are published as [`WorldEvent`]s.
    pub fn send_event_with_events<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        entity_serial: u32,
        event: D::Event,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where P::Value: 'static {
        self.send_event_internal(zone, entity_serial, event, Some(event_tx));
    }

    /// Send a global event to all controllers.
    pub fn broadcast_event<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        event: D::GlobalEvent,
    ) where P::Value: 'static {
        let serials: Vec<u32> = self.controllers.keys().copied().collect();

        for serial in serials {
            // Only dispatch to controllers living in this zone — the global
            // event runs against the passed zone's context.  Peek the map_id
            // without extracting the entry.
            let in_zone = self.controllers.get(&serial)
                .is_some_and(|e| e.map_id == zone.map_id);
            if !in_zone {
                continue;
            }

            self.with_controller(zone, serial, None, None, |controller, ctx| {
                controller.on_global_event(ctx, event.clone());
            });
        }
    }

    /// Send an external command to the entity's controller.
    pub fn send_command<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        entity_serial: u32,
        cmd: D::Command,
    ) where P::Value: 'static {
        let dispatched = self.with_controller(zone, entity_serial, None, None, |controller, ctx| {
            controller.on_command(ctx, cmd);
        });
        if dispatched.is_none() {
            warn!(
                "send_command: no anima for entity 0x{:08X}",
                entity_serial
            );
        }
    }

    /// Send an external command to the entity's controller, with
    /// WorldEvent publishing enabled.
    ///
    /// Unlike [`send_command`](Self::send_command), this variant passes
    /// the `event_tx` to the controller's context so that any
    /// world-mutating operations (movement, damage, effects, targeted
    /// gumps/messages) during `on_command()` are published as
    /// [`WorldEvent`]s.
    pub fn send_command_with_events<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        entity_serial: u32,
        cmd: D::Command,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    ) where P::Value: 'static {
        let dispatched = self.with_controller(zone, entity_serial, Some(event_tx), None, |controller, ctx| {
            controller.on_command(ctx, cmd);
        });
        if dispatched.is_none() {
            warn!(
                "send_command_with_events: no anima for entity 0x{:08X}",
                entity_serial
            );
        }
    }

    /// Internal method for sending an event.
    fn send_event_internal<E: Entity, C: ZoneContainers, P: ZoneItemProps>(
        &mut self,
        zone: &mut Zone<E, C, P>,
        entity_serial: u32,
        event: D::Event,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<WorldEvent>>,
    ) where P::Value: 'static {
        self.with_controller(zone, entity_serial, event_tx, None, |controller, ctx| {
            controller.on_event(ctx, event);
        });
    }
}

impl<D: ControllerDef> Default for ControllerHost<D> {
    fn default() -> Self {
        Self::new()
    }
}
