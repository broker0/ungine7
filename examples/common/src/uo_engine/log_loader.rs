//! Load the final world state from a `.uolog` recording.
//!
//! Processes all S→C packets to build per-world entity maps,
//! extract the player serial, and determine the player's map.
//!
//! [`load_world_from_logs`] loads multiple `.uolog` files sequentially,
//! merging entity maps so that later logs overwrite earlier data
//! (last-write-wins semantics).

use std::collections::HashMap;
use std::io;
use std::path::Path;

use log::{debug, info, warn};
use u_core::PacketDirection;

use packets::speech::SendSpeech;
use packets::traits::ManualPacket;

use crate::packet_log::read_log;
use crate::uo_engine::entity::DemoEntity;
use crate::uo_engine::ingest::ingest_into_entity_map;

// ── Result ─────────────────────────────────────────────────────────────────

/// Data extracted from a `.uolog` file.
pub struct LogWorldData {
    /// Entities per world (map_id → serial → entity).
    pub entities: HashMap<u8, HashMap<u32, DemoEntity>>,
    /// Player serial (from `0x1B CharacterLocaleAndBody`).
    pub player_serial: u32,
    /// Map the player is on (from `0xBF SetMap`, default 0).
    pub player_world: u8,
    /// Raw S→C packets received before the player origin (for login bootstrap).
    pub init_packets: Vec<Vec<u8>>,
    /// Raw container-related S→C packets (0x24, 0x25, 0x3C) collected from
    /// the entire log, paired with the world they were seen on.
    pub container_packets: Vec<(u8, Vec<u8>)>,
    /// Item display names extracted from `SendSpeech` (0x1C) packets.
    ///
    /// When the client single-clicks an item the server responds with a
    /// 0x1C packet whose `name`/`message` carries the item's label.
    /// We capture the **first** 0x1C per serial (the name line) and skip
    /// subsequent ones (metadata lines like "(29 items, 2032 stones)").
    pub item_names: HashMap<u32, String>,
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Read a `.uolog` file and extract the final world state.
///
/// Walks every entry in the log, ingesting S→C packets into entity maps.
/// Returns the accumulated world state at the end of the log.
pub fn load_world_from_log(path: &Path) -> io::Result<LogWorldData> {
    let entries = read_log(path)?;
    info!("[log_loader] loaded {} entries from {}", entries.len(), path.display());

    let mut entities: HashMap<u8, HashMap<u32, DemoEntity>> = HashMap::new();
    let mut player_serial: u32 = 0;
    let mut player_world: u8 = 0;
    let mut init_packets: Vec<Vec<u8>> = Vec::new();
    let mut container_packets: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut item_names: HashMap<u32, String> = HashMap::new();
    let mut origin_found = false;

    for entry in &entries {
        if entry.direction != PacketDirection::ServerToClient {
            continue;
        }
        if entry.data.is_empty() {
            continue;
        }

        let packet_id = entry.data[0];

        // Track player serial from 0x1B CharacterLocaleAndBody.
        if packet_id == 0x1B && entry.data.len() >= 5 {
            player_serial = u32::from_be_bytes([
                entry.data[1], entry.data[2], entry.data[3], entry.data[4],
            ]);
            debug!("[log_loader] 0x1B — player serial={:#010X}", player_serial);
        }

        // Track current world from 0xBF GeneralInfo::SetMap (sub 0x0008).
        if packet_id == 0xBF && entry.data.len() >= 6 {
            let sub_cmd = u16::from_be_bytes([entry.data[3], entry.data[4]]);
            if sub_cmd == 0x0008 {
                player_world = entry.data[5];
                debug!("[log_loader] SetMap: world={}", player_world);
            }
        }

        // Collect init packets (before first DrawMobile for player).
        if !origin_found {
            init_packets.push(entry.data.clone());
            if packet_id == 0x78 && entry.data.len() >= 5 && player_serial != 0 {
                let pkt_serial = u32::from_be_bytes([
                    entry.data[3], entry.data[4], entry.data[5], entry.data[6],
                ]);
                if pkt_serial == player_serial {
                    origin_found = true;
                    debug!("[log_loader] origin found at us_offset={}", entry.us_offset);
                }
            }
        }

        // Ingest into entity map.
        let world_map = entities.entry(player_world).or_default();
        ingest_into_entity_map(&entry.data, player_world, world_map);

        // Collect container-related packets (0x24, 0x25, 0x3C).
        if matches!(packet_id, 0x24 | 0x25 | 0x3C) {
            container_packets.push((player_world, entry.data.clone()));
        }

        // Extract item names from SendSpeech (0x1C).
        //
        // When a player single-clicks an item, the server responds with one
        // or more 0x1C packets.  The *first* one for a given serial carries
        // the display name ("a metal chest"); subsequent ones may carry
        // metadata ("(29 items, 2032 stones)").  We keep only the first
        // name per serial — later logs overwrite earlier names (last-write
        // wins when loading multiple logs).
        if packet_id == SendSpeech::ID {
            if let Ok(speech) = SendSpeech::from_bytes(&entry.data) {
                if speech.serial != 0 && speech.serial != 0xFFFF_FFFF {
                    let world_map = entities.entry(player_world).or_default();
                    let is_item = matches!(
                        world_map.get(&speech.serial),
                        Some(DemoEntity::Item { .. })
                    );
                    if is_item {
                        // Take the message (the overhead text) as the name.
                        // Fall back to the `name` field if message is empty.
                        let label = if speech.message.is_empty() {
                            speech.name.clone()
                        } else {
                            speech.message.clone()
                        };
                        if !label.is_empty() {
                            // Only store the first name per serial within
                            // a single log (skip metadata follow-ups).
                            item_names.entry(speech.serial).or_insert(label);
                        }
                    }
                }
            }
        }

        // Handle 0x1D DeleteObject.
        if packet_id == 0x1D && entry.data.len() >= 5 {
            let serial = u32::from_be_bytes([
                entry.data[1], entry.data[2], entry.data[3], entry.data[4],
            ]);
            // ingest_into_entity_map already handles deletion,
            // but only for non-mobiles (mobiles are preserved).
            let _ = serial;
        }
    }

    info!(
        "[log_loader] result: player={:#010X} world={} worlds={} total_entities={} container_pkts={} item_names={}",
        player_serial,
        player_world,
        entities.len(),
        entities.values().map(|m| m.len()).sum::<usize>(),
        container_packets.len(),
        item_names.len(),
    );

    Ok(LogWorldData {
        entities,
        player_serial,
        player_world,
        init_packets,
        container_packets,
        item_names,
    })
}

// ── Multi-log loader ──────────────────────────────────────────────────────

/// Load multiple `.uolog` files sequentially and merge their world state.
///
/// Logs are processed in order (earliest first).  For each log the entity
/// maps, container packets, and item names are merged with last-write-wins
/// semantics — a later log overwrites any overlapping data from an earlier
/// one.
///
/// `player_serial` and `player_world` are taken from the **last** log that
/// contains a non-zero player serial.  `init_packets` are likewise taken
/// from the last log that produced them.
pub fn load_world_from_logs(paths: &[&Path]) -> io::Result<LogWorldData> {
    assert!(!paths.is_empty(), "load_world_from_logs requires at least one path");

    if paths.len() == 1 {
        return load_world_from_log(paths[0]);
    }

    info!("[log_loader] loading {} log files sequentially", paths.len());

    let mut merged = LogWorldData {
        entities: HashMap::new(),
        player_serial: 0,
        player_world: 0,
        init_packets: Vec::new(),
        container_packets: Vec::new(),
        item_names: HashMap::new(),
    };

    for (i, path) in paths.iter().enumerate() {
        info!("[log_loader] [{}/{}] loading {}", i + 1, paths.len(), path.display());

        let data = match load_world_from_log(path) {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    "[log_loader] [{}/{}] failed to load {}: {e} — skipping",
                    i + 1,
                    paths.len(),
                    path.display(),
                );
                continue;
            }
        };

        // Merge entity maps: per-world, per-serial overwrite.
        for (world_id, world_entities) in data.entities {
            let target = merged.entities.entry(world_id).or_default();
            target.extend(world_entities);
        }

        // Append container packets (ordering matters — later logs can
        // redefine container contents and the last 0x24/0x3C wins during
        // ingestion into the container store).
        merged.container_packets.extend(data.container_packets);

        // Merge item names (last-write-wins).
        merged.item_names.extend(data.item_names);

        // Player identity: last log with a non-zero serial wins.
        if data.player_serial != 0 {
            merged.player_serial = data.player_serial;
            merged.player_world = data.player_world;
        }

        // Init packets: take from the last log that has them.
        if !data.init_packets.is_empty() {
            merged.init_packets = data.init_packets;
        }
    }

    info!(
        "[log_loader] merged result: player={:#010X} world={} worlds={} total_entities={} container_pkts={} item_names={}",
        merged.player_serial,
        merged.player_world,
        merged.entities.len(),
        merged.entities.values().map(|m| m.len()).sum::<usize>(),
        merged.container_packets.len(),
        merged.item_names.len(),
    );

    Ok(merged)
}
