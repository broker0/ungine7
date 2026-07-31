//! Packet ingestion: mirror raw S->C packets into the zone's entity and
//! container stores.
//!
//! - `handle_ingest_packet` — entity-level packet mirroring
//! - `handle_ingest_container_packet` — container-level packet mirroring
//!
//! ## Text / tooltip ingestion
//!
//! In addition to entity-level updates, `handle_ingest_packet` extracts text
//! information from:
//!
//! | Packet | Text source | Stored in |
//! |--------|-------------|-----------|
//! | `0xD6` MegaClilocResponse | Full tooltip (title + properties) | `zone.item_props` via `ObjectText` |
//! | `0xC1` ClilocMessage | Localized object label | `zone.item_props` (items) / `MobileData.name` (mobiles) |
//! | `0x88` OpenPaperdoll | Player/NPC name+title | `MobileData.name` |
//! | `0x1C` SendSpeech (overhead) | Object name label (old clients) | `zone.item_props` (items) |
//! | `0xAE` UnicodeSpeech (overhead) | Object name label (new clients) | `zone.item_props` (items) |

use bytes::Bytes;
use log::{debug, info, trace, warn};

use std::collections::HashMap;

use framework::continuum::{Zone, WorldEvent};
use framework::continuum::item_props::ZoneItemProps;
use framework::continuum::container::{HashContainerStore, ZoneContainers};
use framework::ecumene::Entity as EngineEntity;
use u_core::Pos3D;

use crate::uo_engine::entity::DemoEntity;
use crate::uo_engine::ingest::ingest_into_entity_map;
use crate::uo_engine::item_props::{ItemProps, ObjectText, TextLine};

use super::item_ops::EquipmentIndex;

// ── Entity packet ingestion ─────────────────────────────────────────────

