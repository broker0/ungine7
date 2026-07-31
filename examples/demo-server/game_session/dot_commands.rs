//! Dot-command handling: `.tele`, `.mtele`, `.remove`, `.where`, `.save`, `.load`, `.clear`.
//!
//! Commands require an appropriate [`AccessLevel`]:
//! - `.where` — available to all players
//! - `.tele`, `.mtele` — requires `Seer` or above
//! - `.remove`, `.world` — requires `GameMaster` or above
//! - `.save`, `.load`, `.clear` — requires `Administrator` or above

use log::info;

use protocol::RawPacket;
use packets::traits::{encode_packet, BasicPacket};

use network::error;
use network::session::Session;

use packets::character::DrawGamePlayer;
use packets::interaction::{DeleteObject, TargetCursor};
use packets::mobile_flags::MobileFlags;

use framework::continuum::WorkerCommand;
use framework::diorama::ObserverPipeline;

use common::dot_commands::{self as dot_cmd, CMD_CURSOR_BASE};
use common::uo_engine::auth::AccessLevel;
use common::uo_engine::handler::EngineCommand;

use crate::{DemoCommand, DemoWorkerTx};

use super::world_events::sync_zone_change;
use super::pending_cursor::PendingCursor;
use super::PlayerState;

// ── PendingTarget ─────────────────────────────────────────────────────────

/// Which command is waiting for a target-cursor response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingTarget {
    /// `.remove` — delete the targeted object from the world.
    Remove,
    /// `.tele` — teleport the player to the targeted location (once).
    Teleport,
    /// `.mtele` — teleport the player repeatedly until cancelled.
    MultiTeleport,
}

impl PendingTarget {
    pub(super) fn cursor_id(self) -> u32 {
        CMD_CURSOR_BASE
            | match self {
                Self::Remove => 0x01,
                Self::Teleport => 0x02,
                Self::MultiTeleport => 0x03,
            }
    }
}

// ── Main dispatch ─────────────────────────────────────────────────────────

