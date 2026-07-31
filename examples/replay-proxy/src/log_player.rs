//! [`LogPlayer`] — incremental packet log player.
//!
//! Maintains the full world state (player position, visible objects, entity
//! map, pending move requests) while advancing through a `&[LogEntry]`.
//!
//! Core observation is delegated to [`ObserverPipeline`] which handles
//! position tracking, movement prediction, visible objects, and multi
//! registration in a single pass.  `LogPlayer` adds the three-phase state
//! machine, entity map ingestion, character name extraction, and
//! snapshot-based seeking on top.
//!
//! # Seek semantics
//!
//! - **Forward** (`advance_to`): processes log entries from the current
//!   position to the target — O(delta) work.
//! - **Backward** (`advance_to` / `seek_to`): restores the nearest snapshot
//!   and replays only the remaining tail — O(interval) work instead of O(N).
//!   Falls back to a full reset when no suitable snapshot is available.
//!
//! The caller can use [`LogPlayer::advance_to`] for both directions (it
//! detects backward targets automatically).
//!
//! # Snapshots
//!
//! Snapshots are full clones of `LogPlayer` state captured at regular
//! timeline intervals during preprocessing.  They are stored externally
//! and passed to `advance_to` / `seek_to` as `&[LogPlayerSnapshot]`.

/// Interval between [`LogPlayer`] snapshots on the replay timeline.
///
/// Lower values trade memory for faster backward seeks.
pub const SNAPSHOT_INTERVAL_US: u64 = 5_000_000;

/// A full checkpoint of [`LogPlayer`] state at a specific log position.
#[derive(Clone)]
pub struct LogPlayerSnapshot {
    /// Index into the raw `&[LogEntry]` slice at which this snapshot was taken.
    pub log_idx: usize,
    /// Complete `LogPlayer` state (position, entity map, pending moves, etc.).
    pub state: LogPlayer,
}

use log::{debug, trace};
use std::collections::HashMap;
use std::sync::Arc;

use framework::diorama::ObserverPipeline;
use framework::ecumene::{StaticDataProvider, StaticWorldData};

use u_core::PacketDirection;
use packets::character::CharacterLocaleAndBody;
use packets::login::CharacterList;
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::DrawMobile;


use crate::packet_log::LogEntry;
use crate::uo_engine::entity::DemoEntity;
use crate::uo_engine::ingest::ingest_into_entity_map;

// ── Phase ─────────────────────────────────────────────────────────────────

/// Three-phase state machine mirroring the log structure.
///
/// - `PreWorld`  — before `0x1B`; player serial unknown.
/// - `PreOrigin` — serial known, waiting for first `0x78` of the player.
/// - `World`     — after the origin `0x78`; all packets update state.
#[derive(Clone, PartialEq)]
pub enum Phase {
    PreWorld,
    PreOrigin,
    World,
}

// ── LogPlayer ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LogPlayer {
    // ── Playback position ────────────────────────────────────────────────
    /// Index of the last processed `LogEntry` (inclusive), or `usize::MAX`
    /// when nothing has been processed yet.
    current_log_idx: usize,

    /// Phase of the state machine.
    pub phase: Phase,

    /// Serial of the player character (extracted from `0x1B`).
    pub player_serial: u32,

    // ── Unified diorama observer ───────────────────────────────────────────
    /// Position, pending moves, visible objects, multi registry, current
    /// world — all updated in a single pass per packet.
    pub observer: ObserverPipeline,

    // ── Replay-specific state ────────────────────────────────────────────
    /// Per-world entity stores: `world_id → (serial → Entity)`.
    ///
    /// When the player transitions between worlds (e.g. Felucca → Trammel),
    /// entities from the previous world are preserved in their own sub-map
    /// instead of being discarded.  This allows seeks and end-of-replay
    /// population to restore all worlds correctly.
    pub entity_maps: HashMap<u8, HashMap<u32, DemoEntity>>,

    /// Character name extracted from `0xA9 CharacterList` (first non-empty
    /// slot).  Populated once during pre-world phase.
    pub char_name: Option<String>,
}

