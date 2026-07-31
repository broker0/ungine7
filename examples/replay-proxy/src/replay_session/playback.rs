//! Playback loop — timed dispatch of replay entries with pause, seek,
//! step and fast-forward support.
//!
//! This module contains the [`PlaybackState`] state machine, the main
//! [`run_playback`] loop, entry dispatch, seek/step helpers, and the
//! unified command handler that maps UI actions to state transitions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use log::{debug, info, trace};

use u_core::PacketDirection;
use framework::diorama::ObserverPipeline;
use framework::rythmos::PositionTracker;
use framework::ecumene::StaticWorldData;
use network::error as fw_error;
use network::session::{Session, SessionEvent};
use packets::character::{DrawGamePlayer, UpdateMobile};
use packets::system::{ClientViewRange, LoginComplete};
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

use packets::registry::PacketRegistry;

use crate::dot_commands::{DotCommands, Handled};
use crate::continuum::WorkerCommand;
use crate::log_player::{LogPlayer, LogPlayerSnapshot};
use crate::packet_log::LogEntry;
use crate::replay_handler::ReplayCommand;

use super::client_handler::handle_client_packet;
use super::engine_rpc::ShadowTx;
use common::uo_engine::rpc::EngineProxy;
use common::uo_engine::handler::EngineCommand;
use super::{EntryKind, ReplayEntry, ViewMode, FAST_FORWARD_SPEED};

// ── PlaybackState ─────────────────────────────────────────────────────────

/// Mutable playback state — groups the dozen `let mut` variables that
/// `run_playback` previously scattered across the function body.
pub struct PlaybackState {
    pub pos: PositionTracker,
    /// Real-time instant corresponding to `us_offset == 0`.
    /// Recalculated after seek so that `entries[idx].us_offset` maps to the
    /// correct wall-clock moment.
    pub start: tokio::time::Instant,
    /// Index of the next entry to dispatch.
    pub idx: usize,
    /// Current logical position in the replay timeline (µs).
    pub current_us: u64,
    /// Total duration of the replay (us_offset of the last entry).
    pub total_us: u64,
    /// After a seek we suppress `0x1B`/`0x55` — the client is already
    /// synchronised via `DrawGamePlayer`.
    pub seeked: bool,
    /// World at the time of seek.  If the replay crosses a world boundary
    /// while `seeked` is still `true`, we must reset it so that world-init
    /// packets (`0x20`, etc.) for the new world are forwarded as-is.
    pub seeked_world: u8,
    /// When `true` the timer does not tick; only client events are processed.
    pub paused: bool,
    /// Instant when pause was entered — used to shift `start` on resume.
    pub pause_start: Option<tokio::time::Instant>,
    /// Playback speed multiplier (1.0 = normal, >1.0 = fast, <1.0 = slow).
    pub speed: f64,
    /// When fast-forwarding, the target timeline position (µs).  Once
    /// `current_us >= ff_target_us` the speed resets to 1.
    pub ff_target_us: Option<u64>,
    /// Saved pause state for transitions that temporarily need to unpause
    /// (e.g. fast-forward).  `Some(true)` means "was paused before the
    /// transition started"; restored by `restore_pause()`.
    pub saved_pause: Option<bool>,

    // ── Observer mode ─────────────────────────────────────────────────

    /// View mode for the current session.
    pub view_mode: ViewMode,

    /// Position of the recorded character during Observer mode.
    ///
    /// In `FirstPerson` mode this tracks the same position as `pos`
    /// (both refer to the same entity).  In `Observer` mode, `pos` tracks
    /// the observer entity and `replay_pos` tracks the recorded NPC.
    pub replay_pos: PositionTracker,
}

impl PlaybackState {
    /// Temporarily suspend pause so the timer can tick (e.g. for
    /// fast-forward).  The current pause state is saved and can be
    /// restored later with [`restore_pause`].
    fn suspend_pause(&mut self) {
        self.saved_pause = Some(self.paused);
        self.paused = false;
        self.pause_start = None;
    }

    /// Restore the pause state that was saved by [`suspend_pause`].
    /// If the saved state was "paused", we re-enter pause with a fresh
    /// `pause_start`.  No-op if nothing was saved.
    fn restore_pause(&mut self) {
        if let Some(was_paused) = self.saved_pause.take() {
            if was_paused {
                self.paused = true;
                self.pause_start = Some(tokio::time::Instant::now());
            }
        }
    }

    /// Compute the clamped target timeline position from a signed delta (µs).
    pub(crate) fn clamp_target(&self, delta_us: i64) -> u64 {
        (self.current_us as i64 + delta_us)
            .clamp(0, self.total_us as i64) as u64
    }

    /// Apply a state transition.  Returns `true` if the playback-control
    /// gump should be (re-)sent to the client afterwards.
    ///
    /// **This is the single place** where `paused`, `pause_start`, `start`,
    /// `speed`, and `ff_target_us` are mutated, ensuring consistent state
    /// regardless of which button the player pressed.
    pub fn transition(&mut self, t: PlaybackTransition) -> bool {
        use tokio::time::Instant;

        /// Convert a timeline position `ms` at the given `speed` multiplier
        /// into a wall-clock `Duration`.  Clamps to zero to avoid panics
        /// from `Duration::from_secs_f64` on negative values.
        #[inline]
        fn scaled_duration(us: u64, speed: f64) -> Duration {
            let secs = us as f64 / 1_000_000.0 / speed;
            Duration::from_secs_f64(secs.max(0.0))
        }

        match t {
            // ── Pause / Resume ───────────────────────────────────────
            PlaybackTransition::TogglePause => {
                if self.paused {
                    self.paused = false;
                    if let Some(ps) = self.pause_start.take() {
                        self.start += ps.elapsed();
                    }
                } else {
                    self.paused = true;
                    self.pause_start = Some(Instant::now());
                }
                true
            }

            // ── Snapshot seek (preserves pause state) ────────────────
            PlaybackTransition::Seek { target_us, entry_idx } => {
                self.current_us = target_us;
                self.idx = entry_idx + 1;
                self.start = Instant::now()
                    .checked_sub(scaled_duration(target_us, self.speed))
                    .unwrap_or_else(Instant::now);
                if self.paused {
                    self.pause_start = Some(Instant::now());
                }
                true
            }

            // ── Begin accelerated playback ───────────────────────────
            PlaybackTransition::StartFastForward { target_us } => {
                self.speed = FAST_FORWARD_SPEED;
                self.ff_target_us = Some(target_us);
                // Temporarily unpause so the timer ticks; the original
                // pause state is restored by EndFastForward.
                self.suspend_pause();
                self.start = Instant::now()
                    .checked_sub(scaled_duration(self.current_us, self.speed))
                    .unwrap_or_else(Instant::now);
                // No gump: client already closed it on button press;
                // a fresh one will appear on EndFastForward.
                false
            }

            // ── FF reached its target — back to normal speed ─────────
            PlaybackTransition::EndFastForward => {
                self.ff_target_us = None;
                self.speed = 1.0;
                self.start = Instant::now()
                    .checked_sub(scaled_duration(self.current_us, 1.0))
                    .unwrap_or_else(Instant::now);
                // Restore the pause state that was active before FF.
                self.restore_pause();
                true
            }

            // ── Step to a specific entry (always enters pause) ───────
            PlaybackTransition::StepTo { entry_idx, us } => {
                self.current_us = us;
                self.idx = entry_idx + 1;
                self.paused = true;
                self.pause_start = Some(Instant::now());
                self.start = Instant::now()
                    .checked_sub(scaled_duration(us, 1.0))
                    .unwrap_or_else(Instant::now);
                true
            }
        }
    }
}