/// Handle dot-commands and target-cursor responses.
///
/// Returns:
/// - `Ok(Some(true))` — packet consumed, caller should `continue`.
/// - `Ok(Some(false))` — impossible (reserved).
/// - `Ok(None)` — not a dot-command packet, caller processes normally.
pub(super) async fn handle_dot_commands(
    packet: &RawPacket,
    player: &mut Option<PlayerState>,
    pending_cursor: &mut Option<PendingCursor>,
    access_level: AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &crate::WorldData,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<framework::continuum::WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<std::sync::Arc<framework::continuum::WorldEvent>>,
) -> error::Result<Option<bool>> {
    // ── TargetCursor response (0x6C) ──────────────────────────────────
    if packet.id() == TargetCursor::ID {
        if let Ok(tc) = TargetCursor::from_bytes(&packet.data) {
            // Only intercept if the pending cursor is a DotCommand kind.
            if let Some(pc) = pending_cursor.as_ref() {
                if let super::pending_cursor::CursorKind::DotCommand(pending) = &pc.kind {
                    if tc.cursor_id == pending.cursor_id() {
                        let pending = *pending;
                        // Consume the pending cursor.
                        let _ = pending_cursor.take();
                        match pending {
                            PendingTarget::Remove => {
                                handle_remove_target(
                                    &tc, player, session, worker_tx,
                                ).await?;
                            }
                            PendingTarget::Teleport | PendingTarget::MultiTeleport => {
                                handle_teleport_target(
                                    pending, &tc, player, pending_cursor, session, worker_tx, observer,
                                ).await?;
                            }
                        }
                        return Ok(Some(true));
                    }
                }
                // `.spawner` placement carries a template string.
                if let super::pending_cursor::CursorKind::SpawnerPlacement { .. } = &pc.kind {
                    if tc.cursor_id == pc.cursor_id {
                        let template = if let Some(pc) = pending_cursor.take() {
                            match pc.kind {
                                super::pending_cursor::CursorKind::SpawnerPlacement { template } => template,
                                _ => return Ok(Some(true)),
                            }
                        } else {
                            return Ok(Some(true));
                        };
                        handle_spawner_target(&template, &tc, player, session, worker_tx).await?;
                        return Ok(Some(true));
                    }
                }
            }
        }
        // Let other TargetCursor packets pass through.
        return Ok(None);
    }

    // ── Speech → dispatch dot-command ─────────────────────────────────
    let text = match dot_cmd::extract_speech_text(packet) {
        Some(t) => t,
        None => return Ok(None),
    };
    let text = text.trim();
    if !text.starts_with('.') {
        return Ok(None);
    }

    let (name, _args) = text[1..].split_once(' ').unwrap_or((&text[1..], ""));

    match name.to_ascii_lowercase().as_str() {
        "remove" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            info!("[cmd] .remove — sending target cursor");
            dot_cmd::send_target_cursor(
                PendingTarget::Remove.cursor_id(), 0, session,
            ).await?;
            *pending_cursor = Some(PendingCursor::dot_command(PendingTarget::Remove));
        }
        "tele" => {
            if !require_level(access_level, AccessLevel::Seer, session).await? {
                return Ok(Some(true));
            }
            info!("[cmd] .tele — sending target cursor");
            dot_cmd::send_target_cursor(
                PendingTarget::Teleport.cursor_id(), 1, session,
            ).await?;
            *pending_cursor = Some(PendingCursor::dot_command(PendingTarget::Teleport));
        }
        "mtele" => {
            if !require_level(access_level, AccessLevel::Seer, session).await? {
                return Ok(Some(true));
            }
            info!("[cmd] .mtele — sending target cursor");
            dot_cmd::send_target_cursor(
                PendingTarget::MultiTeleport.cursor_id(), 1, session,
            ).await?;
            *pending_cursor = Some(PendingCursor::dot_command(PendingTarget::MultiTeleport));
        }
        "where" => {
            // Available to all players — no access check.
            if let Some(p) = player {
                let msg = format!("Position: ({},{},{}) world={}", p.x, p.y, p.z, p.world);
                dot_cmd::send_system_message(session, &msg).await?;
            } else {
                dot_cmd::send_system_message(session, "Not spawned yet").await?;
            }
        }
        "world" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            handle_world_switch(
                _args, player, access_level, session, worker_tx, world_data, observer,
                event_rx, event_tx_for_observer,
            ).await?;
        }
        "save" => {
            if !require_level(access_level, AccessLevel::Administrator, session).await? {
                return Ok(Some(true));
            }
            handle_save(player, _args, session, worker_tx, world_data).await?;
        }
        "load" => {
            if !require_level(access_level, AccessLevel::Administrator, session).await? {
                return Ok(Some(true));
            }
            handle_load(player, _args, access_level, session, worker_tx, world_data, observer).await?;
        }
        "clear" => {
            if !require_level(access_level, AccessLevel::Administrator, session).await? {
                return Ok(Some(true));
            }
            handle_clear(player, access_level, session, worker_tx, observer).await?;
        }
        "access" => {
            // Show current access level — available to everyone.
            let msg = format!("Access level: {}", access_level);
            dot_cmd::send_system_message(session, &msg).await?;
        }
        "res" => {
            // Resurrect yourself.  Items return automatically if you are near
            // your corpse (see engine `resurrect`).  Available to all players
            // for the demo; gate behind an access level for production.
            if let Some(p) = player {
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                let ok = engine.resurrect(p.serial).await;
                if !ok {
                    dot_cmd::send_system_message(
                        session, "You are not dead.",
                    ).await?;
                }
            } else {
                dot_cmd::send_system_message(session, "Not spawned yet").await?;
            }
        }
        "notoriety" | "noto" => {
            // Show your current reputation state — available to everyone.
            if let Some(p) = player {
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                if let Some(m) = engine.get_entity(p.serial).await.as_ref().and_then(|e| e.mobile()) {
                    let class = m.effective_notoriety_class();
                    let crim_left = m.criminal_until_ms
                        .saturating_sub(common::uo_engine::entity::MobileData::now_epoch_ms());
                    let msg = format!(
                        "Noto: {:?} | murders={} karma={} fame={} guild={:?} criminal_left={}s",
                        class, m.murders, m.karma, m.fame, m.guild_id, crim_left / 1000,
                    );
                    dot_cmd::send_system_message(session, &msg).await?;
                } else {
                    dot_cmd::send_system_message(session, "No mobile data").await?;
                }
            } else {
                dot_cmd::send_system_message(session, "Not spawned yet").await?;
            }
        }
        "murders" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            if let Some(p) = player {
                let n: u16 = _args.trim().parse().unwrap_or(0);
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                engine.set_reputation(p.serial, Some(n), None, None, None, None).await;
                dot_cmd::send_system_message(session, &format!("Murders set to {}", n)).await?;
            }
        }
        "karma" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            if let Some(p) = player {
                let v: i32 = _args.trim().parse().unwrap_or(0);
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                engine.set_reputation(p.serial, None, Some(v), None, None, None).await;
                dot_cmd::send_system_message(session, &format!("Karma set to {}", v)).await?;
            }
        }
        "fame" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            if let Some(p) = player {
                let v: i32 = _args.trim().parse().unwrap_or(0);
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                engine.set_reputation(p.serial, None, None, Some(v), None, None).await;
                dot_cmd::send_system_message(session, &format!("Fame set to {}", v)).await?;
            }
        }
        "guild" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            if let Some(p) = player {
                let arg = _args.trim();
                let gid = if arg.is_empty() || arg.eq_ignore_ascii_case("none") {
                    None
                } else {
                    arg.parse::<u32>().ok()
                };
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                engine.set_reputation(p.serial, None, None, None, Some(gid), None).await;
                // Keep the session's own viewer context in sync for ally checks.
                if let Some(ref mut ctx) = p.notoriety_ctx {
                    ctx.guild_id = gid;
                }
                dot_cmd::send_system_message(session, &format!("Guild set to {:?}", gid)).await?;
            }
        }
        "criminal" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            if let Some(p) = player {
                let on = !_args.trim().eq_ignore_ascii_case("off");
                let engine = crate::game_util::engine_for(worker_tx, p.world);
                engine.set_reputation(p.serial, None, None, None, None, Some(on)).await;
                dot_cmd::send_system_message(
                    session,
                    if on { "You are now a criminal." } else { "Criminal flag cleared." },
                ).await?;
            }
        }
        "vendor" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            handle_spawn_vendor(_args, player, session, worker_tx).await?;
        }
        "banker" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            handle_spawn_banker(player, session, worker_tx).await?;
        }
        "spawner" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            let template = _args.trim();
            if template.is_empty() {
                let names = crate::spawn_points::default_config().templates;
                let mut list: Vec<&String> = names.keys().collect();
                list.sort();
                let joined = list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                dot_cmd::send_system_message(
                    session,
                    &format!("Usage: .spawner <template>. Known: {}", joined),
                ).await?;
                return Ok(Some(true));
            }
            info!("[cmd] .spawner {} — sending target cursor", template);
            dot_cmd::send_target_cursor(
                PendingCursor::spawner_cursor_id(), 1, session,
            ).await?;
            *pending_cursor = Some(PendingCursor::spawner_placement(template.to_string()));
        }
        "session" => {
            if !require_level(access_level, AccessLevel::Administrator, session).await? {
                return Ok(Some(true));
            }
            handle_session_mode(_args, world_data, session).await?;
        }
        "maketele" => {
            if !require_level(access_level, AccessLevel::GameMaster, session).await? {
                return Ok(Some(true));
            }
            handle_make_teleporter(_args, player, session, worker_tx).await?;
        }
        _ => {
            // Not one of our commands — let it fall through to other
            // handlers (e.g. .lua).
            return Ok(None);
        }
    }

    Ok(Some(true))
}