/// Mirror a raw S->C packet into the zone's entity store.
///
/// Uses the same parsing logic as [`ingest_into_entity_map`] to
/// spawn/update/remove entities.
///
/// When `emit_events` is `false` (default for replay playback),
/// this is fire-and-forget — the zone is updated silently.
///
/// When `emit_events` is `true` (used by live mirror streaming),
/// `WorldEvent::EntitySpawned` / `EntityUpdated` / `EntityRemoved`
/// events are emitted so that connected UO clients see the changes
/// in real time.
pub(super) fn handle_ingest_packet<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<WorldEvent>,
    data: &Bytes,
    emit_events: bool,
    equipment_index: &mut EquipmentIndex,
) where P::Value: 'static {
    // Parse the packet through the same logic used by
    // LogPlayer, then apply the diff to the zone.
    //
    // Update-only packets (0x77 UpdateMobile, 0x11
    // StatusBarInfo, 0x2D MobAttributes, 0xA1 UpdateHealth,
    // 0x1C SendSpeech) use `map.get_mut(serial)` inside
    // `ingest_into_entity_map`.  On an empty temp map that
    // lookup always fails and the update is silently dropped.
    //
    // Fix: pre-populate the temp map with the existing
    // entity from the zone (if any) so in-place updates
    // find the entry and can modify it.
    let mut temp: HashMap<u32, DemoEntity> = HashMap::new();

    if data.len() >= 5 {
        let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        if let Some(existing) = zone.get(serial) {
            temp.insert(serial, existing.clone());
        }
    }

    ingest_into_entity_map(data, zone.map_id, &mut temp);

    let map_id = zone.map_id;
    for (serial, entity) in &temp {
        let is_new = zone.get(*serial).is_none();
        trace!(
            "[shadow] ingest -- upsert serial={:#010X} at ({},{}) new={}",
            serial,
            EngineEntity::pos(entity).x,
            EngineEntity::pos(entity).y,
            is_new,
        );

        if is_new {
            let pos = EngineEntity::pos(entity);
            let snap = entity.snapshot();
            if let DemoEntity::Mobile(m) = entity {
                for eq in &m.items {
                    equipment_index.insert(eq.serial, *serial);
                }
            }
            if entity.is_multi() {
                info!(
                    "[shadow] spawning Multi serial={:#010X} graphic={:#06X} \
                     at ({},{},{}) map_id={}",
                    serial, EngineEntity::graphic(entity),
                    pos.x, pos.y, pos.z, map_id,
                );
            }
            zone.spawn(*serial, entity.clone());
            if emit_events {
                let _ = event_tx.send(WorldEvent::EntitySpawned {
                    map_id,
                    serial: *serial,
                    pos,
                    entity: snap,
                });
            }
        } else {
            let pos = EngineEntity::pos(entity);
            let snap = entity.snapshot();
            // Un-index old equipment, index new.
            if let Some(old) = zone.get(*serial) {
                if let DemoEntity::Mobile(m) = old {
                    for eq in &m.items {
                        equipment_index.remove(&eq.serial);
                    }
                }
            }
            if let DemoEntity::Mobile(m) = entity {
                for eq in &m.items {
                    equipment_index.insert(eq.serial, *serial);
                }
            }
            zone.update(*serial, entity.clone());
            if emit_events {
                let _ = event_tx.send(WorldEvent::EntityUpdated {
                    map_id,
                    serial: *serial,
                    pos,
                    entity: snap,
                });
            }
        }
    }

    // Handle DeleteObject (0x1D) — ingest_into_entity_map
    // removes from the map but we need to mirror that as
    // zone.remove.
    if !data.is_empty() && data[0] == 0x1D && data.len() >= 5 {
        let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        // Match LogPlayer behaviour: only delete non-mobile
        // entities.  Mobiles may walk out of view and return.
        let is_mobile = zone.get(serial)
            .map(|e| e.is_mobile())
            .unwrap_or(false);
        if !is_mobile {
            let last_pos = zone.get(serial)
                .map(|e| EngineEntity::pos(e))
                .unwrap_or(Pos3D::new(0, 0, 0));
            zone.remove(serial);
            trace!(
                "[shadow] ingest -- remove serial={:#010X}",
                serial,
            );
            if emit_events {
                let _ = event_tx.send(WorldEvent::EntityRemoved {
                    map_id,
                    serial,
                    last_pos,
                });
            }
        }
    }

    // Handle SendCustomHouse (0xD8) — decode custom house tile data
    // and register it in the zone's EntityRegistry so that
    // movement validation accounts for custom floors, walls, roofs.
    if !data.is_empty() && data[0] == 0xD8 {
        use packets::house::SendCustomHouse;
        use packets::traits::ManualPacket as _;
        use files::multi::MultiPart;

        match SendCustomHouse::from_bytes(data) {
            Ok(house) => {
                let serial = house.house_serial;

                info!(
                    "[shadow] 0xD8 SendCustomHouse received: serial={:#010X} \
                     rev={} planes={} (map_id={})",
                    serial, house.revision, house.planes.len(), zone.map_id,
                );

                // Look up the multi entity in the zone's registry to get
                // position and graphic.  Copy the values out so we can
                // mutate the registry afterwards.
                let entity_info = zone.registry.get_any(serial).map(|(w, e)| {
                    let pos = EngineEntity::pos(e);
                    let graphic = EngineEntity::graphic(e);
                    (w, pos.x, pos.y, graphic)
                });

                // Also check the entity store directly (not just registry)
                let in_store = zone.get(serial).is_some();
                let store_is_multi = zone.get(serial).map(|e| e.is_multi()).unwrap_or(false);

                info!(
                    "[shadow] 0xD8 serial={:#010X}: registry.get_any={}, \
                     store.get={}, store.is_multi={}",
                    serial,
                    entity_info.is_some(),
                    in_store,
                    store_is_multi,
                );

                if let Some((world, multi_x, multi_y, graphic)) = entity_info {
                    info!(
                        "[shadow] 0xD8 serial={:#010X}: found in registry — \
                         world={} pos=({},{}) graphic={:#06X}",
                        serial, world, multi_x, multi_y, graphic,
                    );

                    // Resolve the standard MultiDef to obtain the foundation
                    // bounding box (relative extent).  Mode-2 planes need
                    // this to compute implicit X/Y for each tile.
                    let extent = zone.registry.resolve_multi_def(graphic)
                        .map(|def| (def.extent.x_min, def.extent.y_min,
                                    def.extent.x_max, def.extent.y_max));

                    let (fx_min, fy_min, fx_max, fy_max) = extent
                        .unwrap_or((0, 0, 0, 0));

                    info!(
                        "[shadow] 0xD8 serial={:#010X}: foundation extent=({},{})..({},{})",
                        serial, fx_min, fy_min, fx_max, fy_max,
                    );

                    match house.decode_all_tiles(fx_min, fy_min, fx_max, fy_max) {
                        Ok(tiles) => {
                            let parts: Vec<MultiPart> = tiles.iter().map(|t| MultiPart {
                                tile_id: t.tile_id,
                                x: t.x,
                                y: t.y,
                                z: t.z,
                                flags: 1,
                            }).collect();

                            info!(
                                "[shadow] 0xD8 SendCustomHouse serial={:#010X} \
                                 rev={} planes={} tiles={} → calling zone.registry.add_custom()",
                                serial, house.revision, house.planes.len(), parts.len(),
                            );

                            // Log a few sample tiles for verification
                            for (i, t) in tiles.iter().take(5).enumerate() {
                                debug!(
                                    "[shadow] 0xD8 serial={:#010X} sample tile[{}]: \
                                     id={:#06X} x={} y={} z={}",
                                    serial, i, t.tile_id, t.x, t.y, t.z,
                                );
                            }

                            zone.registry.add_custom(serial, &parts, world);

                            info!(
                                "[shadow] 0xD8 serial={:#010X}: add_custom done, \
                                 registry custom_defs count={}",
                                serial,
                                zone.registry.cached_defs(),
                            );
                        }
                        Err(e) => {
                            warn!(
                                "[shadow] 0xD8 SendCustomHouse serial={:#010X}: \
                                 tile decode failed: {}",
                                serial, e,
                            );
                        }
                    }
                } else {
                    warn!(
                        "[shadow] 0xD8 SendCustomHouse serial={:#010X}: \
                         entity NOT in zone registry! in_store={} is_multi={} \
                         (custom house tiles will NOT be used for collision)",
                        serial, in_store, store_is_multi,
                    );
                }
            }
            Err(e) => {
                warn!(
                    "[shadow] 0xD8 SendCustomHouse: parse failed: {}",
                    e,
                );
            }
        }
    }

    // ── Text / tooltip packet handling ────────────────────────────────────
    //
    // These packets carry text about objects (names, properties, cliloc
    // lines). They are processed here because `ingest_into_entity_map` only
    // has access to a HashMap<serial, DemoEntity> and cannot write to
    // `zone.item_props`.

    ingest_text_packet(zone, data);
}