/// All possible playback state transitions.
///
/// Every transition is applied exclusively through
/// [`PlaybackState::transition`], which guarantees that fields like
/// `start`, `paused`, `pause_start`, and `speed` stay consistent.
pub enum PlaybackTransition {
    /// Toggle between paused and playing.
    TogglePause,
    /// Snapshot-based seek: jump timeline to `target_us` / `entry_idx`.
    /// Preserves the current pause state.
    Seek { target_us: u64, entry_idx: usize },
    /// Begin accelerated playback towards `target_us`.
    /// Unconditionally leaves pause.
    StartFastForward { target_us: u64 },
    /// Fast-forward reached its target — return to 1× speed.
    EndFastForward,
    /// Step to a specific entry.  Always enters pause.
    StepTo { entry_idx: usize, us: u64 },
}

// ── Seek / step helpers ──────────────────────────────────────────────────

/// Perform a seek to `target_entry_idx` using the given `LogPlayer`.
///
/// The `LogPlayer` is advanced (or reset+replayed) to the target log
/// position.  Then the client is synchronised (position + visible items).
///
/// **Does not** update `PlaybackState` bookkeeping (`current_us`, `idx`,
/// `start`, etc.) — the caller must apply the appropriate
/// [`PlaybackTransition`] afterwards.
async fn perform_seek(
    target_entry_idx: usize,
    entries: &[ReplayEntry],
    log_entries: &[LogEntry],
    player: &mut LogPlayer,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    shadow_tx: &ShadowTx,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
) -> fw_error::Result<()> {
    let target_log_idx = entries[target_entry_idx].log_idx;
    let target_us = entries[target_entry_idx].us_offset;

    let old_world = observer.session.current_world;
    let old_view_range = observer.view_range();
    let old_enable_features = observer.session.last_enable_features.clone();

    // advance_to handles both forward (incremental) and backward (snapshot
    // restore) seeks automatically.
    player.advance_to(log_entries, target_log_idx, snapshots);

    let is_observer = pb.view_mode.is_observer();

    // In Observer mode, only replay_pos jumps; observer pos stays put.
    // In FirstPerson mode, pb.pos tracks the recorded character directly.
    if is_observer {
        pb.replay_pos = player.observer.pos;
    } else {
        pb.pos = player.observer.pos;
    }
    observer.session = framework::diorama::SessionView::new(0, 0, ClientViewRange::DEFAULT as u16);
    observer.session.current_world = player.observer.session.current_world;
    // Restore the view range from the LogPlayer's session state (tracked
    // via S→C 0xC8 packets and preserved in snapshots).
    let new_view_range = player.observer.session.view_range();
    observer.session.visible.set_view_range(new_view_range);
    // Restore EnableFeatures from the LogPlayer's session state.
    observer.session.last_enable_features = player.observer.session.last_enable_features.clone();
    pb.seeked = true;
    pb.seeked_world = observer.session.current_world;

    // Reset all world zones with entities at the seek target point.
    // This ensures that all worlds have correct state after a seek
    // (especially backward seeks that cross world boundaries).
    // reset_zone handles observer re-spawn automatically.
    let all_worlds = player.take_all_world_entities();
    let obs_world = observer.session.current_world;
    for (&world_id, entities) in &all_worlds {
        let obs_pos = if world_id == obs_world {
            Some(&pb.pos)
        } else {
            None
        };
        super::reset_zone(
            shadow_tx, world_id, entities.clone(),
            framework::continuum::HashContainerStore::new(),
            &pb.view_mode, obs_pos,
        ).await;
    }

    // Re-ingest custom house data after zone reset — the reset wipes
    // custom_defs from the EntityRegistry but the entities are now
    // back in the zone, so the 0xD8 handler will find them.
    super::reingest_house_cache(shadow_tx, obs_world, house_cache).await;

    // If the seek crossed a world boundary, send a synthetic SetMap so
    // the client switches its map renderer to the correct facet.
    if observer.session.current_world != old_world {
        info!(
            "[replay] seek crossed world boundary: {} → {}",
            old_world, observer.session.current_world,
        );
        use packets::system::GeneralInfo;
        let set_map = GeneralInfo::SetMap { world: observer.session.current_world };
        client.send(RawPacket::s2c(set_map.to_bytes())).await?;
    }

    // If the view range changed, tell the client.
    if new_view_range != old_view_range {
        info!(
            "[replay] seek view range changed: {} → {}",
            old_view_range, new_view_range,
        );
        let cvr = ClientViewRange::new(new_view_range as u8);
        client.send(RawPacket::s2c(cvr.to_bytes())).await?;
    }

    // If EnableFeatures (0xB9) changed, re-send it so the client has
    // the correct feature flags for this point in the timeline.
    if observer.session.last_enable_features != old_enable_features {
        if let Some(ref ef_data) = observer.session.last_enable_features {
            info!(
                "[replay] seek EnableFeatures changed — re-sending 0xB9 ({} bytes)",
                ef_data.len(),
            );
            client.send(RawPacket::s2c(ef_data.clone())).await?;
        }
    }

    // After zone reset, resolve Z through the physics continuum to account
    // for multi-objects (bridges, houses, stairs, etc.) that LogPlayer
    // does not consider during fast-forward.
    if is_observer {
        // Observer stays where they are — only resync their DrawGamePlayer
        // and send NPC update for the recorded character's new position.
        if pb.pos.is_ready() {
            client
                .send(RawPacket::s2c(pb.pos.to_draw_game_player().to_bytes()))
                .await?;
        }
        if let Some(upd) = pb.view_mode.build_replay_char_update(&pb.replay_pos) {
            client.send(RawPacket::s2c(upd)).await?;
        }
        observer.session.visible.update_view(pb.pos.x, pb.pos.y);
    } else {
        if pb.pos.is_ready() {
            let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
            if let Some(new_z) = engine.resolve_z(
                pb.pos.x, pb.pos.y, pb.pos.z, pb.pos.facing.heading(),
            ).await {
                pb.pos.z = new_z;
            }
            client
                .send(RawPacket::s2c(pb.pos.to_draw_game_player().to_bytes()))
                .await?;
        }
        observer.session.visible.update_view(pb.pos.x, pb.pos.y);
    }
    let world = observer.session.current_world;
    let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
    let items = engine.items_in_area(*observer.view_rect()).await;
    for raw in &items {
        observer.session.ingest_packet(raw);
        client.send(RawPacket::s2c(raw.clone())).await?;
    }

    // Custom house designs (0xD8) are NOT bulk-sent here.  The visible
    // items above include house foundations (0x1A) which trigger
    // 0xBF:001D HouseRevisionState during normal playback.  The client
    // will request designs via 0xBF:001E if needed, and
    // `handle_client_packet` responds from the cache.

    debug!(
        "[replay] seek done — entry={} target={}µs pos ({},{}) world={} view_range={} sent {} items, seeked={}",
        target_entry_idx, target_us, pb.pos.x, pb.pos.y,
        observer.session.current_world, new_view_range, items.len(), pb.seeked
    );

    Ok(())
}