// ── .world ────────────────────────────────────────────────────────────────

/// Check that `current` meets the `required` access level.
///
/// If the check fails, sends "Access denied." to the client and returns
/// `Ok(false)`.  Returns `Ok(true)` when the level is sufficient.
async fn require_level(
    current: AccessLevel,
    required: AccessLevel,
    session: &mut Session,
) -> error::Result<bool> {
    if current >= required {
        return Ok(true);
    }
    let msg = format!("Access denied (requires {}).", required);
    dot_cmd::send_system_message(session, &msg).await?;
    Ok(false)
}

// ── .session ────────────────────────────────────────────────────────────────

/// Change the default session mode applied to **new** connections.
///
/// Usage: `.session [rust|lua|controller]`
///
/// With no argument, reports the current default.  Changing the mode does
/// not affect already-running sessions — they keep their mode until they
/// reconnect.  The `lua` and `controller` modes are only available when the
/// server was built with the `lua` feature.
async fn handle_session_mode(
    args: &str,
    world_data: &crate::WorldData,
    session: &mut Session,
) -> error::Result<()> {
    use crate::game_session::SessionMode;

    let arg = args.trim();
    if arg.is_empty() {
        let msg = format!(
            "Default session mode for new connections: {}",
            world_data.session_mode(),
        );
        dot_cmd::send_system_message(session, &msg).await?;
        return Ok(());
    }

    match arg.parse::<SessionMode>() {
        Ok(mode) => {
            world_data.set_session_mode(mode);
            info!("[cmd] .session — default session mode set to {mode}");
            let msg = format!(
                "Default session mode for new connections set to {}. \
                 Existing sessions keep their current mode until reconnect.",
                mode,
            );
            dot_cmd::send_system_message(session, &msg).await?;
        }
        Err(e) => {
            dot_cmd::send_system_message(session, &format!("Cannot set session mode: {e}")).await?;
        }
    }
    Ok(())
}

// ── .vendor ────────────────────────────────────────────────────────────────

