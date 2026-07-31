//! Client packet handling and free-move loop.
//!
//! This module owns everything that reacts to **client input** — both during
//! playback (reject moves, respond from caches) and during the free-move
//! phase (validate moves via the shadow continuum, stream visible items as
//! the player walks).

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use log::{debug, info, trace, warn};

use u_core::Facing;
use network::error as fw_error;
use network::session::{Session, SessionEvent};
use packets::interaction::{
    DoubleClick, GetMobileStatus, PickUpItem, RejectMoveItem, RejectMoveItemReason, SingleClick,
};
use packets::movement::{MoveAck, MoveReject, MoveRequest, Notoriety, ResyncRequest};
use packets::speech::{SendSpeech, SpeechType};
use packets::status::StatusBarInfo;
use packets::system::ClientViewRange;
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

use framework::ecumene::{
    MovementValidator, StaticDataProvider,
};
use framework::diorama::{ObserverPipeline, CompositeTileProvider};

use crate::dot_commands::{DotCommands, Handled};
use framework::ecumene::Entity;
use crate::uo_engine::entity::DemoEntity;

use super::engine_rpc::ShadowTx;
use common::uo_engine::rpc::EngineProxy;
use common::uo_engine::handler::EngineCommand;

// ── Free-move result ─────────────────────────────────────────────────────

pub(super) enum FreeMoveResult {
    Disconnected,
    RestartReplay,
}

// ── Free-move loop ───────────────────────────────────────────────────────

pub(super) async fn run_free_move(
    client: &mut Session,
    observer: &mut ObserverPipeline,
    shadow_tx: &ShadowTx,
    house_cache: &HashMap<u32, Bytes>,
    static_data: Option<&Arc<dyn StaticDataProvider>>,
) -> fw_error::Result<FreeMoveResult> {
    let mut cmds = DotCommands::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));

    // Show the action menu gump immediately on entering free-move.
    cmds.send_action_menu_gump(observer.pos.serial, client).await?;

    loop {
        tokio::select! {
            event = client.recv() => {
                match event.event {
                    SessionEvent::Packet(p) => {
                        match cmds.handle_packet(&p, client, observer, shadow_tx).await? {
                            Handled::Yes | Handled::StopPlayback => continue,
                            Handled::ReshowActionMenu => {
                                cmds.send_action_menu_gump(observer.pos.serial, client).await?;
                                continue;
                            }
                            Handled::SeekPlayback(_)
                            | Handled::TogglePause
                            | Handled::StepPacket(_)
                            | Handled::StepClientPacket(_)
                            | Handled::StepServerPacket(_)
                            | Handled::FastForward(_) => continue, // ignored in free-move
                            Handled::RestartReplay => {
                                info!("[free-move] restart replay requested");
                                return Ok(FreeMoveResult::RestartReplay);
                            }
                            Handled::No => {}
                        }

                        let prev_xy = (observer.pos.x, observer.pos.y);
                        handle_client_packet(
                            client, p, true, observer,
                            shadow_tx, house_cache, static_data,
                        ).await?;

                        if (observer.pos.x, observer.pos.y) != prev_xy {
                            let world = observer.session.current_world;
                            let new_strips = observer.session.visible.update_view(observer.pos.x, observer.pos.y);

                            let mut count = 0usize;
                            for strip in &new_strips {
                                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
                                let items = engine.items_in_area(*strip).await;
                                for raw in items {
                                    observer.session.ingest_packet(&raw);
                                    client.send(RawPacket::s2c(raw)).await?;
                                    count += 1;
                                }
                            }
                            if count > 0 {
                                debug!(
                                    "[free-move] pos ({},{}) range={} — sent {} items from {} new strip(s)",
                                    observer.pos.x, observer.pos.y, observer.view_range(), count, new_strips.len()
                                );
                            }

                            // Sweep stale multi-objects after the server (shadow
                            // continuum) had a chance to confirm visible multis.
                            observer.session.sweep_stale();
                        }
                    }
                    SessionEvent::Stopped | SessionEvent::Disconnected => return Ok(FreeMoveResult::Disconnected),
                    SessionEvent::Error(e) => return Err(e.into()),
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                // NPC wandering disabled — mobiles stay in their last
                // recorded positions after replay ends.
            }
        }
    }
}

// ── Client input ──────────────────────────────────────────────────────────