/// Seek to the entry closest to `target_us` by `us_offset`, ignoring
/// `ClientPacket` entries (they are not world-state milestones).
pub fn entry_idx_for_us(entries: &[ReplayEntry], target_us: u64) -> usize {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.kind, EntryKind::ClientPacket(_)))
        .filter(|(_, e)| e.us_offset <= target_us)
        .map(|(i, _)| i)
        .last()
        .unwrap_or(0)
}

/// Find the index of the nearest entry in `direction` from `current_idx`
/// that matches the predicate.
/// `direction`: -1 = previous, +1 = next.
pub fn find_step_target(
    entries: &[ReplayEntry],
    current_idx: usize,
    direction: i32,
    predicate: fn(&ReplayEntry) -> bool,
) -> Option<usize> {
    if direction < 0 {
        (0..current_idx)
            .rev()
            .find(|&i| predicate(&entries[i]))
    } else {
        (current_idx + 1..entries.len())
            .find(|&i| predicate(&entries[i]))
    }
}

/// Log a human-readable description of the packet inside `entry`.
///
/// Uses the provided [`PacketRegistry`] to deserialize the raw bytes and
/// prints the result at `info` level.  Entries without raw data (e.g.
/// `MoveAck`) are described by their `EntryKind` variant.
///
/// `prefix` is prepended to the log line (e.g. `"[step]"` or `"[skip]"`).
pub(super) fn log_entry_description(entry: &ReplayEntry, reg: &PacketRegistry, prefix: &str) {
    use packets::registry::{DecodedResult, OutputFormat};

    let (data, dir_label, direction) = match &entry.kind {
        EntryKind::Forward(data) | EntryKind::WorldInit(data) => {
            (data.as_ref(), "S→C", PacketDirection::ServerToClient)
        }
        EntryKind::ClientPacket(data) => {
            (data.as_ref(), "C→S", PacketDirection::ClientToServer)
        }
        EntryKind::MoveAck { direction } => {
            info!(
                "{} @{}µs MoveAck dir={}",
                prefix, entry.us_offset, direction,
            );
            return;
        }
    };

    if data.is_empty() {
        return;
    }
    let id = data[0];
    let desc = match reg.decode(id, data, direction, OutputFormat::Debug) {
        DecodedResult::Ok(decoded) => decoded.into_string(),
        DecodedResult::DecodeError(e) => format!("<decode error: {}>", e),
        DecodedResult::Unknown => format!("<unknown 0x{:02X}>", id),
    };
    info!(
        "{} @{}µs {} 0x{:02X} ({} bytes): {}",
        prefix, entry.us_offset, dir_label, id, data.len(), desc,
    );
}

/// Seek-and-step: find the nearest entry matching `predicate` in
/// `direction`, perform a full seek to it, then send the control gump.
/// Leaves playback in paused state.
///
/// When stepping forward, also logs descriptions of all skipped entries
/// between the current position and the target.
async fn step_to_entry(
    direction: i32,
    predicate: fn(&ReplayEntry) -> bool,
    label: &str,
    entries: &[ReplayEntry],
    log_entries: &[LogEntry],
    player: &mut LogPlayer,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    shadow_tx: &ShadowTx,
    cmds: &mut DotCommands,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
) -> fw_error::Result<()> {
    // pb.idx points to the *next* entry to dispatch; the entry we are
    // currently sitting on is pb.idx - 1.  Use that as the anchor so
    // backward search doesn't keep finding the same entry.
    let anchor = pb.idx.saturating_sub(1);
    if let Some(ei) = find_step_target(entries, anchor, direction, predicate) {
        let reg = PacketRegistry::default();

        // When stepping forward, log all skipped entries between anchor
        // and the target so the user can see what was passed over.
        if direction > 0 && ei > anchor + 1 {
            let skipped = ei - anchor - 1;
            info!("[replay] step {} (dir={}) → entry={} (skipping {} entries)", label, direction, ei, skipped);
            for skip_idx in (anchor + 1)..ei {
                log_entry_description(&entries[skip_idx], &reg, "[skip]");
            }
        } else {
            info!("[replay] step {} (dir={}) → entry={}", label, direction, ei);
        }

        log_entry_description(&entries[ei], &reg, "[step]");
        perform_seek(ei, entries, log_entries, player, pb, observer, client, shadow_tx, snapshots, house_cache).await?;
        pb.transition(PlaybackTransition::StepTo {
            entry_idx: ei,
            us: entries[ei].us_offset,
        });

        // perform_seek syncs position + visible items but does not send the
        // target packet itself.  Forward it so the client actually sees it
        // (e.g. speech, sound effects, animations).  WorldInit and MoveAck
        // are already covered by the DrawGamePlayer that perform_seek sends;
        // ClientPacket is C→S and never forwarded.
        if let EntryKind::Forward(data) = &entries[ei].kind {
            client.send(RawPacket::s2c(data.clone())).await?;
        }
    }
    cmds.send_playback_control_gump(
        pb.pos.serial, pb.current_us / 1_000, pb.total_us / 1_000, pb.paused, client,
    ).await?;
    Ok(())
}

// ── Command dispatch ─────────────────────────────────────────────────────

/// Result of processing a single `Handled` variant inside `run_playback`.
pub enum CommandResult {
    /// Command consumed — caller should `continue` the main loop.
    Continue,
    /// The user requested to stop playback — caller should return `pos`.
    StopPlayback,
    /// The packet was not a command — caller should process it normally.
    ProcessNormally(RawPacket),
}