/// Spawn an NPC vendor of the given type next to the player.
///
/// Usage: `.vendor <mage|scribe|healer>`
async fn handle_spawn_vendor(
    args: &str,
    player: &Option<PlayerState>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    use common::uo_engine::entity::{DemoEntity, MobileData};
    use common::uo_engine::item_props::{ItemProps, MetaValue};
    use common::uo_engine::notoriety::NotorietyClass;
    use packets::mobile_flags::MobileFlags;
    use packets::movement::Notoriety;
    use u_core::Heading;

    use crate::vendor;

    let Some(p) = player else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let Some(vt) = vendor::parse_vendor_type(args) else {
        dot_cmd::send_system_message(
            session, "Usage: .vendor <mage|scribe|healer>",
        ).await?;
        return Ok(());
    };

    let engine = crate::game_util::engine_for(worker_tx, p.world);

    // Place the vendor one tile east of the player.
    let vx = p.x.wrapping_add(1);
    let vy = p.y;
    let vz = engine.resolve_z(vx, vy, p.z, Heading::South).await.unwrap_or(p.z);

    let npc_serial = engine.allocate_mobile_serial().await;
    if npc_serial == 0 {
        dot_cmd::send_system_message(session, "Serial space exhausted.").await?;
        return Ok(());
    }

    let name = vt.default_name().to_string();
    let npc = DemoEntity::Mobile(MobileData {
        serial: npc_serial,
        graphic: vt.body_graphic(),
        x: vx,
        y: vy,
        z: vz,
        direction: 0,
        color: 0,
        status: MobileFlags(0),
        notoriety: Notoriety::Innocent,
        items: Vec::new(),
        name: name.clone(),
        hits: 100,
        hits_max: 100,
        mana: 0,
        mana_max: 0,
        stamina: 100,
        stamina_max: 100,
        str_: 80,
        dex: 60,
        int: 60,
        is_player: false,
        dead: false,
        living_graphic: 0,
        noto_class: NotorietyClass::Innocent,
        ..Default::default()
    });

    engine.spawn_entity(npc_serial, npc).await;

    // Tag the NPC as a vendor (persisted in item props meta).
    let mut props = ItemProps::with_name(&name);
    props.set_meta("vendor_type", MetaValue::Str(vt.as_str().to_string()));
    engine.set_item_props(npc_serial, Some(props)).await;

    dot_cmd::send_system_message(
        session,
        &format!("Spawned {} (0x{:08X}). Say \"buy\" or \"sell\" near it.", name, npc_serial),
    ).await?;
    Ok(())
}

// ── .banker ───────────────────────────────────────────────────────────────

/// Spawn a banker NPC next to the player.
///
/// Usage: `.banker`
async fn handle_spawn_banker(
    player: &Option<PlayerState>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    use common::uo_engine::entity::{DemoEntity, MobileData};
    use common::uo_engine::item_props::{ItemProps, MetaValue};
    use common::uo_engine::notoriety::NotorietyClass;
    use packets::mobile_flags::MobileFlags;
    use packets::movement::Notoriety;
    use u_core::Heading;

    use crate::bank;

    let Some(p) = player else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let engine = crate::game_util::engine_for(worker_tx, p.world);

    // Place the banker one tile east of the player.
    let bx = p.x.wrapping_add(1);
    let by = p.y;
    let bz = engine.resolve_z(bx, by, p.z, Heading::South).await.unwrap_or(p.z);

    let npc_serial = engine.allocate_mobile_serial().await;
    if npc_serial == 0 {
        dot_cmd::send_system_message(session, "Serial space exhausted.").await?;
        return Ok(());
    }

    let name = bank::BANKER_NAME.to_string();
    let npc = DemoEntity::Mobile(MobileData {
        serial: npc_serial,
        graphic: bank::banker_body_graphic(),
        x: bx,
        y: by,
        z: bz,
        direction: 0,
        color: 0,
        status: MobileFlags(0),
        notoriety: Notoriety::Innocent,
        items: Vec::new(),
        name: name.clone(),
        hits: 100,
        hits_max: 100,
        mana: 0,
        mana_max: 0,
        stamina: 100,
        stamina_max: 100,
        str_: 80,
        dex: 60,
        int: 60,
        is_player: false,
        dead: false,
        living_graphic: 0,
        noto_class: NotorietyClass::Innocent,
        ..Default::default()
    });

    engine.spawn_entity(npc_serial, npc).await;

    // Tag the NPC as a banker (persisted in item props meta).
    let mut props = ItemProps::with_name(&name);
    props.set_meta(bank::META_NPC_TYPE, MetaValue::Str(bank::META_BANKER.to_string()));
    engine.set_item_props(npc_serial, Some(props)).await;

    // Attach a WanderController so the banker wanders in a small area.
    let controller = Box::new(crate::controller_registry::WanderController::with_radius(
        std::time::Duration::from_secs(5),
        4,
    ));
    let _ = worker_tx.send(framework::continuum::WorkerCommand::MapCommand(
        p.world,
        crate::DemoCommand::AttachControllerPersist {
            serial: npc_serial,
            controller,
            controller_id: crate::controller_registry::controller_id("wander", "5,r=4"),
        },
    )).await;

    dot_cmd::send_system_message(
        session,
        &format!("Spawned {} (0x{:08X}). Say \"bank\" near it.", name, npc_serial),
    ).await?;
    Ok(())
}