pub(super) async fn handle_client_packet(
    client: &mut Session,
    packet: RawPacket,
    log_finished: bool,
    observer: &mut ObserverPipeline,
    shadow_tx: &ShadowTx,
    house_cache: &HashMap<u32, Bytes>,
    static_data: Option<&Arc<dyn StaticDataProvider>>,
) -> fw_error::Result<()> {
    // ── 0xBF sub 0x001E — RequestHouseState ─────────────────────────────
    // The client sends this when it sees a HouseRevisionState (0xBF:0x001D)
    // and doesn't have the design cached.  Respond from our house cache
    // regardless of playback/free-move mode.
    if packet.id() == 0xBF && packet.data.len() >= 9 {
        let sub = u16::from_be_bytes([packet.data[3], packet.data[4]]);
        if sub == 0x001E {
            let serial = u32::from_be_bytes([
                packet.data[5], packet.data[6], packet.data[7], packet.data[8],
            ]);
            if let Some(house_data) = house_cache.get(&serial) {
                info!(
                    "[client] 0xBF:001E RequestHouseState serial={:#010X} — responding from cache ({} bytes)",
                    serial, house_data.len(),
                );
                client.send(RawPacket::s2c(house_data.clone())).await?;
            } else {
                info!(
                    "[client] 0xBF:001E RequestHouseState serial={:#010X} — not in cache",
                    serial,
                );
            }
            return Ok(());
        }
    }

    // During replay (log_finished=false) only handle MoveRequest (reject it);
    // everything else is silently dropped — the log packets already contain
    // the correct server responses (e.g. names from SingleClick), and we
    // must not send duplicate responses.
    //
    // Exception: C→S 0xC8 ClientViewRange — the client may request a
    // different view range after SetMap (per-facet default).  We echo it
    // back and update the session so the visible area stays correct.
    if !log_finished && packet.id() != MoveRequest::ID {
        if packet.id() == ClientViewRange::ID {
            if let Ok(cvr) = ClientViewRange::from_bytes(&packet.data) {
                let old = observer.view_range();
                observer.session.visible.set_view_range(cvr.range as u16);
                client.send(RawPacket::s2c(packet.data.clone())).await?;
                info!(
                    "[client] C→S 0xC8 ClientViewRange during playback: range {} → {} (world={}) — echoed",
                    old, cvr.range, observer.session.current_world,
                );
            }
            return Ok(());
        }
        trace!(
            "[client] 0x{:02X} during replay — dropped (free-move only)",
            packet.id()
        );
        return Ok(());
    }

    if packet.id() == ClientViewRange::ID {
        if let Ok(cvr) = ClientViewRange::from_bytes(&packet.data) {
            observer.session.visible.set_view_range(cvr.range as u16);
            debug!("[client] 0xC8 ClientViewRange: range={}", cvr.range);
            client.send(RawPacket::s2c(packet.data.clone())).await?;
        }
        return Ok(());
    }

    if packet.id() == ResyncRequest::ID {
        debug!(
            "[client] 0x22 ResyncRequest — sending DrawGamePlayer ({},{},{}) facing={}",
            observer.pos.x, observer.pos.y, observer.pos.z, observer.pos.facing
        );
        if observer.pos.is_ready() {
            client
                .send(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                .await?;
        }
        let world = observer.session.current_world;
        let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
        let items = engine.items_in_area(*observer.view_rect()).await;
        if !items.is_empty() {
            debug!(
                "[client] resync — re-sending {} cached items (incl. multis)",
                items.len()
            );
            observer.session.visible.clear();
            observer.session.registry.clear_world(observer.session.current_world);
            for raw in items {
                observer.session.ingest_packet(&raw);
                client.send(RawPacket::s2c(raw)).await?;
            }
            // Re-ingest custom house data — clear_world wiped custom_defs.
            for (_serial, data) in house_cache {
                observer.session.ingest_packet(data);
            }
        }
        return Ok(());
    }

    if packet.id() == PickUpItem::ID {
        trace!("[client] 0x07 PickUpItem — rejecting (CannotLift)");
        let reject = RejectMoveItem::new(RejectMoveItemReason::CannotLift);
        client.send(RawPacket::s2c(reject.to_bytes())).await?;
        return Ok(());
    }

    // ── DoubleClick (0x06) — open container from continuum cache ───────────
    if packet.id() == DoubleClick::ID && log_finished {
        if let Ok(dc) = DoubleClick::from_bytes(&packet.data) {
            let world = observer.session.current_world;
            let is_paperdoll = dc.serial & 0x8000_0000 != 0;
            let clean_serial = dc.serial & 0x7FFF_FFFF;

            debug!(
                "[client] DoubleClick serial={:#010X}{} world={}",
                clean_serial,
                if is_paperdoll { " (paperdoll request)" } else { "" },
                world,
            );

            // Query the entity from the continuum to describe what was clicked.
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
            let entity = engine.get_entity(clean_serial).await;

            // Query the authoritative container store in the continuum.
            let engine_container = engine.get_container(clean_serial).await;

            // Check local session cache for validation.
            let local_container = observer.session.visible.containers().get(clean_serial);

            // Validate sync between continuum and session.
            if let (Some(engine), Some(local)) = (&engine_container, local_container) {
                if engine.item_count() != local.item_count() {
                    warn!(
                        "[desync] container {:#010X}: continuum has {} items, session has {}",
                        clean_serial, engine.item_count(), local.item_count(),
                    );
                }
            } else if engine_container.is_some() != local_container.is_some() {
                debug!(
                    "[client] container {:#010X}: continuum={}, session={}",
                    clean_serial,
                    if engine_container.is_some() { "present" } else { "absent" },
                    if local_container.is_some() { "present" } else { "absent" },
                );
            }

            // Use the continuum version as authoritative.
            if let Some(container) = engine_container {
                info!(
                    "[client] DoubleClick {:#010X} — opening container (gump={:#06X}, {} items)",
                    clean_serial, container.gump_model(), container.item_count(),
                );
                // Send 0x24 DrawContainer (open the gump).
                {
                    use packets::interaction::{DrawContainer, DrawContainerLegacy};
                    use packets::traits::BasicPacket;
                    let draw_pkt = DrawContainerLegacy {
                        id: DrawContainer::ID,
                        serial: container.serial(),
                        gump_model: container.gump_model(),
                    };
                    client
                        .send(RawPacket::s2c(draw_pkt.to_bytes()))
                        .await?;
                }
                // Send 0x3C ContainerContent (items inside).
                {
                    use packets::interaction::{ContainerContent, ContainerItemLegacy};
                    use packets::traits::ManualPacket;
                    let legacy_items: Vec<ContainerItemLegacy> = container.items.iter().map(|i| {
                        ContainerItemLegacy {
                            serial: i.serial,
                            graphic: i.graphic,
                            _pad0: (),
                            amount: i.amount,
                            x: i.x,
                            y: i.y,
                            container_serial: container.serial(),
                            color: i.color,
                        }
                    }).collect();
                    let content_pkt = ContainerContent::Legacy(legacy_items);
                    client
                        .send(RawPacket::s2c(content_pkt.to_bytes()))
                        .await?;
                }
            } else {
                // Not a container — describe what was clicked and send
                // an overhead message so the user gets feedback.
                let desc = match &entity {
                    Some(DemoEntity::Mobile(m)) => {
                        let label = if m.name.is_empty() {
                            format!("mob {:#06X}", m.graphic)
                        } else {
                            m.name.clone()
                        };
                        format!("{} ({:#010X})", label, m.serial)
                    }
                    Some(DemoEntity::Item { graphic, serial, .. }) => {
                        format!("item {:#06X} ({:#010X})", graphic, serial)
                    }
                    Some(DemoEntity::Multi { graphic, serial, .. }) => {
                        format!("multi {:#06X} ({:#010X})", graphic, serial)
                    }
                    None => format!("unknown object {:#010X}", clean_serial),
                };
                info!(
                    "[client] DoubleClick {:#010X} — not a container: {}",
                    clean_serial, desc,
                );

                // Send a system message to the client.
                let msg = SendSpeech {
                    serial: 0xFFFF_FFFF,
                    model: 0xFFFF,
                    speech_type: SpeechType::System,
                    color: 0x03B2,
                    font: 3,
                    name: String::new(),
                    message: format!("[replay] {} - no container data", desc),
                };
                client.send(RawPacket::s2c(msg.to_bytes())).await?;
            }
        }
        return Ok(());
    }

    // ── SingleClick (0x09) — show name overhead ─────────────────────────
    if packet.id() == SingleClick::ID {
        if let Ok(click) = SingleClick::from_bytes(&packet.data) {
            let world = observer.session.current_world;
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
            if let Some(entity) = engine.get_entity(click.serial).await {
                if let DemoEntity::Mobile(m) = &entity
                {
                    let label = if m.name.is_empty() {
                        format!("[mob 0x{:04X}]", m.graphic)
                    } else {
                        m.name.clone()
                    };
                    let color = if m.status.golden_health() {
                        0x035 // golden / yellow — invulnerable or special status
                    } else {
                        notoriety_hue(m.notoriety)
                    };
                    debug!("[client] SingleClick 0x{:08X} — name {:?}", m.serial, label);
                    let speech = SendSpeech {
                        serial: m.serial,
                        model: m.graphic,
                        speech_type: SpeechType::Normal,
                        color,
                        font: 3,
                        name: label.clone(),
                        message: label,
                    };
                    client.send(RawPacket::s2c(speech.to_bytes())).await?;
                }
            }
        }
        return Ok(());
    }

    // ── GetMobileStatus (0x34) — respond with StatusBarInfo (0x11) ───────
    if packet.id() == GetMobileStatus::ID {
        if let Ok(req) = GetMobileStatus::from_bytes(&packet.data) {
            let world = observer.session.current_world;
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
            if let Some(entity) = engine.get_entity(req.serial).await {
                if let DemoEntity::Mobile(m) = &entity
                {
                    let label = if m.name.is_empty() {
                        format!("[mob 0x{:04X}]", m.graphic)
                    } else {
                        m.name.clone()
                    };
                    debug!(
                        "[client] GetMobileStatus 0x{:08X} — name {:?}, hp {}/{}",
                        m.serial, label, m.hits, m.hits_max
                    );
                    let sbi = StatusBarInfo {
                        serial: m.serial,
                        name: packets::u_io::FixedString::new(&label),
                        hit_points: m.hits,
                        max_hit_points: m.hits_max,
                        name_change_flag: 0,
                        status_flag: 0,
                        is_female: None,
                        stats: None,
                        uoml: None,
                        uor: None,
                        aos: None,
                        uokr: None,
                    };
                    client.send(RawPacket::s2c(sbi.to_bytes())).await?;
                }
            }
        }
        return Ok(());
    }

    if packet.id() != MoveRequest::ID {
        trace!("[client] received packet 0x{:02X} — dropped", packet.id());
        return Ok(());
    }

    let Ok(req) = MoveRequest::from_bytes(&packet.data) else {
        return Ok(());
    };

    if log_finished {
        let facing = Facing::new(req.direction);
        let heading = facing.heading();
        let cur_heading = observer.pos.facing.heading();

        if heading != cur_heading {
            observer.pos.facing = facing;
            debug!(
                "[client] MoveRequest seq={} dir={:#04X} — turn at ({},{},{})",
                req.sequence, req.direction, observer.pos.x, observer.pos.y, observer.pos.z
            );
            let ack = MoveAck {
                id: MoveAck::ID,
                sequence: req.sequence,
                notoriety: Notoriety::Innocent,
            };
            client.send(RawPacket::s2c(ack.to_bytes())).await?;

            // Also tell the zone about the turn via MobileStep so the
            // entity direction stays in sync.
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
            let _ = engine.mobile_step(observer.pos.serial, facing).await;
        } else {
            let before = (observer.pos.x, observer.pos.y, observer.pos.z);
            let world = observer.session.current_world;
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);

            // ── Entity position check ─────────────────────────────────
            // Fetch the player entity from the zone to verify its
            // position matches PositionTracker.  A mismatch means the
            // zone is validating movement from the wrong origin.
            let zone_entity = engine.get_entity(observer.pos.serial).await;
            if let Some(ref ent) = zone_entity {
                let epos = Entity::pos(ent);
                if epos.x != observer.pos.x || epos.y != observer.pos.y || epos.z != observer.pos.z {
                    warn!(
                        "[desync] entity pos ({},{},{}) != tracker pos ({},{},{}) — \
                         teleporting entity to tracker pos",
                        epos.x, epos.y, epos.z, observer.pos.x, observer.pos.y, observer.pos.z,
                    );
                    engine.teleport(
                        observer.pos.serial, observer.pos.x, observer.pos.y, observer.pos.z,
                        None,
                    ).await;
                }
            }

            // ── Local validation (client-side provider) ─────────────
            let local_result = if let Some(sd) = static_data {
                let provider = CompositeTileProvider::new(
                    &**sd,
                    world,
                    &observer.session.visible,
                    &observer.session.registry,
                );
                MovementValidator::new(&provider).test_step(observer.pos.x, observer.pos.y, observer.pos.z, heading)
            } else {
                None
            };

            // ── Zone validation (shadow continuum via RPC) ──────────────
            let zone_result =
                engine.mobile_step(observer.pos.serial, facing).await;
            let zone_z = zone_result.map(|r| r.z);

            // ── Desync detection ─────────────────────────────────────
            if static_data.is_some() {
                match (local_result, zone_z) {
                    (Some(local_z), Some(engine_z)) if local_z != engine_z => {
                        warn!(
                            "[desync] Z mismatch at ({},{},{}): local={} zone={} dir={}",
                            observer.pos.x, observer.pos.y, observer.pos.z, local_z, engine_z, heading,
                        );
                    }
                    (Some(_), None) => {
                        warn!(
                            "[desync] local=passable zone=blocked at ({},{},{}) dir={}",
                            observer.pos.x, observer.pos.y, observer.pos.z, heading,
                        );
                    }
                    (None, Some(_)) => {
                        warn!(
                            "[desync] local=blocked zone=passable at ({},{},{}) dir={}",
                            observer.pos.x, observer.pos.y, observer.pos.z, heading,
                        );
                    }
                    _ => {} // both agree
                }
            }

            // Use the zone result as authoritative (it has the full
            // context including dynamic collision snapshot).
            match zone_z {
                Some(new_z) => {
                    observer.pos.step(facing);
                    observer.pos.z = new_z;
                    debug!(
                        "[client] MoveRequest seq={} {} — step ({},{},{}) → ({},{},{}){}",
                        req.sequence,
                        heading,
                        before.0,
                        before.1,
                        before.2,
                        observer.pos.x,
                        observer.pos.y,
                        observer.pos.z,
                        if new_z != before.2 {
                            format!(" (z adjusted: {} → {})", before.2, new_z)
                        } else {
                            String::new()
                        },
                    );
                    let ack = MoveAck {
                        id: MoveAck::ID,
                        sequence: req.sequence,
                        notoriety: Notoriety::Innocent,
                    };
                    client.send(RawPacket::s2c(ack.to_bytes())).await?;
                }
                None => {
                    debug!(
                        "[client] MoveRequest seq={} {} — BLOCKED at ({},{},{}) world={}",
                        req.sequence, heading, observer.pos.x, observer.pos.y, observer.pos.z, world
                    );
                    let reject = MoveReject {
                        id: MoveReject::ID,
                        sequence: req.sequence,
                        x: observer.pos.x,
                        y: observer.pos.y,
                        direction: observer.pos.facing.raw(),
                        z: observer.pos.z,
                    };
                    client.send(RawPacket::s2c(reject.to_bytes())).await?;
                    if observer.pos.is_ready() {
                        client
                            .send(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                            .await?;
                    }
                }
            }
        }
    } else {
        debug!(
            "[client] MoveRequest seq={} dir={:#04X} — rejecting, snapping to ({},{},{}) facing={}",
            req.sequence, req.direction, observer.pos.x, observer.pos.y, observer.pos.z, observer.pos.facing
        );
        let reject = MoveReject {
            id: MoveReject::ID,
            sequence: req.sequence,
            x: observer.pos.x,
            y: observer.pos.y,
            direction: observer.pos.facing.raw(),
            z: observer.pos.z,
        };
        client.send(RawPacket::s2c(reject.to_bytes())).await?;

        if observer.pos.is_ready() {
            client
                .send(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                .await?;
        } else {
            debug!("[client] MoveReject sent but serial=0, skipping DrawGamePlayer");
        }
    }

    Ok(())
}

// ── Notoriety → UO hue colour ────────────────────────────────────────────

/// Map a [`Notoriety`] value to the standard UO name-overhead hue.
fn notoriety_hue(n: Notoriety) -> u16 {
    match n {
        Notoriety::Innocent => 0x059,    // blue
        Notoriety::Ally => 0x043,        // green
        Notoriety::Attackable => 0x3B2,  // gray
        Notoriety::Criminal => 0x3B2,    // gray
        Notoriety::Enemy => 0x030,       // orange
        Notoriety::Murderer => 0x026,    // red
        Notoriety::Translucent => 0x3B2, // gray
        Notoriety::Invalid => 0x3B2,     // gray
        Notoriety::Unknown(_) => 0x3B2,  // gray
    }
}
