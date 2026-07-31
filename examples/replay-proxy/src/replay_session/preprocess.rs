//! Pre-processing of `.uolog` files into timed replay entries.
//!
//! The [`preprocess`] function reads the raw log, drives a [`LogPlayer`]
//! through every entry, and produces a compact timeline of
//! [`ReplayEntry`](super::ReplayEntry) values ready for playback.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use log::{debug, info, trace, warn};

use u_core::PacketDirection;
use framework::continuum::container::ContainerStore;
use framework::diorama::DrainReason;
use framework::ecumene::StaticWorldData;
use packets::character::CharacterLocaleAndBody;
use packets::movement::{MoveAck, MoveReject, MoveRequest};
use packets::traits::BasicPacket;

use crate::log_player::{LogPlayer, LogPlayerSnapshot};
use crate::packet_log::LogEntry;

use super::{EntryKind, ReplayEntry};

/// When `true`, MoveAck replay entries use the timestamp of the matching
/// C→S MoveRequest instead of the S→C MoveAck.  This makes the synthesised
/// DrawGamePlayer (0x20) arrive at the client at the moment of the original
/// move request rather than when the server acknowledged it.
///
/// **To revert to the old (server-ack) timing, set this to `false`.**
const MOVE_ACK_USE_REQUEST_TIMING: bool = true;

/// Extra microseconds added to the MoveRequest timestamp when
/// `MOVE_ACK_USE_REQUEST_TIMING` is enabled.  Use 0 for "instant" dispatch
/// at the request moment, or a small value (e.g. 5000–10000) to simulate minimal
/// round-trip time.
const MOVE_ACK_TIMING_OFFSET_US: u64 = 0;