// ── .maketele ───────────────────────────────────────────────────────────────

/// Place a teleporter object at the player's current tile.
///
/// Usage: `.maketele <world> <x> <y> <z> [filter]`
///
/// Stepping onto the placed object moves a mobile to `(world, x, y, z)` —
/// cross-world when `world` differs from the current map.  The object is an
/// ordinary item carrying the `teleport_*` meta keys plus a persistent
/// `teleporter` controller, so it survives `.save`/`.load` and is detected
/// server-side via the engine's step-on trigger for *any* mobile.
///
/// The optional `filter` is one of `players` (default), `all`, `no_pets`.
async fn handle_make_teleporter(
    args: &str,
    player: &Option<PlayerState>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    use common::uo_engine::entity::DemoEntity;
    use common::uo_engine::item_props::{ItemProps, MetaValue};
    use crate::teleporters::{self, TeleportDest};

    let Some(p) = player.as_ref() else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let parts: Vec<&str> = args.split_whitespace().collect();
    let parsed = (|| {
        let world: u8 = parts.first()?.parse().ok()?;
        let x: u16 = parts.get(1)?.parse().ok()?;
        let y: u16 = parts.get(2)?.parse().ok()?;
        let z: i8 = parts.get(3)?.parse().ok()?;
        Some(TeleportDest { world, x, y, z })
    })();
    let Some(dest) = parsed else {
        dot_cmd::send_system_message(
            session,
            "Usage: .maketele <world> <x> <y> <z> [players|all|no_pets]",
        ).await?;
        return Ok(());
    };
    let filter = parts.get(4).copied();

    let engine = crate::game_util::engine_for(worker_tx, p.world);

    let serial = engine.allocate_serial().await;
    if serial == 0 {
        dot_cmd::send_system_message(session, "Serial space exhausted.").await?;
        return Ok(());
    }

    let entity = DemoEntity::Item {
        serial,
        graphic: teleporters::TELEPORTER_GRAPHIC,
        color: 0,
        amount: 1,
        x: p.x,
        y: p.y,
        z: p.z,
        is_container: false,
        hidden: false,
        facing: None,
    };
    engine.spawn_entity(serial, entity).await;

    let mut props = teleporters::write_dest(ItemProps::with_name("a teleporter"), dest);
    if let Some(f) = filter {
        props.set_meta(teleporters::META_TP_FILTER, MetaValue::Str(f.to_string()));
    }
    engine.set_item_props(serial, Some(props)).await;

    // Attach the teleporter controller (persisted in item props meta) so the
    // engine's step-on trigger drives the teleport server-side for any mobile.
    let controller = Box::new(crate::controller_registry::TeleporterController::new());
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        p.world,
        DemoCommand::AttachControllerPersist {
            serial,
            controller,
            controller_id: "teleporter".to_string(),
        },
    )).await;

    info!(
        "[cmd] .maketele — placed teleporter 0x{:08X} at ({},{},{}) on world {} → ({},{},{}) world {}",
        serial, p.x, p.y, p.z, p.world, dest.x, dest.y, dest.z, dest.world,
    );
    dot_cmd::send_system_message(
        session,
        &format!(
            "Placed teleporter 0x{:08X} at ({},{},{}) → world {} ({},{},{}).",
            serial, p.x, p.y, p.z, dest.world, dest.x, dest.y, dest.z,
        ),
    ).await?;
    Ok(())
}

async fn handle_world_switch(
    args: &str,
    player: &mut Option<PlayerState>,
    access_level: AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &crate::WorldData,
    observer: &mut Option<ObserverPipeline>,
    event_rx: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<framework::continuum::WorldEvent>>,
    event_tx_for_observer: &tokio::sync::mpsc::Sender<std::sync::Arc<framework::continuum::WorldEvent>>,
) -> error::Result<()> {
    let Some(p) = player.as_mut() else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let world_id: u8 = match args.trim().parse() {
        Ok(w) if w <= 5 => w,
        _ => {
            let msg = format!("Usage: .world <0-5> (current: {})", p.world);
            dot_cmd::send_system_message(session, &msg).await?;
            return Ok(());
        }
    };

    if world_id == p.world {
        let msg = format!("Already on world {world_id}");
        dot_cmd::send_system_message(session, &msg).await?;
        return Ok(());
    }

    // Use the atomic cross-zone transfer.
    super::transfer::transfer_player(
        session,
        p,
        access_level,
        worker_tx,
        world_data,
        observer,
        event_rx,
        event_tx_for_observer,
        world_id,
        p.x,
        p.y,
        p.z,
    )
    .await?;

    let msg = format!("Switched to world {world_id}");
    dot_cmd::send_system_message(session, &msg).await?;

    Ok(())
}

// ── .save ─────────────────────────────────────────────────────────────────