/// Unified handler for all [`Handled`] variants inside the playback loop.
///
/// Both the *paused* and *active* branches call this function, eliminating
/// the duplicated match arms that existed before.
///
/// All playback-state mutations go through [`PlaybackState::transition`],
/// and the control gump is (re-)sent exactly when `transition` says so.
///
/// The gump refresh is handled uniformly: each branch sets `show_gump` to
/// indicate whether the control bar should be updated, and a single block
/// at the end performs the actual send.
async fn handle_playback_command(
    handled: Handled,
    packet: RawPacket,
    entries: &[ReplayEntry],
    log_entries: &[LogEntry],
    player: &mut LogPlayer,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    shadow_tx: &ShadowTx,
    cmds: &mut DotCommands,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
) -> fw_error::Result<CommandResult> {
    // Set by branches that call `pb.transition()`; the gump is refreshed
    // once after the match if this is `true`.
    let mut show_gump = false;

    match handled {
        Handled::StopPlayback => {
            info!(
                "[replay] stopped by user via gump (entry {}/{}, {})",
                pb.idx,
                entries.len(),
                if pb.paused { "paused" } else { "playing" },
            );
            return Ok(CommandResult::StopPlayback);
        }

        Handled::TogglePause => {
            let action = if pb.paused { "resumed" } else { "paused" };
            show_gump = pb.transition(PlaybackTransition::TogglePause);
            info!("[replay] {} at {}µs", action, pb.current_us);
        }

        Handled::SeekPlayback(delta_ms) => {
            let delta_us = delta_ms * 1_000; // convert ms → µs
            let target_us = pb.clamp_target(delta_us);
            let abs_delta = (target_us as i64 - pb.current_us as i64).unsigned_abs();
            info!(
                "[replay] seek {} ms: {}µs → {}µs / {}µs{}",
                delta_ms, pb.current_us, target_us, pb.total_us,
                if pb.paused { " (paused)" } else { "" },
            );

            // Snapshot-based seek for both directions.
            // For large seeks (> 1 s) show intermediate frames.
            const INTERMEDIATE_PAUSE: Duration = Duration::from_millis(100);
            const STEP_US: u64 = 1_000_000;
            const MIN_DELTA_FOR_INTERMEDIATES: u64 = 1_000_000;

            if abs_delta > MIN_DELTA_FOR_INTERMEDIATES {
                let sign: i64 = if delta_ms >= 0 { 1 } else { -1 };

                if sign < 0 {
                    let earliest_ei = entry_idx_for_us(entries, target_us);
                    let earliest_log_idx = entries[earliest_ei].log_idx;
                    player.seek_to(log_entries, earliest_log_idx, snapshots);
                }

                let mut cursor = pb.current_us;
                loop {
                    let next = (cursor as i64 + sign * STEP_US as i64)
                        .clamp(0, pb.total_us as i64) as u64;
                    let past_target = if sign > 0 { next >= target_us } else { next <= target_us };
                    if past_target {
                        break;
                    }
                    let ei = entry_idx_for_us(entries, next);
        perform_seek(ei, entries, log_entries, player, pb, observer, client, shadow_tx, snapshots, house_cache).await?;
                    pb.transition(PlaybackTransition::Seek { target_us: next, entry_idx: ei });
                    tokio::time::sleep(INTERMEDIATE_PAUSE).await;
                    cursor = next;
                }
            }

            let target_ei = entry_idx_for_us(entries, target_us);
            perform_seek(target_ei, entries, log_entries, player, pb, observer, client, shadow_tx, snapshots, house_cache).await?;
            show_gump = pb.transition(PlaybackTransition::Seek {
                target_us,
                entry_idx: target_ei,
            });
            if !pb.paused {
                pb.start += Duration::from_millis(800);
            }
        }

        Handled::FastForward(delta_ms) => {
            let delta_us = delta_ms * 1_000; // convert ms → µs
            let target_us = pb.clamp_target(delta_us);
            info!(
                "[replay] fast-forward {} ms: {}µs → {}µs / {}µs{}",
                delta_ms, pb.current_us, target_us, pb.total_us,
                if pb.paused { " (paused)" } else { "" },
            );

            show_gump = pb.transition(PlaybackTransition::StartFastForward {
                target_us,
            });
            info!(
                "[replay] fast-forward x{} to {}µs",
                FAST_FORWARD_SPEED, target_us,
            );
        }

        Handled::StepPacket(dir) => {
            step_to_entry(
                dir, |_| true, "packet",
                entries, log_entries, player, pb, observer, client, shadow_tx, cmds,
                snapshots, house_cache,
            ).await?;
        }

        Handled::StepClientPacket(dir) => {
            step_to_entry(
                dir, |e| e.kind.is_client(), "client packet",
                entries, log_entries, player, pb, observer, client, shadow_tx, cmds,
                snapshots, house_cache,
            ).await?;
        }

        Handled::StepServerPacket(dir) => {
            step_to_entry(
                dir, |e| !e.kind.is_client(), "server packet",
                entries, log_entries, player, pb, observer, client, shadow_tx, cmds,
                snapshots, house_cache,
            ).await?;
        }

        Handled::Yes | Handled::ReshowActionMenu | Handled::RestartReplay => {}

        Handled::No => {
            return Ok(CommandResult::ProcessNormally(packet));
        }
    }

    // ── Uniform gump refresh ─────────────────────────────────────────
    // Step* branches send the gump themselves (via step_to_entry);
    // all other branches delegate to this single block.
    if show_gump {
        cmds.send_playback_control_gump(
            pb.pos.serial, pb.current_us / 1_000, pb.total_us / 1_000, pb.paused, client,
        ).await?;
    }

    Ok(CommandResult::Continue)
}

// ── Entry dispatch ───────────────────────────────────────────────────────