// ── Text / tooltip ingestion ─────────────────────────────────────────────

/// Extract text / tooltip data from a single S->C packet and store it in
/// `zone.item_props` (for items) or `MobileData.name` (for mobiles).
///
/// Handles:
/// - `0xD6` MegaClilocResponse — full tooltip (title + property lines).
/// - `0xC1` ClilocMessage       — localized overhead label.
/// - `0x88` OpenPaperdoll       — character name / title string.
/// - `0x1C` SendSpeech          — ASCII overhead label (items, old clients).
/// - `0xAE` UnicodeSpeech       — UTF-16 overhead label (items, new clients).
fn ingest_text_packet<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    data: &Bytes,
) where P::Value: 'static {
    if data.is_empty() {
        return;
    }

    match data[0] {
        // ── 0xD6 MegaClilocResponse ───────────────────────────────────────
        // Full tooltip: first entry is the title/name, subsequent entries are
        // property rows (damage, resist, durability, …).
        //
        // For mobiles:  overwrite MobileData.name if the title is non-empty.
        // For items:    upsert ItemProps with the full ObjectText, preserving
        //               existing meta / weight_override.
        0xD6 => {
            use packets::tooltip::MegaClilocResponse;
            use packets::traits::ManualPacket as _;

            match MegaClilocResponse::from_bytes(data) {
                Ok(resp) if !resp.entries.is_empty() => {
                    let serial = resp.serial;
                    let mut text = ObjectText::default();
                    for entry in &resp.entries {
                        text.lines.push(TextLine::Cliloc {
                            id: entry.cliloc_id,
                            args: entry.text.clone(),
                        });
                    }
                    text.revision += 1;

                    trace!(
                        "[shadow] 0xD6 MegaCliloc serial={:#010X} lines={}",
                        serial, text.lines.len()
                    );

                    // Mobile: update name field directly.
                    if let Some(DemoEntity::Mobile(m)) = zone.store.get_mut(serial) {
                        if let Some(title) = text.title_string() {
                            if m.name.is_empty() || m.name.starts_with("[mob ") {
                                m.name = title;
                            }
                        }
                        return;
                    }

                    // Item / Multi: upsert ItemProps preserving meta.
                    let mut props = zone.item_props
                        .get(serial)
                        .map(|p| {
                            // SAFETY: ZoneItemProps::Value = ItemProps in demo-server.
                            // We need a concrete downcast — use the same unsafe_cast
                            // pattern used elsewhere in the handler.
                            let any: &dyn std::any::Any = p;
                            any.downcast_ref::<ItemProps>().cloned().unwrap_or_default()
                        })
                        .unwrap_or_default();

                    props.text = text;
                    let boxed: P::Value = unsafe_cast_props::<P>(props);
                    zone.item_props.insert(serial, boxed);
                }
                Ok(_) => {}
                Err(e) => {
                    debug!("[shadow] 0xD6 MegaClilocResponse parse failed: {e}");
                }
            }
        }

        // ── 0xC1 ClilocMessage ────────────────────────────────────────────
        // Localized overhead message (NPC speech, system label).
        //
        // Pattern for an object name label:
        //   - serial is valid (non-zero, not 0xFFFF_FFFF)
        //   - speech_type is Normal or MessageCorner
        //   - name field carries the NPC / object name
        //
        // We store the cliloc line as the title in ObjectText only when the
        // existing name is absent / generic, to avoid overwriting richer data.
        0xC1 => {
            use packets::speech::{ClilocMessage, SpeechType};
            use packets::traits::ManualPacket as _;

            let Ok(msg) = ClilocMessage::from_bytes(data) else { return };
            let valid_serial = msg.serial != 0 && msg.serial != 0xFFFF_FFFF;
            let is_overhead = matches!(
                msg.speech_type,
                SpeechType::Normal | SpeechType::MessageCorner
            );
            if !valid_serial || !is_overhead {
                return;
            }

            let name_str = msg.name.to_string();
            let args = if msg.arguments.is_empty() {
                None
            } else {
                Some(msg.arguments.clone())
            };

            // Mobile: fill name from cliloc args or name field.
            if let Some(DemoEntity::Mobile(m)) = zone.store.get_mut(msg.serial) {
                if m.name.is_empty() || m.name.starts_with("[mob ") {
                    m.name = if !name_str.is_empty() {
                        name_str
                    } else {
                        // Fall back to cliloc placeholder.
                        format!("[cliloc #{}]", msg.message_number)
                    };
                }
                return;
            }

            // Item: upsert ObjectText title with the cliloc line.
            let mut props = zone.item_props
                .get(msg.serial)
                .and_then(|p| {
                    let any: &dyn std::any::Any = p;
                    any.downcast_ref::<ItemProps>().cloned()
                })
                .unwrap_or_default();

            // Only set if currently unnamed to avoid overwriting richer data.
            if props.text.is_empty() || props.name().map_or(true, |n| n.starts_with("[item ")) {
                props.text.set_title_cliloc(msg.message_number, args);
            }
            let boxed: P::Value = unsafe_cast_props::<P>(props);
            zone.item_props.insert(msg.serial, boxed);
        }

        // ── 0x88 OpenPaperdoll ────────────────────────────────────────────
        // Contains character name + title as a 60-byte fixed string.
        // Fill MobileData.name if currently empty or placeholder.
        0x88 => {
            use packets::character::OpenPaperdoll;
            use packets::traits::BasicPacket as _;

            let Ok(pd) = OpenPaperdoll::from_bytes(data) else { return };
            let text = pd.text.to_string();
            // The text may be "Name, Title" or just "Name".
            // Extract just the name part (before the first comma).
            let name_part = text.split(',').next().unwrap_or(&text).trim().to_string();
            if name_part.is_empty() {
                return;
            }
            if let Some(DemoEntity::Mobile(m)) = zone.store.get_mut(pd.serial) {
                if m.name.is_empty() || m.name.starts_with("[mob ") {
                    m.name = name_part;
                }
            }
        }

        // ── 0x1C SendSpeech (overhead, items only) ────────────────────────
        // Mobile names are handled in ingest_into_entity_map.  Here we catch
        // the item case: an overhead label from the server for an item serial.
        //
        // Pattern:
        //   - valid serial (non-zero, not 0xFFFF_FFFF)
        //   - speech_type == Normal (overhead, not system corner)
        //   - serial resolves to an Item entity (not Mobile/Multi)
        //   - name field is non-empty (the NPC / object identifier)
        0x1C => {
            use packets::speech::{SendSpeech, SpeechType};
            use packets::traits::ManualPacket as _;

            let Ok(speech) = SendSpeech::from_bytes(data) else { return };
            let valid_serial = speech.serial != 0 && speech.serial != 0xFFFF_FFFF;
            let is_overhead = matches!(speech.speech_type, SpeechType::Normal);
            if !valid_serial || !is_overhead {
                return;
            }
            // Only apply to items — mobiles are handled in ingest_into_entity_map.
            let is_item = matches!(zone.store.get(speech.serial), Some(DemoEntity::Item { .. }));
            if !is_item {
                return;
            }
            // Use the message (the displayed label) as the name, falling back
            // to the name field.  A non-empty message is more reliable for
            // items (e.g. "a sword", "gold coins (1000)").
            let label = if !speech.message.is_empty() {
                speech.message.clone()
            } else if !speech.name.is_empty() {
                speech.name.clone()
            } else {
                return;
            };

            let mut props = zone.item_props
                .get(speech.serial)
                .and_then(|p| {
                    let any: &dyn std::any::Any = p;
                    any.downcast_ref::<ItemProps>().cloned()
                })
                .unwrap_or_default();

            if props.text.is_empty() || props.name().map_or(true, |n| n.starts_with("[item ")) {
                props.text.set_title(label);
            }
            let boxed: P::Value = unsafe_cast_props::<P>(props);
            zone.item_props.insert(speech.serial, boxed);
        }

        // ── 0xAE UnicodeSpeech (overhead, items only) ─────────────────────
        // UTF-16 variant of SendSpeech — same item-name extraction logic.
        0xAE => {
            use packets::speech::{SpeechType, UnicodeSpeech};
            use packets::traits::BasicPacket as _;

            let Ok(speech) = UnicodeSpeech::from_bytes(data) else { return };
            let valid_serial = speech.serial != 0 && speech.serial != 0xFFFF_FFFF;
            let is_overhead = matches!(speech.speech_type, SpeechType::Normal);
            if !valid_serial || !is_overhead {
                return;
            }
            let is_item = matches!(zone.store.get(speech.serial), Some(DemoEntity::Item { .. }));
            if !is_item {
                return;
            }
            let message = speech.message.to_string();
            let name_str = speech.name.to_string();
            let label = if !message.is_empty() {
                message
            } else if !name_str.is_empty() {
                name_str
            } else {
                return;
            };

            let mut props = zone.item_props
                .get(speech.serial)
                .and_then(|p| {
                    let any: &dyn std::any::Any = p;
                    any.downcast_ref::<ItemProps>().cloned()
                })
                .unwrap_or_default();

            if props.text.is_empty() || props.name().map_or(true, |n| n.starts_with("[item ")) {
                props.text.set_title(label);
            }
            let boxed: P::Value = unsafe_cast_props::<P>(props);
            zone.item_props.insert(speech.serial, boxed);
        }

        _ => {}
    }
}