async fn handle_save(
    player: &mut Option<PlayerState>,
    args: &str,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &crate::WorldData,
) -> error::Result<()> {
    let Some(p) = player else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let path = if args.is_empty() { "world_save.json" } else { args };
    info!("[cmd] .save — saving all zones to {}", path);

    // Snapshot every map id that has live entities.  Worlds are 0..=5 (the
    // `.world` command's valid range); `SaveSnapshot` auto-creates a zone if
    // one didn't exist, so empty zones are filtered out to avoid polluting
    // the save file with phantom maps.
    // Also include the offline-character storage zone (LOGOUT_STORAGE_MAP =
    // 0xFE) so that characters who are sitting there (timer fired) survive a
    // server restart / `.save` + `--load` cycle.
    let mut zones = Vec::new();
    let map_ids_to_save: Vec<u8> = (0u8..=5)
        .chain(std::iter::once(crate::logout::LOGOUT_STORAGE_MAP))
        .collect();
    for world_id in map_ids_to_save {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let sent = worker_tx
            .send(WorkerCommand::MapCommand(
                world_id,
                DemoCommand::Engine(EngineCommand::SaveSnapshot { reply: reply_tx }),
            ))
            .await;
        if sent.is_err() {
            continue;
        }
        if let Ok(zone_data) = reply_rx.await {
            // Keep the player's own zone even if otherwise empty, plus any
            // zone that actually contains entities.
            if world_id == p.world || !zone_data.entities.is_empty() {
                zones.push(zone_data);
            }
        }
    }

    let world_save = common::uo_engine::snapshot::WorldSaveData {
        zones,
        player_serial: p.serial,
        player_world: p.world,
    };

    match common::uo_engine::snapshot::save_to_file(
        &world_save,
        std::path::Path::new(path),
    ) {
        Ok(()) => {
            let zone_count = world_save.zones.len();
            // Also persist the per-account character map (names / world).
            crate::game_util::persist_accounts(world_data).await;
            dot_cmd::send_system_message(
                session,
                &format!(
                    "World saved to {} ({} zone(s)); accounts saved to {}",
                    path, zone_count, crate::game_util::ACCOUNTS_SAVE_PATH,
                ),
            ).await?;
            info!("[cmd] .save — done ({} zones)", zone_count);
        }
        Err(e) => {
            dot_cmd::send_system_message(session, &format!("Save failed: {}", e)).await?;
            log::error!("[cmd] .save — {}", e);
        }
    }

    Ok(())
}

// ── .load ─────────────────────────────────────────────────────────────────

async fn handle_load(
    player: &mut Option<PlayerState>,
    args: &str,
    access_level: AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    world_data: &crate::WorldData,
    observer: &mut Option<ObserverPipeline>,
) -> error::Result<()> {
    let Some(p) = player else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let path = if args.is_empty() { "world_save.json" } else { args };
    info!("[cmd] .load — loading all zones from {}", path);

    // Read the entire world save (all zones).
    let world_save = match common::uo_engine::snapshot::load_from_file(std::path::Path::new(path)) {
        Ok(ws) => ws,
        Err(e) => {
            dot_cmd::send_system_message(session, &format!("Load failed: {}", e)).await?;
            log::error!("[cmd] .load — {}", e);
            return Ok(());
        }
    };

    // Collect currently visible entity serials in the player's zone before
    // the restore wipes it, and save the player entity itself.
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let old_visible = engine.query_area(p.view_rect).await;
    let old_serials: Vec<u32> = old_visible.iter()
        .map(|e| framework::ecumene::Entity::serial(e))
        .collect();
    let saved_player = engine.get_entity(p.serial).await;

    // Dispatch RestoreSnapshot for every zone in the save file.
    //
    // `reset_alloc: false` on all zones — the serial allocator is shared with
    // every live session on the server, so a full reset would forget the
    // serials of other connected players.  RestoreSnapshot with reset_alloc
    // false still calls mark_occupied for all restored serials, so future
    // allocations remain collision-free.
    let mut total_entities: usize = 0;
    let mut total_containers: usize = 0;
    let mut restored_zones: Vec<u8> = Vec::new();
    for zone_data in world_save.zones {
        let map_id = zone_data.map_id;
        total_entities += zone_data.entities.len();
        total_containers += zone_data.containers.len();
        restored_zones.push(map_id);
        let _ = worker_tx
            .send(WorkerCommand::MapCommand(
                map_id,
                DemoCommand::Engine(EngineCommand::RestoreSnapshot {
                    data: zone_data,
                    reset_alloc: false,
                    crash_recovery: false,
                }),
            ))
            .await;
    }

    // Re-spawn the player in their zone if that zone was part of the restore
    // (otherwise it was not wiped and the player entity is still there).
    if restored_zones.contains(&p.world) {
        if let Some(entity) = saved_player {
            let _ = worker_tx
                .send(WorkerCommand::MapCommand(
                    p.world,
                    DemoCommand::Engine(EngineCommand::SpawnEntity {
                        entity_id: p.serial,
                        data: entity,
                    }),
                ))
                .await;
        }
    }

    // Sync the client view for the player's current zone.
    sync_zone_change(p, &old_serials, access_level, session, worker_tx, observer).await?;

    // Reload per-account characters from disk so the character-selection
    // screen stays consistent with the restored world.
    let accounts_path = std::path::Path::new(crate::game_util::ACCOUNTS_SAVE_PATH);
    if accounts_path.exists() {
        match common::uo_engine::snapshot::load_accounts_from_file(accounts_path) {
            Ok(map) => {
                let chars: usize = map.values().map(|v| v.len()).sum();
                *world_data.account_characters.write().await = map;
                log::info!(
                    "[cmd] .load — reloaded {} character(s) from {}",
                    chars, crate::game_util::ACCOUNTS_SAVE_PATH,
                );
            }
            Err(e) => {
                log::warn!(
                    "[cmd] .load — could not reload {}: {}",
                    crate::game_util::ACCOUNTS_SAVE_PATH, e,
                );
            }
        }
    }

    let zone_list: Vec<String> = restored_zones.iter().map(|id| id.to_string()).collect();
    let msg = format!(
        "Loaded {} zone(s) [{}], {} entities, {} containers from {}",
        restored_zones.len(),
        zone_list.join(", "),
        total_entities,
        total_containers,
        path,
    );
    dot_cmd::send_system_message(session, &msg).await?;
    info!(
        "[cmd] .load — done ({} zone(s), {} entities)",
        restored_zones.len(), total_entities,
    );

    Ok(())
}