/// Send a single timed entry to the client, updating `pb.pos` / `diorama.session`.
async fn dispatch_entry(
    entry: &ReplayEntry,
    idx: usize,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    shadow_tx: &ShadowTx,
    house_cache: &HashMap<u32, Bytes>,
) -> fw_error::Result<()> {
    // ── Observer mode: delegate to specialised handler ────────────────
    if let ViewMode::Observer { player_serial, .. } = &pb.view_mode {
        return dispatch_entry_observer(
            entry, idx, pb, *player_serial, observer, client, shadow_tx, house_cache,
        ).await;
    }

    // ── FirstPerson mode: original logic ─────────────────────────────
    match &entry.kind {
        EntryKind::Forward(data) => {
            // Intercept 0xBF sub 0x001D (HouseRevisionState).  The original
            // server sent this to tell the client "house X has revision Y".
            // If we have the 0xD8 design data cached, forward the revision
            // state so the client can request the design via 0xBF:001E —
            // we respond from the cache in `handle_client_packet`.
            // If we DON'T have the design, suppress the packet entirely —
            // otherwise the client switches to "custom house" rendering
            // and shows an empty shell.
            if data.len() >= 13 && data[0] == 0xBF {
                let sub = u16::from_be_bytes([data[3], data[4]]);
                if sub == 0x001D {
                    let serial = u32::from_be_bytes([data[5], data[6], data[7], data[8]]);
                    let revision = u32::from_be_bytes([data[9], data[10], data[11], data[12]]);
                    if house_cache.contains_key(&serial) {
                        // Forward the revision state — the client will
                        // request the 0xD8 design data if it needs it.
                        debug!(
                            "[playback] 0xBF:001D HouseRevisionState serial={:#010X} rev={} — forwarding (0xD8 available on request)",
                            serial, revision,
                        );
                        // Fall through to the normal Forward path below.
                    } else {
                        debug!(
                            "[playback] 0xBF:001D HouseRevisionState serial={:#010X} rev={} — suppressed (no 0xD8 in cache)",
                            serial, revision,
                        );
                        return Ok(());
                    }
                }
            }

            pb.pos.update_from_packet(data);
            observer.session.ingest_packet(data);
            client.send(RawPacket::s2c(data.clone())).await?;

            // ── Diagnostic: log spawn/delete/setmap packets ──────────
            if !data.is_empty() {
                match data[0] {
                    0x1A => {
                        // ObjectInfo — item/multi spawn
                        if data.len() >= 5 {
                            let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]) & 0x7FFF_FFFF;
                            if let Ok(obj) = packets::world::ObjectInfo::from_bytes(data) {
                                trace!(
                                    "[dispatch] FWD 0x1A ObjectInfo serial={:#010X} graphic={:#06X} at ({},{},{}) world={} seeked={} visible={}",
                                    serial, obj.graphic, obj.x, obj.y, obj.z,
                                    observer.session.current_world, pb.seeked, observer.session.visible.len(),
                                );
                            } else {
                                trace!(
                                    "[dispatch] FWD 0x1A ObjectInfo serial={:#010X} (parse failed) world={} seeked={} visible={}",
                                    serial, observer.session.current_world, pb.seeked, observer.session.visible.len(),
                                );
                            }
                        }
                    }
                    0x78 => {
                        // DrawMobile — mobile spawn
                        if data.len() >= 5 {
                            let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                            if let Ok(mob) = packets::world::DrawMobile::parse(data, false) {
                                trace!(
                                    "[dispatch] FWD 0x78 DrawMobile serial={:#010X} graphic={:#06X} at ({},{},{}) world={} seeked={} visible={}",
                                    serial, mob.graphic, mob.x, mob.y, mob.z,
                                    observer.session.current_world, pb.seeked, observer.session.visible.len(),
                                );
                            } else {
                                trace!(
                                    "[dispatch] FWD 0x78 DrawMobile serial={:#010X} (parse failed) world={} seeked={} visible={}",
                                    serial, observer.session.current_world, pb.seeked, observer.session.visible.len(),
                                );
                            }
                        }
                    }
                    0xF3 => {
                        // ObjectInfoSA — item/multi spawn (SA+)
                        if data.len() >= 7 {
                            let serial = u32::from_be_bytes([data[2], data[3], data[4], data[5]]);
                            // Parse coordinates and graphic for diagnostics.
                            if let Ok(obj) = packets::world::ObjectInfoSA::from_bytes(data) {
                                trace!(
                                    "[dispatch] FWD 0xF3 ObjectInfoSA serial={:#010X} graphic={:#06X} at ({},{},{}) world={} seeked={} visible={}",
                                    serial, obj.graphic, obj.x, obj.y, obj.z,
                                    observer.session.current_world, pb.seeked, observer.session.visible.len(),
                                );
                            } else {
                                trace!(
                                    "[dispatch] FWD 0xF3 ObjectInfoSA serial={:#010X} (parse failed) world={} seeked={} visible={}",
                                    serial, observer.session.current_world, pb.seeked, observer.session.visible.len(),
                                );
                            }
                        }
                    }
                    0xF7 => {
                        // PacketList — batch item spawn
                        trace!(
                            "[dispatch] FWD 0xF7 PacketList ({} bytes) world={} seeked={} visible={}",
                            data.len(), observer.session.current_world, pb.seeked, observer.session.visible.len(),
                        );
                    }
                    0x1D => {
                        // DeleteObject
                        if data.len() >= 5 {
                            let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                            trace!(
                                "[dispatch] FWD 0x1D DeleteObject serial={:#010X} world={} seeked={} visible={}",
                                serial, observer.session.current_world, pb.seeked, observer.session.visible.len(),
                            );
                        }
                    }
                    0xE2 => {
                        // DrawMobileExtended
                        if data.len() >= 5 {
                            let serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                            trace!(
                                "[dispatch] FWD 0xE2 DrawMobileExt serial={:#010X} world={} seeked={} visible={}",
                                serial, observer.session.current_world, pb.seeked, observer.session.visible.len(),
                            );
                        }
                    }
                    _ => {}
                }
            }

            // Mirror entity changes (spawn/update/delete) into the shadow
            // continuum so items persist when the player walks away and back.
            let _ = shadow_tx.send(WorkerCommand::MapCommand(
                observer.session.current_world,
                ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
            )).await;

            // Mirror container-related packets (0x24, 0x25, 0x3C) into
            // the zone's container store so we can serve them on
            // DoubleClick during free-move.
            if !data.is_empty() && matches!(data[0], 0x24 | 0x25 | 0x3C) {
                debug!(
                    "[dispatch] container packet 0x{:02X} ({} bytes) → continuum world={}",
                    data[0], data.len(), observer.session.current_world,
                );
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.ingest_container(data.clone()).await;
            }

            // Detect 0xBF sub 0x0008 (SetMap) — the player is
            // transitioning to a different world/facet.  The zone for the
            // new world is auto-created by the worker's factory if it
            // doesn't exist yet.  We do NOT reset the zone here — entities
            // accumulated in each world's zone are preserved across world
            // transitions and will be populated via IngestPacket as the
            // replay progresses.
            if data.len() >= 6 && data[0] == 0xBF {
                let sub = u16::from_be_bytes([data[3], data[4]]);
                if sub == 0x0008 {
                    let new_world = data[5];
                    info!(
                        "[playback] 0xBF:0008 SetMap — switching to world {}, session view_range={} visible_count={}",
                        new_world, observer.view_range(), observer.session.visible.len(),
                    );
                }
            }
        }

        EntryKind::WorldInit(data) => {
            if pb.seeked && observer.session.current_world != pb.seeked_world {
                // World changed since the seek — the client needs these
                // init packets (0x20 DrawGamePlayer with correct coords,
                // 0x55 LoginComplete, etc.) to properly enter the new
                // world.  Reset seeked and forward as-is.
                info!(
                    "[dispatch] WorldInit 0x{:02X} — world changed ({} → {}), resetting seeked, forwarding as-is",
                    data.first().copied().unwrap_or(0),
                    pb.seeked_world, observer.session.current_world,
                );
                pb.seeked = false;
                pb.pos.update_from_packet(data);
                observer.session.ingest_packet(data);
                client.send(RawPacket::s2c(data.clone())).await?;

                // Mirror entity changes into the shadow continuum.
                let _ = shadow_tx.send(WorkerCommand::MapCommand(
                    observer.session.current_world,
                    ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
                )).await;
            } else if pb.seeked {
                // Same world as seek — suppress or replace to avoid
                // resetting client UI.
                if !data.is_empty() && data[0] == LoginComplete::ID {
                    debug!(
                        "[dispatch] WorldInit 0x55 LoginComplete SUPPRESSED (seeked=true → false) world={}",
                        observer.session.current_world,
                    );
                    pb.seeked = false;
                } else {
                    debug!(
                        "[dispatch] WorldInit 0x{:02X} REPLACED with DrawGamePlayer (seeked=true) world={} pos=({},{},{})",
                        data.first().copied().unwrap_or(0),
                        observer.session.current_world, pb.pos.x, pb.pos.y, pb.pos.z,
                    );
                    // 0x1B/0x20: keep non-coordinate fields, restore seek coords.
                    let seek_xyz = (pb.pos.x, pb.pos.y, pb.pos.z);
                    pb.pos.update_from_packet(data);
                    pb.pos.x = seek_xyz.0;
                    pb.pos.y = seek_xyz.1;
                    pb.pos.z = seek_xyz.2;
                    if pb.pos.is_ready() {
                        client.send(RawPacket::s2c(
                            pb.pos.to_draw_game_player().to_bytes()
                        )).await?;
                    }
                }
            } else {
                debug!(
                    "[dispatch] WorldInit 0x{:02X} forwarded (seeked=false) world={}",
                    data.first().copied().unwrap_or(0), observer.session.current_world,
                );
                pb.pos.update_from_packet(data);
                observer.session.ingest_packet(data);
                client.send(RawPacket::s2c(data.clone())).await?;

                // Mirror entity changes into the shadow continuum.
                let _ = shadow_tx.send(WorkerCommand::MapCommand(
                    observer.session.current_world,
                    ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
                )).await;
            }
        }

        EntryKind::MoveAck { direction } => {
            let before = (pb.pos.x, pb.pos.y, pb.pos.z, pb.pos.facing);
            let moved = pb.pos.step(*direction);

            if moved {
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                if let Some(new_z) = engine.resolve_z(
                    pb.pos.x, pb.pos.y, pb.pos.z, direction.heading(),
                ).await {
                    pb.pos.z = new_z;
                }
            }

            trace!(
                "[playback] {} #{idx} dir={} ({},{},{}) → ({},{},{}){}",
                if moved { "step" } else { "turn" },
                direction,
                before.0, before.1, before.2,
                pb.pos.x, pb.pos.y, pb.pos.z,
                if moved && pb.pos.z != before.2 { " (z adjusted)" } else { "" },
            );
            // Periodic position + visible count log (every 10 steps).
            if moved && idx % 10 == 0 {
                debug!(
                    "[dispatch] MoveAck #{idx} pos=({},{},{}) world={} seeked={} visible={}",
                    pb.pos.x, pb.pos.y, pb.pos.z,
                    observer.session.current_world, pb.seeked, observer.session.visible.len(),
                );
            }
            if pb.pos.is_ready() {
                client.send(RawPacket::s2c(
                    pb.pos.to_draw_game_player().to_bytes()
                )).await?;
            } else {
                debug!("[playback] #{idx}: serial=0, skipping DrawGamePlayer");
            }
        }

        EntryKind::ClientPacket(_) => {
            // C→S packets are stored for packet-stepping but not forwarded.
        }
    }
    Ok(())
}