/// Result of preprocessing a `.uolog` file.
///
/// Returns:
/// - replay entries starting from the first `0x78` of our character,
/// - player position at that origin point,
/// - character name extracted from `0xA9`,
/// - list of world entities to populate the ZoneWorker.
pub fn preprocess(
    log: &[LogEntry],
    static_data: Option<Arc<StaticWorldData>>,
) -> (
    Vec<ReplayEntry>,           // timed world entries starting from our 0x78
    Vec<Bytes>,                 // init packets (everything S→C up to and including our 0x78)
    LogPlayer,                  // log player at end-of-log (pos, session, entity_maps, etc.)
    Vec<LogPlayerSnapshot>,     // periodic snapshots for fast backward seeks
    HashMap<u32, Bytes>,        // custom house cache: house_serial → raw 0xD8 packet bytes
    ContainerStore,             // container inventory: container_serial → ContainerInfo
) {
    use crate::log_player::{Phase, SNAPSHOT_INTERVAL_US};

    let preprocess_start = std::time::Instant::now();

    let mut player = LogPlayer::new(static_data);
    let mut entries: Vec<ReplayEntry> = Vec::new();
    let mut init_packets: Vec<Bytes> = Vec::new();
    let mut world_base_offset_us: u64 = 0;
    let mut in_world = false;

    // Snapshot state for fast backward seeks.
    let mut snapshots: Vec<LogPlayerSnapshot> = Vec::new();
    let mut next_snapshot_us: u64 = SNAPSHOT_INTERVAL_US;

    // Stats counters.
    let mut stats_c2s: usize = 0;
    let mut stats_s2c: usize = 0;
    let mut stats_forward: usize = 0;
    let mut stats_world_init: usize = 0;
    let mut stats_move_ack: usize = 0;
    let mut stats_skipped_gumps: usize = 0;
    let mut stats_skipped_target: usize = 0;
    let mut stats_move_ack_lost: usize = 0;
    let mut stats_move_reject: usize = 0;
    // Per-drain-reason counters for lost MoveAck steps.
    let mut stats_lost_by_draw_game_player: usize = 0;
    let mut stats_lost_by_move_reject: usize = 0;
    let mut stats_lost_by_move_ack_desync: usize = 0;
    let mut stats_lost_by_set_map: usize = 0;
    let mut stats_lost_by_other: usize = 0;

    // Custom house cache: house_serial → raw 0xD8 packet bytes.
    // Populated from S→C 0xD8 packets in the log so we can respond to
    // client requests (0xBF sub 0x001E) during replay.
    let mut house_cache: HashMap<u32, Bytes> = HashMap::new();

    // MoveRequest sequence → us_offset map for request-timing mode.
    // Only populated when MOVE_ACK_USE_REQUEST_TIMING is enabled.
    let mut move_request_timestamps: HashMap<u8, u64> = HashMap::new();

    // Container inventory: container_serial → ContainerInfo.
    // Populated from S→C 0x24 (DrawContainer), 0x3C (ContainerContent),
    // and 0x25 (AddItemToContainer) packets in the log.
    let mut container_store = ContainerStore::new();

    for (log_idx, e) in log.iter().enumerate() {
        // Let LogPlayer process the entry first (updates pos, session,
        // entity_maps, pending_moves, phase).
        let phase_before = player.phase.clone();
        player.advance_to(log, log_idx, &[]);

        match e.direction {
            PacketDirection::ClientToServer => {
                if e.data.is_empty() { continue; }
                stats_c2s += 1;
                if e.data[0] == MoveRequest::ID {
                    if MOVE_ACK_USE_REQUEST_TIMING {
                        if let Ok(req) = MoveRequest::from_bytes(&e.data) {
                            move_request_timestamps.insert(req.sequence, e.us_offset);
                        }
                    }
                    trace!(
                        "[preprocess] C→S MoveRequest seq={} dir={}",
                        e.data.get(1).copied().unwrap_or(0),
                        e.data.get(2).copied().unwrap_or(0),
                    );
                }
                // Log C→S 0xBF sub 0x001E (RequestHouseState) for diagnostics.
                if e.data[0] == 0xBF && e.data.len() >= 9 {
                    let sub = u16::from_be_bytes([e.data[3], e.data[4]]);
                    if sub == 0x001E {
                        let serial = u32::from_be_bytes([e.data[5], e.data[6], e.data[7], e.data[8]]);
                        debug!(
                            "[preprocess] C→S 0xBF:001E RequestHouseState serial={:#010X} @{}µs",
                            serial, e.us_offset,
                        );
                    }
                }
                // In World phase: store C→S for packet-stepping.
                if in_world {
                    entries.push(ReplayEntry {
                        us_offset: e.us_offset.saturating_sub(world_base_offset_us),
                        log_idx,
                        kind: EntryKind::ClientPacket(Bytes::copy_from_slice(&e.data)),
                    });
                }
            }

            PacketDirection::ServerToClient => {
                if e.data.is_empty() { continue; }
                stats_s2c += 1;

                // Cache 0xD8 SendCustomHouse packets (keyed by house serial)
                // so we can serve them when the client requests house state
                // (0xBF sub 0x001E) during replay.
                if e.data[0] == 0xD8 && e.data.len() >= 9 {
                    let serial = u32::from_be_bytes([
                        e.data[5], e.data[6], e.data[7], e.data[8],
                    ]);
                    debug!(
                        "[preprocess] S→C 0xD8 SendCustomHouse serial={:#010X} ({} bytes) @{}µs",
                        serial, e.data.len(), e.us_offset,
                    );
                    house_cache.insert(serial, Bytes::copy_from_slice(&e.data));
                }

                // Log 0xBF sub 0x001D (HouseRevisionState) for diagnostics.
                if e.data[0] == 0xBF && e.data.len() >= 13 {
                    let sub = u16::from_be_bytes([e.data[3], e.data[4]]);
                    if sub == 0x001D {
                        let serial = u32::from_be_bytes([e.data[5], e.data[6], e.data[7], e.data[8]]);
                        let revision = u32::from_be_bytes([e.data[9], e.data[10], e.data[11], e.data[12]]);
                        debug!(
                            "[preprocess] S→C 0xBF:001D HouseRevisionState serial={:#010X} revision={} @{}µs (cached: {})",
                            serial, revision, e.us_offset, house_cache.contains_key(&serial),
                        );
                    }
                }

                match phase_before {
                    Phase::PreWorld => {
                        if e.data[0] == CharacterLocaleAndBody::ID {
                            debug!(
                                "[preprocess] 0x1B — player serial={:#010X}",
                                player.player_serial
                            );
                            init_packets.push(Bytes::copy_from_slice(&e.data));
                        } else if e.data[0] == 0xA9 {
                            trace!("[preprocess] S→C 0xA9 CharacterList — skipped");
                        }
                        // Other pre-world S→C: state applied by LogPlayer, no recording.
                    }

                    Phase::PreOrigin => {
                        // Collect as init packet to send to client at startup.
                        init_packets.push(Bytes::copy_from_slice(&e.data));

                        // Check if LogPlayer just transitioned to World phase.
                        if player.phase == Phase::World && !in_world {
                            world_base_offset_us = e.us_offset;
                            in_world = true;
                            debug!(
                                "[preprocess] 0x78 for player {:#010X} — origin at {}µs",
                                player.player_serial, world_base_offset_us
                            );
                            // This packet is also the first timed entry at us_offset=0.
                            entries.push(ReplayEntry {
                                us_offset: 0,
                                log_idx,
                                kind: EntryKind::WorldInit(
                                    Bytes::copy_from_slice(&e.data)
                                ),
                            });
                        }
                    }

                    Phase::World => {
                        if e.data[0] == MoveAck::ID {
                            // LogPlayer already processed this MoveAck.
                            // Check the explicit flag to know if the step
                            // was accepted (seq matched head of queue).
                            if player.observer.last_move_accepted {
                                let direction = player.observer.pos.facing;

                                // Determine the replay timestamp for this
                                // move step.  When request-timing mode is
                                // enabled, use the C→S MoveRequest timestamp
                                // (+ configurable offset) instead of the
                                // S→C MoveAck timestamp.
                                let step_us = if MOVE_ACK_USE_REQUEST_TIMING {
                                    MoveAck::from_bytes(&e.data)
                                        .ok()
                                        .and_then(|ack| move_request_timestamps.remove(&ack.sequence))
                                        .map(|req_us| req_us.saturating_add(MOVE_ACK_TIMING_OFFSET_US))
                                        .unwrap_or(e.us_offset)
                                } else {
                                    e.us_offset
                                };

                                trace!(
                                    "[preprocess] S→C MoveAck matched dir={} → step (us: ack={}, used={})",
                                    direction,
                                    e.us_offset.saturating_sub(world_base_offset_us),
                                    step_us.saturating_sub(world_base_offset_us),
                                );
                                entries.push(ReplayEntry {
                                    us_offset: step_us.saturating_sub(world_base_offset_us),
                                    log_idx,
                                    kind: EntryKind::MoveAck { direction },
                                });
                                stats_move_ack += 1;
                            } else {
                                let ack_seq = MoveAck::from_bytes(&e.data)
                                    .map(|a| a.sequence)
                                    .unwrap_or(0);
                                let drain_reason = player.observer.last_drain_reason;
                                let drain_count = player.observer.last_drain_count;
                                warn!(
                                    "[preprocess] S→C MoveAck seq={} — not accepted, \
                                     skipping (drain_reason={:?}, drained={}, \
                                     queue_now={}, pending={:?}) \
                                     @{}µs log_idx={}",
                                    ack_seq,
                                    drain_reason,
                                    drain_count,
                                    player.observer.pending_moves().len(),
                                    player.observer.pending_moves()
                                        .iter()
                                        .map(|(s, f)| format!("(seq={}, dir={})", s, f))
                                        .collect::<Vec<_>>(),
                                    e.us_offset,
                                    log_idx,
                                );
                                stats_move_ack_lost += 1;
                                match drain_reason {
                                    DrainReason::DrawGamePlayer => stats_lost_by_draw_game_player += 1,
                                    DrainReason::MoveReject => stats_lost_by_move_reject += 1,
                                    DrainReason::MoveAckDesync => stats_lost_by_move_ack_desync += 1,
                                    DrainReason::SetMap => stats_lost_by_set_map += 1,
                                    _ => stats_lost_by_other += 1,
                                }
                            }
                        } else if e.data[0] == MoveReject::ID {
                            if let Ok(rej) = MoveReject::from_bytes(&e.data) {
                                warn!(
                                    "[preprocess] S→C MoveReject seq={} pos=({},{},{}) dir={} \
                                     (pending={:?}) @{}µs log_idx={}",
                                    rej.sequence,
                                    rej.x, rej.y, rej.z, rej.direction,
                                    player.observer.pending_moves()
                                        .iter()
                                        .map(|(s, f)| format!("(seq={}, dir={})", s, f))
                                        .collect::<Vec<_>>(),
                                    e.us_offset,
                                    log_idx,
                                );
                            }
                            stats_move_reject += 1;
                        } else if e.data[0] == 0x6C {
                            trace!("[preprocess] S→C 0x6C TargetCursor — skipped");
                            stats_skipped_target += 1;
                        } else if e.data[0] == 0x7C || e.data[0] == 0xB0 || e.data[0] == 0xDD {
                            trace!("[preprocess] S→C 0x{:02X} gump — skipped", e.data[0]);
                            stats_skipped_gumps += 1;
                        } else {
                            // Ingest container-related packets into the
                            // container store.
                            match e.data[0] {
                                0x24 => {
                                    if e.data.len() >= 7 {
                                        let cs = u32::from_be_bytes([e.data[1], e.data[2], e.data[3], e.data[4]]);
                                        let gump = u16::from_be_bytes([e.data[5], e.data[6]]);
                                        container_store.ingest_open(cs, gump);
                                        debug!(
                                            "[preprocess] S→C 0x24 DrawContainer serial={:#010X} gump={:#06X} @{}µs",
                                            cs, gump, e.us_offset,
                                        );
                                    }
                                }
                                0x3C => {
                                    use packets::interaction::ContainerContent;
                                    use packets::traits::ManualPacket;
                                    if let Ok(cc) = ContainerContent::from_bytes(&e.data) {
                                        if let Some(cs) = cc.container_serial() {
                                            let items = framework::diorama::container_items_from_content(&cc);
                                            container_store.ingest_content(cs, items);
                                            debug!(
                                                "[preprocess] S→C 0x3C ContainerContent container={:#010X} ({} items) @{}µs",
                                                cs,
                                                container_store.get(cs).map(|c| c.item_count()).unwrap_or(0),
                                                e.us_offset,
                                            );
                                        }
                                    }
                                }
                                0x25 => {
                                    use packets::interaction::AddItemToContainer;
                                    use packets::traits::ManualPacket;
                                    if let Ok(add) = AddItemToContainer::from_bytes(&e.data) {
                                        let cs = add.container_serial();
                                        let item = framework::diorama::container_item_from_add(&add);
                                        container_store.ingest_item_upsert(cs, item);
                                        debug!(
                                            "[preprocess] S→C 0x25 AddItemToContainer container={:#010X} ({} items now) @{}µs",
                                            cs,
                                            container_store.get(cs).map(|c| c.item_count()).unwrap_or(0),
                                            e.us_offset,
                                        );
                                    }
                                }
                                _ => {}
                            }

                            let is_world_init = matches!(
                                e.data[0],
                                0x1B | 0x20 | 0x55
                            );
                            entries.push(ReplayEntry {
                                us_offset: e.us_offset.saturating_sub(world_base_offset_us),
                                log_idx,
                                kind: if is_world_init {
                                    stats_world_init += 1;
                                    EntryKind::WorldInit(Bytes::copy_from_slice(&e.data))
                                } else {
                                    stats_forward += 1;
                                    EntryKind::Forward(Bytes::copy_from_slice(&e.data))
                                },
                            });
                        }
                    }
                }
            }
        }

        // Save a snapshot at regular timeline intervals for fast backward
        // seeks.  Snapshots are only taken once we are in the "world" phase.
        if in_world {
            let timeline_us = e.us_offset.saturating_sub(world_base_offset_us);
            if timeline_us >= next_snapshot_us {
                snapshots.push(LogPlayerSnapshot {
                    log_idx,
                    state: player.clone(),
                });
                next_snapshot_us = timeline_us + SNAPSHOT_INTERVAL_US;
            }
        }
    }

    // When request-timing mode shifts MoveAck entries earlier in time,
    // entries may end up out of chronological order.  Re-sort by us_offset
    // (stable sort preserves relative order of entries with equal timestamps).
    if MOVE_ACK_USE_REQUEST_TIMING {
        entries.sort_by_key(|e| e.us_offset);
    }

    let preprocess_elapsed = preprocess_start.elapsed();
    let total_timeline_us = entries.last().map(|e| e.us_offset).unwrap_or(0);
    debug!(
        "[preprocess] done in {:?} — {} raw log packets (C→S: {}, S→C: {})",
        preprocess_elapsed, log.len(), stats_c2s, stats_s2c,
    );
    debug!(
        "[preprocess] {} entries: {} forward, {} world-init, {} move-ack, {} C→S",
        entries.len(), stats_forward, stats_world_init, stats_move_ack,
        entries.iter().filter(|e| matches!(e.kind, EntryKind::ClientPacket(_))).count(),
    );
    debug!(
        "[preprocess] {} init packets, skipped {} gumps + {} target cursors, {} custom houses cached",
        init_packets.len(), stats_skipped_gumps, stats_skipped_target, house_cache.len(),
    );
    if stats_move_ack_lost > 0 || stats_move_reject > 0 {
        warn!(
            "[preprocess] movement issues: {} MoveAck lost, {} MoveReject \
             (lost by: DrawGamePlayer={}, MoveReject={}, MoveAckDesync={}, \
             SetMap={}, other={})",
            stats_move_ack_lost,
            stats_move_reject,
            stats_lost_by_draw_game_player,
            stats_lost_by_move_reject,
            stats_lost_by_move_ack_desync,
            stats_lost_by_set_map,
            stats_lost_by_other,
        );
        warn!(
            "[preprocess] accepted {} of {} total MoveAck ({:.1}% loss rate)",
            stats_move_ack,
            stats_move_ack + stats_move_ack_lost,
            if stats_move_ack + stats_move_ack_lost > 0 {
                stats_move_ack_lost as f64 / (stats_move_ack + stats_move_ack_lost) as f64 * 100.0
            } else {
                0.0
            },
        );
    }
    info!(
        "[preprocess] timeline: {:.1}s, {} snapshots (interval: {}ms), {} containers",
        total_timeline_us as f64 / 1_000_000.0,
        snapshots.len(),
        SNAPSHOT_INTERVAL_US / 1_000,
        container_store.len(),
    );
    (entries, init_packets, player, snapshots, house_cache, container_store)
}