// ── .clear ────────────────────────────────────────────────────────────────

async fn handle_clear(
    player: &mut Option<PlayerState>,
    access_level: AccessLevel,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    observer: &mut Option<ObserverPipeline>,
) -> error::Result<()> {
    let Some(p) = player else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    info!("[cmd] .clear — clearing zone {}", p.world);
    // Collect currently visible entity serials before the wipe.
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let old_visible = engine.query_area(p.view_rect).await;
    let old_serials: Vec<u32> = old_visible.iter()
        .map(|e| framework::ecumene::Entity::serial(e))
        .collect();
    // Save the player entity before the zone is wiped.
    let saved_player = engine.get_entity(p.serial).await;
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            p.world,
            DemoCommand::Engine(EngineCommand::ResetZone {
                entities: Vec::new(),
                containers: framework::continuum::HashContainerStore::new(),
            }),
        ))
        .await;
    // Re-spawn the player so they remain functional.
    if let Some(entity) = saved_player {
        let _ = worker_tx
            .send(WorkerCommand::MapCommand(
                p.world,
                DemoCommand::Engine(EngineCommand::SpawnEntity {
                    entity_id: p.serial,
                    data: entity,
                }),
            ))
            .await;
    }
    // Sync the client: delete old entities (no new ones after clear).
    sync_zone_change(p, &old_serials, access_level, session, worker_tx, observer).await?;
    dot_cmd::send_system_message(session, "Zone cleared").await?;
    info!("[cmd] .clear — done");

    Ok(())
}

// ── Remove target handling ────────────────────────────────────────────────

/// Handle a `.remove` target-cursor response.
///
/// Removes the targeted entity from the engine zone and sends `DeleteObject`
/// to the client.
async fn handle_remove_target(
    tc: &TargetCursor,
    player: &mut Option<PlayerState>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let serial = tc.target_serial;
    if dot_cmd::is_target_cancelled(tc) || serial == 0 {
        info!("[cmd] .remove target cancelled");
        return Ok(());
    }

    let p = match player.as_ref() {
        Some(p) => p,
        None => return Ok(()),
    };

    info!(
        "[cmd] .remove target serial={:#010X} at ({},{},{})",
        serial, tc.x, tc.y, tc.z
    );

    // Remove entity from engine zone.
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            p.world,
            DemoCommand::Engine(EngineCommand::RemoveEntity {
                entity_id: serial,
            }),
        ))
        .await;

    // Send DeleteObject to client.
    let del = DeleteObject {
        id: DeleteObject::ID,
        serial,
    };
    session.send(RawPacket::s2c(encode_packet(&del))).await?;

    let msg = format!("Removed object {:#010X}", serial);
    dot_cmd::send_system_message(session, &msg).await?;

    Ok(())
}

// ── .spawner target handling ───────────────────────────────────────────────

