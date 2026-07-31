//! Interaction packet handlers: SingleClick, DoubleClick, GetMobileStatus, paperdoll.

use log::info;

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use packets::character::OpenPaperdoll;
use packets::interaction::{DoubleClick, GetMobileStatus, SingleClick};
use packets::mobile_flags::MobileFlags;
use packets::speech::{SendSpeech, SpeechType};
use packets::status::StatusBarInfo;

use framework::continuum::WorkerCommand;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::base_handler::BaseCommand;
use common::uo_engine::handler::{ResolvedItemName, UseObjectResult};
use crate::{DemoCommand, DemoWorkerTx};
use crate::constants::hue;
use crate::game_util::chebyshev;

use super::containers::ContainerKind;
use super::util::notoriety_hue;
use super::PlayerState;

/// Maximum Chebyshev distance for using (double-clicking) a ground object.
/// Matches the UO client's interaction range: one free tile between the
/// player and the object.  Paperdolls and scripted objects are exempt.
const USE_RANGE: u16 = 2;

/// Eye-height offset for LOS checks (same as item_ops / combat).
const EYE_HEIGHT: i16 = 14;

/// Resolve the per-viewer wire notoriety of a mobile `m` as seen by `player`.
///
/// Mirrors the resolution done in the world-event render path so the name
/// label colour matches the health-bar / mobile colour.
fn resolve_single_click_notoriety(
    player: &PlayerState,
    m: &common::uo_engine::entity::MobileData,
) -> packets::movement::Notoriety {
    use common::uo_engine::notoriety::{resolve_notoriety, NotorietyClass, NotorietyView};

    let target_view = m.notoriety_view();
    let viewer_view = match &player.notoriety_ctx {
        Some(c) => NotorietyView {
            class: NotorietyClass::from_u8(c.class),
            guild_id: c.guild_id,
            is_player: c.is_player,
        },
        None => NotorietyView::default(),
    };
    let is_self = m.serial == player.serial;
    let aggressor_to_viewer = m.is_aggressor_to(player.serial);
    resolve_notoriety(&viewer_view, &target_view, is_self, aggressor_to_viewer)
}

// ── 0x09 SingleClick ──────────────────────────────────────────────────────

/// Format an item's SingleClick label from its resolved name.
///
/// Stackable items with an amount greater than one and a generic (non-explicit)
/// name get a leading count, matching classic UO (`"1543 gold coins"`).
/// Explicitly named items (crafted/named loot/quest) are proper nouns and are
/// shown verbatim, as are non-stackable items.
fn format_item_label(resolved: &ResolvedItemName) -> String {
    if resolved.stackable && resolved.amount > 1 && !resolved.explicit_name {
        format!("{} {}", resolved.amount, resolved.base_name)
    } else {
        resolved.base_name.clone()
    }
}

/// Respond with a name label (SendSpeech 0x1C) for the clicked entity.
pub(super) async fn handle_single_click(
    packet: &RawPacket,
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
) -> Option<Vec<RawPacket>> {
    let click = SingleClick::from_bytes(&packet.data).ok()?;
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    // Resolve the clicked entity from the top-level store.  Items inside
    // containers (e.g. the player's own backpack) are NOT in this store, so
    // `get_entity` returns `None` for them — we handle that below via the
    // item-name resolver, which searches all storage tiers.
    let entity = engine.get_entity(click.serial).await;

    let (name, graphic, color) = match entity.as_ref() {
        Some(DemoEntity::Mobile(m)) => {
            let label = if m.name.is_empty() {
                format!("[mob 0x{:04X}]", m.graphic)
            } else {
                m.name.clone()
            };
            let hue = if m.status.golden_health() {
                hue::GOLDEN_HEALTH
            } else {
                // Per-viewer notoriety colour (relative to this player).
                notoriety_hue(resolve_single_click_notoriety(player, m))
            };
            (label, m.graphic, hue)
        }
        Some(DemoEntity::Multi { graphic, .. }) => {
            (format!("[multi 0x{:04X}]", graphic), *graphic, hue::SYSTEM_GRAY)
        }
        // Either a top-level item, or an item inside a container (None from
        // `get_entity`).  Resolve the name across all storage tiers; this is
        // what makes backpack items show a label at all.
        Some(DemoEntity::Item { .. }) | None => {
            let resolved = engine.resolve_item_name(click.serial).await?;
            let label = format_item_label(&resolved);

            // If the item has a multi-line ObjectText (tooltip from mirror/0xD6),
            // send the title as overhead + additional lines as system messages.
            if let Some(props) = engine.get_item_props(click.serial).await {
                if props.text.lines.len() > 1 {
                    let packets: Vec<RawPacket> = props.text
                        .to_speech_lines(click.serial, resolved.graphic, hue::SYSTEM_GRAY)
                        .into_iter()
                        .map(|s| RawPacket::s2c(s.to_bytes()))
                        .collect();
                    if !packets.is_empty() {
                        return Some(packets);
                    }
                }
            }

            (label, resolved.graphic, hue::SYSTEM_GRAY)
        }
    };

    Some(vec![RawPacket::s2c(
        SendSpeech {
            serial: click.serial,
            model: graphic,
            speech_type: SpeechType::Normal,
            color,
            font: 3,
            name: name.clone(),
            message: name,
        }
        .to_bytes(),
    )])
}