// ── Constructors ──────────────────────────────────────────────────────────

impl LogPlayer {
    /// Create a new player in the initial (empty) state.
    ///
    /// If `static_data` is provided, it is used for multi-object
    /// registration and Z resolution during movement.
    pub fn new(static_data: Option<Arc<StaticWorldData>>) -> Self {
        let sd: Option<Arc<dyn StaticDataProvider>> =
            static_data.map(|s| s as Arc<dyn StaticDataProvider>);
        Self {
            current_log_idx: usize::MAX,
            phase: Phase::PreWorld,
            player_serial: 0,
            observer: ObserverPipeline::new(sd),
            entity_maps: HashMap::new(),
            char_name: None,
        }
    }

    /// Reset all state back to the initial (empty) state.
    ///
    /// The observer's static data reference is preserved.
    pub fn reset(&mut self) {
        self.current_log_idx = usize::MAX;
        self.phase = Phase::PreWorld;
        self.player_serial = 0;
        self.observer.reset();
        self.entity_maps.clear();
        self.char_name = None;
    }
}

// ── Seeking ───────────────────────────────────────────────────────────────

impl LogPlayer {
    /// Process log entries from the current position up to `target_log_idx`
    /// (inclusive).
    ///
    /// If `target_log_idx` is behind the current position, restores the
    /// nearest snapshot and replays the remaining tail.  Falls back to a
    /// full reset when no suitable snapshot is available.
    pub fn advance_to(
        &mut self,
        log: &[LogEntry],
        target_log_idx: usize,
        snapshots: &[LogPlayerSnapshot],
    ) {
        let start = if self.current_log_idx == usize::MAX {
            0
        } else {
            self.current_log_idx + 1
        };

        if target_log_idx < start {
            // Backward seek — try to restore from the nearest snapshot.
            self.restore_nearest(log, target_log_idx, snapshots);
        } else {
            self.process_range(log, start, target_log_idx);
        }
    }

    /// Reset state and replay from the best available starting point up to
    /// `target_log_idx` (inclusive).
    pub fn seek_to(
        &mut self,
        log: &[LogEntry],
        target_log_idx: usize,
        snapshots: &[LogPlayerSnapshot],
    ) {
        self.restore_nearest(log, target_log_idx, snapshots);
    }

    /// Restore from the best snapshot with `log_idx <= target` and replay
    /// the remaining entries.  Falls back to a full reset + replay from 0
    /// when no suitable snapshot exists.
    fn restore_nearest(
        &mut self,
        log: &[LogEntry],
        target_log_idx: usize,
        snapshots: &[LogPlayerSnapshot],
    ) {
        let best = match snapshots.binary_search_by_key(&target_log_idx, |s| s.log_idx) {
            Ok(exact) => Some(&snapshots[exact]),
            Err(ins) if ins > 0 && snapshots[ins - 1].log_idx <= target_log_idx => {
                Some(&snapshots[ins - 1])
            }
            _ => None,
        };

        if let Some(snap) = best {
            *self = snap.state.clone();
            if snap.log_idx < target_log_idx {
                self.process_range(log, snap.log_idx + 1, target_log_idx);
            }
        } else {
            self.reset();
            self.process_range(log, 0, target_log_idx);
        }
    }

    /// The index of the last processed log entry, or `usize::MAX` if
    /// nothing has been processed.
    pub fn current_log_idx(&self) -> usize {
        self.current_log_idx
    }