// ── Observer dispatch ─────────────────────────────────────────────────────

/// Observer-mode variant of [`dispatch_entry`].
///
/// Key differences from FirstPerson:
///
/// - **`MoveAck`**: the recorded character's `replay_pos` is stepped and an
///   `UpdateMobile (0x77)` is sent for the NPC.  The observer's `pos` and
///   camera are **not** moved.
///
/// - **`Forward(0x20)` (DrawGamePlayer)**: intercepted — updates `replay_pos`
///   and sends `UpdateMobile` for the NPC instead of forwarding.
///
/// - **`Forward(0x77)` for player_serial**: updates `replay_pos`; the raw
///   packet is forwarded so the NPC updates visually.
///
/// - **`WorldInit(0x1B/0x20)`**: the original is consumed for `replay_pos`;
///   a `DrawGamePlayer` for the **observer** is sent instead (keeping the
///   camera on the observer), plus an `UpdateMobile` for the NPC.
///
/// - All other packets are forwarded unchanged (speech, item spawns, etc.).
async fn dispatch_entry_observer(
    entry: &ReplayEntry,
    idx: usize,
    pb: &mut PlaybackState,
    player_serial: u32,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    shadow_tx: &ShadowTx,
    house_cache: &HashMap<u32, Bytes>,
) -> fw_error::Result<()> {
    match &entry.kind {
        EntryKind::Forward(data) => {
            // ── House revision state filter (same as FirstPerson) ─────
            if data.len() >= 13 && data[0] == 0xBF {
                let sub = u16::from_be_bytes([data[3], data[4]]);
                if sub == 0x001D {
                    let serial = u32::from_be_bytes([data[5], data[6], data[7], data[8]]);
                    if !house_cache.contains_key(&serial) {
                        return Ok(());
                    }
                }
            }

            let pkt_id = data[0];

            // ── 0x20 DrawGamePlayer — intercept for recorded char ─────
            if pkt_id == DrawGamePlayer::ID {
                // Update replay_pos from the packet.
                pb.replay_pos.update_from_packet(data);
                // Send UpdateMobile for the NPC so the client sees it move.
                if let Some(upd) = pb.view_mode.build_replay_char_update(&pb.replay_pos) {
                    client.send(RawPacket::s2c(upd)).await?;
                }
                // Do NOT forward the original 0x20 — it would hijack the camera.
                // Mirror into the shadow continuum.
                let _ = shadow_tx.send(WorkerCommand::MapCommand(
                    observer.session.current_world,
                    ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
                )).await;
                return Ok(());
            }

            // ── 0x77 UpdateMobile for the recorded char ───────────────
            if pkt_id == UpdateMobile::ID && data.len() >= 5 {
                let pkt_serial = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                if pkt_serial == player_serial {
                    pb.replay_pos.update_from_packet(data);
                    // Forward the raw 0x77 — the NPC updates visually.
                    client.send(RawPacket::s2c(data.clone())).await?;
                    observer.session.ingest_packet(data);
                    let _ = shadow_tx.send(WorkerCommand::MapCommand(
                        observer.session.current_world,
                        ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
                    )).await;
                    return Ok(());
                }
            }

            // ── All other Forward packets — pass through unchanged ────
            // Do NOT call pb.pos.update_from_packet for packets addressed
            // to the recorded character (observer stays put).
            observer.session.ingest_packet(data);
            client.send(RawPacket::s2c(data.clone())).await?;

            // Mirror into shadow continuum.
            let _ = shadow_tx.send(WorkerCommand::MapCommand(
                observer.session.current_world,
                ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
            )).await;

            // Container mirroring.
            if !data.is_empty() && matches!(data[0], 0x24 | 0x25 | 0x3C) {
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.ingest_container(data.clone()).await;
            }
        }

        EntryKind::WorldInit(data) => {
            let pkt_id = data.first().copied().unwrap_or(0);

            if pb.seeked && observer.session.current_world != pb.seeked_world {
                // World changed since seek — forward init packets as-is
                // but remap 0x1B/0x20 serial for the observer.
                pb.seeked = false;
                pb.replay_pos.update_from_packet(data);
                observer.session.ingest_packet(data);

                // For 0x1B/0x20, send observer variant + NPC update.
                if pkt_id == 0x1B || pkt_id == DrawGamePlayer::ID {
                    if pb.pos.is_ready() {
                        client.send(RawPacket::s2c(
                            pb.pos.to_draw_game_player().to_bytes()
                        )).await?;
                    }
                    if let Some(upd) = pb.view_mode.build_replay_char_update(&pb.replay_pos) {
                        client.send(RawPacket::s2c(upd)).await?;
                    }
                } else {
                    client.send(RawPacket::s2c(data.clone())).await?;
                }

                let _ = shadow_tx.send(WorkerCommand::MapCommand(
                    observer.session.current_world,
                    ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
                )).await;
            } else if pb.seeked {
                // Same world as seek — suppress or replace.
                if pkt_id == LoginComplete::ID {
                    pb.seeked = false;
                } else {
                    // Update replay_pos from the packet data but keep
                    // observer pos unchanged.
                    pb.replay_pos.update_from_packet(data);
                    // Send observer DrawGamePlayer (camera stays).
                    if pb.pos.is_ready() {
                        client.send(RawPacket::s2c(
                            pb.pos.to_draw_game_player().to_bytes()
                        )).await?;
                    }
                    // Send NPC update.
                    if let Some(upd) = pb.view_mode.build_replay_char_update(&pb.replay_pos) {
                        client.send(RawPacket::s2c(upd)).await?;
                    }
                }
            } else {
                // Normal flow (not seeked).
                pb.replay_pos.update_from_packet(data);

                if pkt_id == 0x1B || pkt_id == DrawGamePlayer::ID {
                    // Send observer identity to the client.
                    if pb.pos.is_ready() {
                        client.send(RawPacket::s2c(
                            pb.pos.to_draw_game_player().to_bytes()
                        )).await?;
                    }
                    // Send NPC update for the recorded char.
                    if let Some(upd) = pb.view_mode.build_replay_char_update(&pb.replay_pos) {
                        client.send(RawPacket::s2c(upd)).await?;
                    }
                } else {
                    // 0x55 LoginComplete etc. — forward as-is.
                    observer.session.ingest_packet(data);
                    client.send(RawPacket::s2c(data.clone())).await?;
                }

                let _ = shadow_tx.send(WorkerCommand::MapCommand(
                    observer.session.current_world,
                    ReplayCommand::IngestPacket { data: data.clone(), emit_events: false },
                )).await;
            }
        }

        EntryKind::MoveAck { direction } => {
            // Step the recorded character (NPC), not the observer.
            let before = (pb.replay_pos.x, pb.replay_pos.y, pb.replay_pos.z);
            let moved = pb.replay_pos.step(*direction);

            if moved {
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                if let Some(new_z) = engine.resolve_z(
                    pb.replay_pos.x, pb.replay_pos.y, pb.replay_pos.z,
                    direction.heading(),
                ).await {
                    pb.replay_pos.z = new_z;
                }
            }

            trace!(
                "[playback:obs] {} #{idx} dir={} ({},{},{}) → ({},{},{}){}",
                if moved { "step" } else { "turn" },
                direction,
                before.0, before.1, before.2,
                pb.replay_pos.x, pb.replay_pos.y, pb.replay_pos.z,
                if moved && pb.replay_pos.z != before.2 { " (z adjusted)" } else { "" },
            );

            // Send UpdateMobile for the NPC.
            if let Some(upd) = pb.view_mode.build_replay_char_update(&pb.replay_pos) {
                client.send(RawPacket::s2c(upd)).await?;
            }
            // Sync the recorded character's entity in the shadow continuum
            // so that visible-item queries (observer movement, pause, seek,
            // free-move) return the correct position instead of the stale
            // one from the last IngestPacket (0x20/0x77).
            if let ViewMode::Observer { player_serial, .. } = &pb.view_mode {
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.teleport(
                    *player_serial, pb.replay_pos.x, pb.replay_pos.y, pb.replay_pos.z,
                    Some(pb.replay_pos.facing.raw()),
                ).await;
            }
            // Observer's camera (pb.pos) is NOT moved.
        }

        EntryKind::ClientPacket(_) => {
            // C→S packets are stored for packet-stepping but not forwarded.
        }
    }
    Ok(())
}