// ── 0x34 GetMobileStatus ─────────────────────────────────────────────────

/// Respond with a StatusBarInfo (0x11) packet.
pub(super) async fn handle_get_status(
    packet: &RawPacket,
    player: &PlayerState,
    held_item: &Option<super::items::HeldItem>,
    worker_tx: &DemoWorkerTx,
) -> Option<Vec<RawPacket>> {
    let req = GetMobileStatus::from_bytes(&packet.data).ok()?;
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let entity = engine.get_entity(req.serial).await?;

    if let Some(m) = entity.mobile()
    {
        let label = if m.name.is_empty() {
            format!("[mob 0x{:04X}]", m.graphic)
        } else {
            m.name.clone()
        };

        // For the controlled player — send full stats (status_flag = 1).
        // For other mobiles — basic info only (status_flag = 0).
        let is_self = m.serial == player.serial;

        // Compute weight for the controlled player.
        let (cur_weight, _max_weight) = if is_self {
            let held = held_item.as_ref().map(|h| (h.serial, h.graphic, h.amount));
            engine.compute_weight(m.serial, held)
                .await
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        // Compute armor rating for the controlled player.
        let armor_rating = if is_self {
            engine.query_equipment_armor(m.serial)
                .await
                .map(|p| p.total())
                .unwrap_or(0)
        } else {
            0
        };

        // Real gold tally (self only, recursive including sub-containers).
        let gold = if is_self {
            let held = held_item.as_ref().map(|h| (h.serial, h.graphic, h.amount));
            engine.count_gold(m.serial, held).await
        } else {
            0
        };

        let sbi = StatusBarInfo {
            serial: m.serial,
            name: packets::u_io::FixedString::new(&label),
            hit_points: m.hits,
            max_hit_points: m.hits_max,
            name_change_flag: 0,
            status_flag: if is_self { 1 } else { 0 },
            is_female: if is_self { Some(crate::game_util::is_female_body(m.graphic)) } else { None },
            stats: if is_self {
                Some(packets::status::BaseStats {
                    strength: m.str_,
                    dexterity: m.dex,
                    intelligence: m.int,
                    stamina: m.stamina,
                    max_stamina: m.stamina_max,
                    mana: m.mana,
                    max_mana: m.mana_max,
                    gold,
                    armor_rating,
                    weight: cur_weight,
                })
            } else {
                None
            },
            uoml: None,
            uor: None,
            aos: None,
            uokr: None,
        };
        Some(vec![RawPacket::s2c(sbi.to_bytes())])
    } else {
        None
    }
}

// ── 0x06 DoubleClick ─────────────────────────────────────────────────────

/// Returned alongside packets when a container was opened.
pub(super) struct OpenedContainer {
    pub serial: u32,
    pub kind: ContainerKind,
}

/// Result of a double-click: response packets + optional container info.
pub(super) struct DoubleClickResult {
    pub packets: Vec<RawPacket>,
    pub opened_container: Option<OpenedContainer>,
}

/// Open a paperdoll for mobiles, a container for containers, or send a
/// description message for other objects.
pub(super) async fn handle_double_click(
    packet: &RawPacket,
    player: &PlayerState,
    access_level: common::uo_engine::auth::AccessLevel,
    worker_tx: &DemoWorkerTx,
) -> Option<DoubleClickResult> {
    let dc = DoubleClick::from_bytes(&packet.data).ok()?;
    let is_paperdoll = dc.serial & 0x8000_0000 != 0;
    let clean_serial = dc.serial & 0x7FFF_FFFF;

    if is_paperdoll {
        // Explicit paperdoll request (high bit set by the client).
        let pkts = open_paperdoll(clean_serial, player, worker_tx).await?;
        return Some(DoubleClickResult { packets: pkts, opened_container: None });
    }

    // Spawner objects: only GameMaster+ may interact.  The object is hidden
    // from regular players, but gate here too so a crafted DoubleClick packet
    // can't reach the spawner controller.
    {
        let engine = crate::game_util::engine_for(worker_tx, player.world);
        if let Some(props) = engine.get_item_props(clean_serial).await {
            use common::uo_engine::auth::AccessLevel;
            if props.get_meta_str(crate::spawner_object::META_SPAWN_TEMPLATE).is_some()
                && access_level < AccessLevel::GameMaster
            {
                return Some(DoubleClickResult { packets: vec![], opened_container: None });
            }
        }
    }

    // Check if the object has a controller script attached.
    // Controllers handle their own range/access checks.  This is also where
    // teleporter objects are handled: a `teleporter` controller is attached to
    // them, and step-on teleportation is detected engine-side — double-clicking
    // a teleporter is intentionally inert (mirrors UO; use a travel stone for a
    // click-to-travel gump instead).
    {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = DemoCommand::Base(BaseCommand::UseObject {
            serial: clean_serial,
            player_serial: player.serial,
            reply: reply_tx,
        });
        let _ = worker_tx.send(WorkerCommand::MapCommand(player.world, cmd)).await;
        if let Ok(UseObjectResult::HandledByController) = reply_rx.await {
            return Some(DoubleClickResult { packets: vec![], opened_container: None });
        }
    }

    // ── Range + LOS check for ground entities ────────────────────────
    //
    // Ground items/containers must be within USE_RANGE (Chebyshev ≤ 2)
    // and line of sight.  Paperdolls, own equipment, and container items
    // are exempt (handled by later branches or game-logic handlers).
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let entity = engine.get_entity(clean_serial).await;

    if let Some(ref ent) = entity {
        let ground_pos = match ent {
            DemoEntity::Item { x, y, z, .. } => Some((*x, *y, *z)),
            DemoEntity::Multi { x, y, z, .. } => Some((*x, *y, *z)),
            DemoEntity::Mobile(_) => None, // paperdoll — no range check
        };

        if let Some((ix, iy, iz)) = ground_pos {
            let dist = chebyshev(player.x, player.y, ix, iy);
            if dist > USE_RANGE {
                info!(
                    "[interaction] DoubleClick {:#010X}: too far (dist={}, max={})",
                    clean_serial, dist, USE_RANGE,
                );
                return Some(DoubleClickResult {
                    packets: vec![too_far_away_packet()],
                    opened_container: None,
                });
            }
            if !engine.check_los(
                player.x, player.y, player.z as i16 + EYE_HEIGHT,
                ix, iy, iz as i16,
            ).await {
                info!(
                    "[interaction] DoubleClick {:#010X}: no LOS from ({},{},{}) to ({},{},{})",
                    clean_serial, player.x, player.y, player.z, ix, iy, iz,
                );
                return Some(DoubleClickResult {
                    packets: vec![out_of_sight_packet()],
                    opened_container: None,
                });
            }
        }
    }

    // Try to open as container.
    if let Some(container) = engine.get_container(clean_serial).await {
        use packets::interaction::DeleteObject;
        use packets::traits::BasicPacket;

        let is_corpse = container.gump_model == 0x0009;
        let version = player.client_version;

        let mut pkts = Vec::new();
        pkts.push(common::spawn::build_draw_container(container.serial, container.gump_model, version));

        if is_corpse {
            // Corpse containers need special handling:
            // 1. Send CorpseClothing (0x89) with cosmetic items for display.
            // 2. Send DeleteObject (0x1D) for non-lootable cosmetic items.
            // 3. Send ContainerContent (0x3C) with only lootable items.

            // Read corpse clothing from item props metadata.
            let corpse_props = engine.get_item_props(clean_serial).await;
            let clothing_data: Vec<(u8, u32, u16, u16)> = corpse_props.as_ref()
                .and_then(|p| p.get_meta_str("corpse_clothing"))
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();

            // Build cosmetic item set (hair, beard, face layers).
            let cosmetic_serials: std::collections::HashSet<u32> = clothing_data.iter()
                .filter(|(layer_wire, _, _, _)| {
                    let layer = packets::layer::Layer::from_wire(*layer_wire);
                    matches!(layer, packets::layer::Layer::Hair | packets::layer::Layer::Beard | packets::layer::Layer::Face)
                })
                .map(|(_, serial, _, _)| *serial)
                .collect();

            // Delete cosmetic items (they are visual-only on the corpse).
            for &cosmetic_serial in &cosmetic_serials {
                pkts.push(RawPacket::s2c(encode_packet(&DeleteObject {
                    id: DeleteObject::ID,
                    serial: cosmetic_serial,
                })));
            }

            // Container content: only lootable items (exclude cosmetics).
            let items: Vec<(u32, u16, u16, u16, u16, u32, u16)> = container.items.iter()
                .filter(|i| !cosmetic_serials.contains(&i.serial))
                .map(|i| (i.serial, i.graphic, i.amount, i.x, i.y, container.serial, i.color))
                .collect();
            pkts.push(common::spawn::build_container_content(&items, version));
        } else {
            // Normal container: send all items.
            let items: Vec<(u32, u16, u16, u16, u16, u32, u16)> = container.items.iter()
                .map(|i| (i.serial, i.graphic, i.amount, i.x, i.y, container.serial, i.color))
                .collect();
            pkts.push(common::spawn::build_container_content(&items, version));
        }

        // Determine the container kind for open-container tracking.
        let kind = resolve_container_kind(
            clean_serial,
            container.gump_model(),
            player,
            worker_tx,
        ).await;

        return Some(DoubleClickResult {
            packets: pkts,
            opened_container: Some(OpenedContainer {
                serial: clean_serial,
                kind,
            }),
        });
    }

    // Look up the entity to decide what to do.
    // (already fetched above for range check; reuse it)

    // Double-clicking a mobile opens its paperdoll (standard UO behaviour).
    if matches!(&entity, Some(DemoEntity::Mobile(_))) {
        let pkts = open_paperdoll(clean_serial, player, worker_tx).await?;
        return Some(DoubleClickResult { packets: pkts, opened_container: None });
    }

    // Non-mobile, non-container — describe what was clicked.
    let desc = match &entity {
        Some(DemoEntity::Item { graphic, serial, .. }) => {
            Some(format!("item {:#06X} ({:#010X})", graphic, serial))
        }
        Some(DemoEntity::Multi { graphic, serial, .. }) => {
            Some(format!("multi {:#06X} ({:#010X})", graphic, serial))
        }
        _ => {
            // Not a top-level entity — check inside containers (backpack
            // items like scrolls, reagents, bandages).  If found there,
            // silently ignore; game-logic handlers (Rust or Lua) will
            // deal with these items.
            let item_info = engine.find_item_info(clean_serial).await;
            if let Some((_, graphic, _, _)) = &item_info {
                info!("[infra] DoubleClick {:#010X}: container item graphic={:#06X} — skipping (game-logic)", clean_serial, graphic);
                None // item lives inside a container — nothing for infra to do
            } else {
                Some(format!("unknown object {:#010X}", clean_serial))
            }
        }
    };

    match desc {
        Some(text) => Some(DoubleClickResult {
            packets: vec![RawPacket::s2c(
                SendSpeech {
                    serial: 0xFFFF_FFFF,
                    model: 0xFFFF,
                    speech_type: SpeechType::System,
                    color: hue::SYSTEM_GRAY,
                    font: 3,
                    name: String::new(),
                    message: format!("[demo] {}", text),
                }
                .to_bytes(),
            )],
            opened_container: None,
        }),
        None => None, // container item — handled by game-logic, not infra
    }
}

/// Determine the [`ContainerKind`] for a freshly opened container.
async fn resolve_container_kind(
    serial: u32,
    gump_model: u16,
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
) -> ContainerKind {
    let engine = crate::game_util::engine_for(worker_tx, player.world);

    // Shop gump model → vendor container.
    if gump_model == 0x0030 {
        info!(
            "[resolve_kind] container=0x{:08X} gump=0x{:04X} → Vendor",
            serial, gump_model,
        );
        return ContainerKind::Vendor;
    }

    // Check if this is the player's own backpack (equipped on Layer::Backpack).
    if let Some(entity) = engine.get_entity(player.serial).await {
        if let Some(m) = entity.mobile() {
            if m.items.iter().any(|eq| eq.serial == serial && eq.layer == packets::layer::Layer::Backpack) {
                info!(
                    "[resolve_kind] container=0x{:08X} is equipped on player 0x{:08X} \
                     at Layer::Backpack → OwnBackpack",
                    serial, player.serial,
                );
                return ContainerKind::OwnBackpack;
            }
        }
    }

    // Check if the container is a ground entity.
    if let Some(entity) = engine.get_entity(serial).await {
        match &entity {
            DemoEntity::Item { x, y, .. } => {
                info!(
                    "[resolve_kind] container=0x{:08X} found as ground Item at ({},{}) → Ground",
                    serial, x, y,
                );
                return ContainerKind::Ground { x: *x, y: *y };
            }
            DemoEntity::Mobile(_) | DemoEntity::Multi { .. } => {
                info!(
                    "[resolve_kind] container=0x{:08X} found as Mobile/Multi → Ground (fallback at player pos)",
                    serial,
                );
                return ContainerKind::Ground { x: player.x, y: player.y };
            }
        }
    }

    // Check if the container is inside another container (nested).
    if let Some(parent_serial) = engine.find_container_of_item(serial).await {
        info!(
            "[resolve_kind] container=0x{:08X} found inside parent=0x{:08X} → Nested",
            serial, parent_serial,
        );
        return ContainerKind::Nested { parent_serial };
    }

    // Not found in store or containers — likely a bank box or
    // equipped container we can't resolve further.
    // Default to Bank-like behaviour (close on move) as a safe default
    // for unresolvable equipped containers like the bank box.
    info!(
        "[resolve_kind] container=0x{:08X}: not backpack, not ground, not nested → \
         fallback Bank (will close on move)",
        serial,
    );
    ContainerKind::Bank { x: player.x, y: player.y }
}

/// Build and return an `OpenPaperdoll` (0x88) packet for the given mobile.
async fn open_paperdoll(
    serial: u32,
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
) -> Option<Vec<RawPacket>> {
    let engine = crate::game_util::engine_for(worker_tx, player.world);
    let entity = engine.get_entity(serial).await;
    let Some(m) = entity.as_ref().and_then(|e| e.mobile())
    else {
        return None;
    };

    let is_self = m.serial == player.serial;
    let label = if m.name.is_empty() {
        format!("[mob 0x{:04X}]", m.serial)
    } else {
        m.name.clone()
    };

    // Set "can alter paperdoll" bit for the player's own paperdoll
    // so the client allows equip/unequip.
    let mut flags = m.status;
    if is_self {
        flags = MobileFlags(flags.0 | 0x02);
    }

    let pkt = OpenPaperdoll {
        id: OpenPaperdoll::ID,
        serial: m.serial,
        text: packets::u_io::FixedString::new(&label),
        flags,
    };
    Some(vec![RawPacket::s2c(encode_packet(&pkt))])
}

// ── System message helpers ────────────────────────────────────────────────

/// Build a "That is too far away." system message packet.
fn too_far_away_packet() -> RawPacket {
    RawPacket::s2c(
        SendSpeech {
            serial: 0xFFFF_FFFF,
            model: 0xFFFF,
            speech_type: SpeechType::System,
            color: hue::SYSTEM_GRAY,
            font: 3,
            name: String::new(),
            message: "That is too far away.".to_string(),
        }
        .to_bytes(),
    )
}

/// Build a "That is out of sight." system message packet.
fn out_of_sight_packet() -> RawPacket {
    RawPacket::s2c(
        SendSpeech {
            serial: 0xFFFF_FFFF,
            model: 0xFFFF,
            speech_type: SpeechType::System,
            color: hue::SYSTEM_GRAY,
            font: 3,
            name: String::new(),
            message: "That is out of sight.".to_string(),
        }
        .to_bytes(),
    )
}