/// Cast `ItemProps` into a `P::Value` for storage in `zone.item_props`.
///
/// # Safety
/// The zone's `ZoneItemProps::Value` must be `ItemProps`.  This holds for all
/// users of this handler (demo-server, path-server).
fn unsafe_cast_props<P: ZoneItemProps>(props: ItemProps) -> P::Value where P::Value: 'static {
    let boxed: Box<dyn std::any::Any> = Box::new(props);
    *boxed
        .downcast::<P::Value>()
        .expect("ZoneItemProps::Value must be ItemProps")
}

// ── Container packet ingestion ──────────────────────────────────────────

/// Ingest a container-related S->C packet (0x24, 0x25, 0x3C) into
/// the zone's container store.
///
/// Also marks the corresponding entity as `is_container = true`
/// in the entity store (for 0x24 packets).
pub(super) fn handle_ingest_container_packet<P: ZoneItemProps>(
    zone: &mut Zone<DemoEntity, HashContainerStore, P>,
    data: &Bytes,
) {
    if data.is_empty() {
        return;
    }

    let container_serial = match data[0] {
        0x24 => {
            if data.len() < 7 {
                return;
            }
            let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
            let gump_model = u16::from_be_bytes([data[5], data[6]]);
            zone.containers.ingest_open(serial, gump_model);
            // Mark entity as container in the entity store.
            if let Some(entity) = zone.store.get_mut(serial) {
                if let DemoEntity::Item { is_container, .. } = entity {
                    *is_container = true;
                }
            }
            serial
        }
        0x25 => {
            use packets::interaction::AddItemToContainer;
            use packets::traits::ManualPacket;
            let Ok(add) = AddItemToContainer::from_bytes(data) else { return };
            let cs = add.container_serial();
            let item = framework::continuum::ContainerItem {
                serial: add.serial(),
                graphic: add.graphic(),
                amount: add.amount(),
                x: add.x(),
                y: add.y(),
                color: add.color(),
                grid_index: add.grid_index(),
            };
            zone.containers.ingest_item_upsert(cs, item);
            cs
        }
        0x3C => {
            use packets::interaction::ContainerContent;
            use packets::traits::ManualPacket;
            let Ok(cc) = ContainerContent::from_bytes(data) else { return };
            let Some(cs) = cc.container_serial() else { return };
            let items = framework::diorama::container_items_from_content(&cc);
            zone.containers.ingest_content(cs, items);
            cs
        }
        _ => return,
    };

    trace!(
        "[shadow] ingest container packet 0x{:02X} -- container={:#010X} ({} items)",
        data[0],
        container_serial,
        zone.containers.get(container_serial).map(|c| c.item_count()).unwrap_or(0),
    );
}