// ── run_playback ─────────────────────────────────────────────────────────

pub async fn run_playback(
    client: &mut Session,
    log_entries: &[LogEntry],
    entries: &[ReplayEntry],
    init_packets: Option<&[Bytes]>,
    observer: &mut ObserverPipeline,
    shadow_tx: &ShadowTx,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
    static_data: Option<Arc<StaticWorldData>>,
    view_mode: &ViewMode,
) -> fw_error::Result<()> {
    use tokio::time::{Instant, sleep_until};
    let mut cmds = DotCommands::new();
    let mut player = LogPlayer::new(static_data);

    if entries.is_empty() {
        info!("[replay] no entries to replay");
        return Ok(());
    }

    // Send init packets (bootstrap) if provided.  On restart they are
    // None — the client is already in the world.
    if let Some(pkts) = init_packets {
        debug!("[replay] sending {} init packets (bootstrap)", pkts.len());
        for pkt in pkts {
            // Skip stale DrawGamePlayer (0x20) — it may carry coordinates
            // from before the player's origin 0x78, causing the client to
            // briefly teleport to the wrong location.
            if !pkt.is_empty() && pkt[0] == 0x20 {
                trace!("[replay] skipping 0x20 DrawGamePlayer in init_packets");
                continue;
            }
            // Let the session observe init packets so it picks up state
            // set during the pre-world/pre-origin phases — most importantly
            // S→C 0xC8 ClientViewRange (the client and server negotiate
            // the view range once at login; modern clients may use up to
            // 24 tiles instead of the classic 18).
            observer.session.ingest_packet(pkt);
            client.send(RawPacket::s2c(pkt.clone())).await?;
        }
    }

    let total_us = entries.last().map(|e| e.us_offset).unwrap_or(0);

    // Dispatch initial entries up to and including LoginComplete (0x55)
    // before showing the playback gump.  On OSI shards LoginComplete
    // arrives *after* the origin DrawMobile (0x78), so it ends up in
    // `entries` rather than `init_packets`.  The classic client ignores
    // gumps received before LoginComplete, causing the playback control
    // to never appear.  (Orion buffers them and shows them later.)
    //
    // LoginComplete may arrive with a non-zero us_offset (a few ms after
    // the origin 0x78), so we scan up to 2 seconds into the timeline
    // instead of only us_offset == 0 entries.
    const PRE_GUMP_MAX_US: u64 = 2_000_000; // 2 seconds
    let mut start_idx: usize = 1; // entry 0 is our origin 0x78, already sent via init_packets
    let pre_gump_t0 = Instant::now();
    let mut pre_gump_sent: usize = 0;
    let mut pre_gump_found_login_complete = false;
    let is_observer = view_mode.is_observer();
    for (i, entry) in entries.iter().enumerate().skip(1) {
        if entry.us_offset > PRE_GUMP_MAX_US {
            break;
        }
        match &entry.kind {
            EntryKind::Forward(data) | EntryKind::WorldInit(data) => {
                if is_observer {
                    // In Observer mode, use the observer dispatch path for
                    // us_offset==0 entries so that 0x20 DrawGamePlayer is
                    // intercepted and observer.pos.serial is preserved.
                    // However, `dispatch_entry_observer` needs a full
                    // PlaybackState which isn't created yet.  Instead,
                    // handle the critical cases inline:
                    let pkt_id = data.first().copied().unwrap_or(0);
                    if pkt_id == packets::character::DrawGamePlayer::ID {
                        // Intercept: update replay_pos (not observer pos),
                        // do NOT send to client (would hijack camera).
                    } else {
                        observer.session.ingest_packet(data);
                        client.send(RawPacket::s2c(data.clone())).await?;
                    }
                } else {
                    observer.pos.update_from_packet(data);
                    observer.session.ingest_packet(data);
                    client.send(RawPacket::s2c(data.clone())).await?;
                }
                pre_gump_sent += 1;
            }
            _ => {}
        }
        start_idx = i + 1;
        // Stop after LoginComplete — the client is now fully in game.
        if matches!(&entry.kind, EntryKind::WorldInit(d) if !d.is_empty() && d[0] == LoginComplete::ID)
        {
            pre_gump_found_login_complete = true;
            break;
        }
    }
    debug!(
        "[replay] pre-gump dispatch: {} packets sent in {:?}, LoginComplete {}",
        pre_gump_sent,
        pre_gump_t0.elapsed(),
        if pre_gump_found_login_complete { "found" } else { "NOT found (was in init_packets)" },
    );

    cmds.send_playback_control_gump(observer.pos.serial, 0, total_us / 1_000, false, client).await?;

    let mut pb = PlaybackState {
        pos: observer.pos,
        start: Instant::now(),
        idx: start_idx,
        current_us: 0,
        total_us,
        seeked: false,
        seeked_world: 0,
        paused: false,
        pause_start: None,
        speed: 1.0,
        ff_target_us: None,
        saved_pause: None,
        view_mode: view_mode.clone(),
        // In Observer mode, replay_pos starts at the same position as
        // the observer (they're co-located at replay start).  It will
        // diverge as MoveAck entries are dispatched.
        replay_pos: observer.pos,
    };

    'playback: loop {
        // ── Paused: wait for client events only (no timer) ───────────
        if pb.paused {
            match client.recv().await.event {
                SessionEvent::Packet(p) => {
                    observer.pos = pb.pos;
                    let handled = cmds.handle_packet(&p, client, observer, shadow_tx).await?;
                    pb.pos = observer.pos;
                    match handle_playback_command(
                        handled, p, &entries, log_entries,
                        &mut player, &mut pb, observer, client, shadow_tx, &mut cmds,
                        snapshots, house_cache,
                    ).await? {
                        CommandResult::StopPlayback => break 'playback,
                        CommandResult::ProcessNormally(p) => {
                                let prev_xy = (pb.pos.x, pb.pos.y);
                                observer.pos = pb.pos;
                                let allow_move = pb.view_mode.is_observer();
                                handle_client_packet(client, p, allow_move, observer, shadow_tx, house_cache, None).await?;
                                pb.pos = observer.pos;

                                // In Observer mode, stream visible items when
                                // the observer walks during playback.
                                if allow_move && (pb.pos.x, pb.pos.y) != prev_xy {
                                    let world = observer.session.current_world;
                                    let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
                                    let new_strips = observer.session.visible.update_view(pb.pos.x, pb.pos.y);
                                    for strip in &new_strips {
                                        let items = engine.items_in_area(*strip).await;
                                        for raw in items {
                                            observer.session.ingest_packet(&raw);
                                            client.send(RawPacket::s2c(raw)).await?;
                                        }
                                    }
                                    observer.session.sweep_stale();
                                }
                        }
                        CommandResult::Continue => {}
                    }
                }
                SessionEvent::Stopped | SessionEvent::Disconnected => break 'playback,
                SessionEvent::Error(e) => return Err(e.into()),
                _ => {}
            }
            continue;
        }

        // ── Playing: compute next deadline, skip ClientPacket entries ─
        while pb.idx < entries.len() && matches!(entries[pb.idx].kind, EntryKind::ClientPacket(_)) {
            pb.idx += 1;
        }
        let Some(deadline) = (pb.idx < entries.len())
            .then(|| {
                let secs = entries[pb.idx].us_offset as f64 / 1_000_000.0 / pb.speed;
                pb.start + Duration::from_secs_f64(secs.max(0.0))
            })
        else {
            // Reached the end of recorded entries.  If we arrived here
            // during a fast-forward that was started from pause, end the
            // FF and restore pause instead of leaving playback entirely.
            if pb.ff_target_us.is_some() {
                info!(
                    "[replay] fast-forward hit end of replay at {}µs, ending FF",
                    pb.current_us,
                );
                pb.current_us = pb.total_us;
                let show_gump = pb.transition(PlaybackTransition::EndFastForward);
                if pb.paused {
                    // Pause was restored — stay in the loop.
                    if show_gump {
                        cmds.send_playback_control_gump(
                            pb.pos.serial, pb.current_us / 1_000, pb.total_us / 1_000, pb.paused, client,
                        ).await?;
                    }
                    continue;
                }
                // Was not paused before FF — fall through to break.
                if show_gump {
                    cmds.send_playback_control_gump(
                        pb.pos.serial, pb.current_us / 1_000, pb.total_us / 1_000, pb.paused, client,
                    ).await?;
                }
            }
            break 'playback;
        };

        tokio::select! {
            biased; // client events take priority over the timer

            event = client.recv() => {
                match event.event {
                    SessionEvent::Packet(p) => {
                        observer.pos = pb.pos;
                        let handled = cmds.handle_packet(&p, client, observer, shadow_tx).await?;
                        pb.pos = observer.pos;
                        match handle_playback_command(
                            handled, p, &entries, log_entries,
                            &mut player, &mut pb, observer, client, shadow_tx, &mut cmds,
                            snapshots, house_cache,
                        ).await? {
                            CommandResult::StopPlayback => break 'playback,
                            CommandResult::ProcessNormally(p) => {
                            let prev_xy = (pb.pos.x, pb.pos.y);
                            observer.pos = pb.pos;
                            let allow_move = pb.view_mode.is_observer();
                            handle_client_packet(client, p, allow_move, observer, shadow_tx, house_cache, None).await?;
                            pb.pos = observer.pos;

                            // Stream visible items when observer moves during playback.
                            if allow_move && (pb.pos.x, pb.pos.y) != prev_xy {
                                let world = observer.session.current_world;
                                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world);
                                let new_strips = observer.session.visible.update_view(pb.pos.x, pb.pos.y);
                                for strip in &new_strips {
                                    let items = engine.items_in_area(*strip).await;
                                    for raw in items {
                                        observer.session.ingest_packet(&raw);
                                        client.send(RawPacket::s2c(raw)).await?;
                                    }
                                }
                                observer.session.sweep_stale();
                            }
                            }
                            CommandResult::Continue => {}
                        }
                    }
                    SessionEvent::Stopped | SessionEvent::Disconnected => break 'playback,
                    SessionEvent::Error(e) => return Err(e.into()),
                    _ => {}
                }
            }

            _ = sleep_until(deadline) => {
                pb.current_us = entries[pb.idx].us_offset;
                dispatch_entry(&entries[pb.idx], pb.idx, &mut pb, observer, client, shadow_tx, house_cache).await?;
                pb.idx += 1;

                // Check if fast-forward target has been reached.
                if let Some(target) = pb.ff_target_us {
                    if pb.current_us >= target {
                        info!("[replay] fast-forward reached target {}µs, resuming normal speed", target);
                        let show_gump = pb.transition(PlaybackTransition::EndFastForward);
                        if show_gump {
                            cmds.send_playback_control_gump(
                                pb.pos.serial, pb.current_us / 1_000, pb.total_us / 1_000, pb.paused, client,
                            ).await?;
                        }
                    }
                }
            }
        }
    }

    // Close the noclose playback gump so it doesn't linger on screen.
    cmds.close_playback_gump(client).await?;

    // Sync the diorama position from playback state.
    observer.pos = pb.pos;

    Ok(())
}