/// Handle a `.spawner` target-cursor response: place a hidden spawner object
/// at the clicked tile, tag it with default spawner meta, and attach the
/// `SpawnerController` (persisted so it survives `.save`/`.load`).
async fn handle_spawner_target(
    template: &str,
    tc: &TargetCursor,
    player: &Option<PlayerState>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    use common::uo_engine::entity::DemoEntity;
    use crate::spawner_object;

    if dot_cmd::is_target_cancelled(tc) {
        info!("[cmd] .spawner target cancelled");
        return Ok(());
    }

    let Some(p) = player.as_ref() else {
        dot_cmd::send_system_message(session, "Not spawned yet").await?;
        return Ok(());
    };

    let engine = crate::game_util::engine_for(worker_tx, p.world);

    let serial = engine.allocate_serial().await;
    if serial == 0 {
        dot_cmd::send_system_message(session, "Serial space exhausted.").await?;
        return Ok(());
    }

    let entity = DemoEntity::Item {
        serial,
        graphic: spawner_object::SPAWNER_GRAPHIC,
        color: 0,
        amount: 1,
        x: tc.x,
        y: tc.y,
        z: tc.z,
        is_container: false,
        hidden: true,
        facing: None,
    };
    engine.spawn_entity(serial, entity).await;

    // Default parameters + controller id (persisted in item props meta).
    let props = spawner_object::default_props(template);
    engine.set_item_props(serial, Some(props)).await;

    // Attach the UI controller (handler wires host.attach).
    let controller = Box::new(spawner_object::SpawnerController::new());
    let _ = worker_tx.send(WorkerCommand::MapCommand(
        p.world,
        DemoCommand::AttachControllerPersist {
            serial,
            controller,
            controller_id: "spawner".to_string(),
        },
    )).await;

    let known = crate::spawn_points::default_config().templates.contains_key(template);
    let note = if known { "" } else { " (warning: unknown template — spawns nothing)" };
    dot_cmd::send_system_message(
        session,
        &format!(
            "Placed spawner 0x{:08X} ({}) at ({},{},{}).{}",
            serial, template, tc.x, tc.y, tc.z, note,
        ),
    ).await?;

    Ok(())
}

// ── Teleport target handling ──────────────────────────────────────────────

/// Handle a teleport target-cursor response.
///
/// Teleports the player to the clicked location via `TeleportEntity`,
/// sends `DrawGamePlayer` to update the client, and refreshes the view.
async fn handle_teleport_target(
    target: PendingTarget,
    tc: &TargetCursor,
    player: &mut Option<PlayerState>,
    pending_cursor: &mut Option<PendingCursor>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
    observer: &mut Option<ObserverPipeline>,
) -> error::Result<()> {
    // Cancelled by client.
    if dot_cmd::is_target_cancelled(tc) {
        info!("[cmd] .{:?} target cancelled", target);
        return Ok(());
    }

    let p = match player.as_mut() {
        Some(p) => p,
        None => return Ok(()),
    };

    let (old_x, old_y, old_z) = (p.x, p.y, p.z);
    let (new_x, new_y, new_z) = (tc.x, tc.y, tc.z);

    info!(
        "[cmd] .{:?} ({},{},{}) → ({},{},{})",
        target, old_x, old_y, old_z, new_x, new_y, new_z
    );

    // Teleport entity in the engine.
    let _ = worker_tx
        .send(WorkerCommand::MapCommand(
            p.world,
            DemoCommand::Engine(EngineCommand::TeleportEntity {
                serial: p.serial,
                x: new_x,
                y: new_y,
                z: new_z,
                direction: None,
            }),
        ))
        .await;

    // Update local player state.
    p.x = new_x;
    p.y = new_y;
    p.z = new_z;

    // Send DrawGamePlayer so the client updates its position.
    let engine = crate::game_util::engine_for(worker_tx, p.world);
    let entity = engine.get_entity(p.serial).await;
    let (graphic, color) = match entity.as_ref().and_then(|e| e.mobile()) {
        Some(m) => (m.graphic, m.color),
        _ => (crate::constants::body::MALE_HUMAN, 0),
    };

    let dgp = DrawGamePlayer {
        id: 0x20,
        serial: p.serial,
        body_type: graphic,
        _pad0: (),
        hue: color,
        flags: MobileFlags(0),
        x: new_x,
        y: new_y,
        _pad1: (),
        direction: p.direction,
        z: new_z,
    };
    let pkt = RawPacket::s2c(encode_packet(&dgp));
    if let Some(obs) = observer {
        obs.ingest_s2c(&pkt.data);
    }
    session.send(pkt).await?;

    // System message with new coordinates.
    let msg = format!("Teleported to ({},{},{})", new_x, new_y, new_z);
    dot_cmd::send_system_message(session, &msg).await?;

    // For multi-teleport, send another target cursor to continue the chain.
    if target == PendingTarget::MultiTeleport {
        dot_cmd::send_target_cursor(
            PendingTarget::MultiTeleport.cursor_id(), 1, session,
        ).await?;
        *pending_cursor = Some(PendingCursor::dot_command(PendingTarget::MultiTeleport));
    }

    Ok(())
}