    /// Take a detached copy of entities for the current world.
    pub fn take_entities(&self) -> Vec<DemoEntity> {
        self.entity_maps
            .get(&self.observer.session.current_world)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Take a detached copy of entities for a specific world.
    pub fn take_entities_for_world(&self, world: u8) -> Vec<DemoEntity> {
        self.entity_maps
            .get(&world)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Take detached copies of entities for all worlds.
    pub fn take_all_world_entities(&self) -> HashMap<u8, Vec<DemoEntity>> {
        self.entity_maps
            .iter()
            .map(|(&world, m)| (world, m.values().cloned().collect()))
            .collect()
    }
}

// ── Internal processing ───────────────────────────────────────────────────

impl LogPlayer {
    /// Process log entries `log[from ..= to]`.
    fn process_range(&mut self, log: &[LogEntry], from: usize, to: usize) {
        let to = to.min(log.len().saturating_sub(1));
        for i in from..=to {
            self.process_entry(i, &log[i]);
        }
    }

    /// Process a single log entry, advancing internal state.
    fn process_entry(&mut self, idx: usize, entry: &LogEntry) {
        self.current_log_idx = idx;
        self.observer.last_move_accepted = false;

        match entry.direction {
            PacketDirection::ClientToServer => {
                self.observer.ingest_c2s(&entry.data);
            }
            PacketDirection::ServerToClient => {
                if entry.data.is_empty() {
                    return;
                }
                self.process_s2c(entry);
            }
        }
    }

    fn process_s2c(&mut self, entry: &LogEntry) {
        match self.phase {
            Phase::PreWorld => self.process_s2c_pre_world(entry),
            Phase::PreOrigin => self.process_s2c_pre_origin(entry),
            Phase::World => self.process_s2c_world(entry),
        }
    }

    /// Apply a S→C packet through the observer and entity map.
    ///
    /// The observer handles position, movement, visible set, multi
    /// registry, and world tracking in a single pass.  Entity map
    /// ingestion is done separately (replay-specific).
    fn ingest_s2c_state(&mut self, data: &[u8]) {
        // Single-pass observer: position + movement + visible + multi + world.
        self.observer.ingest_s2c(data);

        // Entity map ingestion (replay-specific, still parses independently
        // for entity-specific packets like StatusBarInfo, SendSpeech, etc.).
        let current_world = self.observer.session.current_world;
        let world_map = self.entity_maps.entry(current_world).or_default();
        ingest_into_entity_map(data, current_world, world_map);
    }

    fn process_s2c_pre_world(&mut self, entry: &LogEntry) {
        if entry.data[0] == CharacterLocaleAndBody::ID {
            if let Ok(pkt) = CharacterLocaleAndBody::from_bytes(&entry.data) {
                self.player_serial = pkt.serial;
                debug!(
                    "[log_player] 0x1B — player serial={:#010X}",
                    self.player_serial
                );
            }
            self.ingest_s2c_state(&entry.data);
            self.phase = Phase::PreOrigin;
        } else if entry.data[0] == 0xA9 {
            // Extract character name from the first non-empty slot.
            if self.char_name.is_none() {
                if let Ok(cl) = CharacterList::from_bytes(&entry.data) {
                    for slot in cl.characters.iter() {
                        if !slot.is_empty() {
                            self.char_name = Some(slot.name.to_string());
                            break;
                        }
                    }
                }
            }
            trace!("[log_player] S→C 0xA9 CharacterList");
        } else {
            // Any other pre-world S→C: apply state but nothing else.
            self.ingest_s2c_state(&entry.data);
        }
    }

    fn process_s2c_pre_origin(&mut self, entry: &LogEntry) {
        self.ingest_s2c_state(&entry.data);

        // Check for our first 0x78 DrawMobile.
        if entry.data[0] == DrawMobile::ID {
            if let Ok(pkt) = DrawMobile::from_bytes(&entry.data) {
                if pkt.serial == self.player_serial {
                    debug!(
                        "[log_player] 0x78 for player {:#010X} — entering World phase",
                        self.player_serial
                    );
                    self.phase = Phase::World;
                }
            }
        }
    }

    fn process_s2c_world(&mut self, entry: &LogEntry) {
        self.ingest_s2c_state(&entry.data);
    }
}

// `ingest_into_entity_map` has been moved to `common::uo_engine::ingest`.
// It is re-exported via `crate::uo_engine::ingest::ingest_into_entity_map`
// and imported at the top of this file.
