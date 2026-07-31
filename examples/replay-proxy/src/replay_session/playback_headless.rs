//! Headless playback — no gumps, no shadow continuum.
//!
//! These functions mirror the gump-based playback in [`super::playback`] but
//! are driven by an external command channel (e.g. a web UI) and do not
//! require the shadow continuum worker.
//!
//! Every S→C, C→S, and synthesized packet is also logged to a broadcast
//! channel so that external consumers (web UI packet inspector) can observe
//! the full packet stream in real time.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use log::{debug, info, trace};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

use u_core::PacketDirection;
use framework::diorama::ObserverPipeline;
use framework::ecumene::StaticWorldData;
use network::error as fw_error;
use network::session::{Session, SessionEvent};
use packets::system::{ClientViewRange, LoginComplete};
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

use packets::registry::{DecodedResult, OutputFormat, PacketRegistry};

use crate::log_player::{LogPlayer, LogPlayerSnapshot};
use crate::packet_log::LogEntry;

use super::{EntryKind, ReplayEntry};
use super::playback::{
    PlaybackState, PlaybackTransition,
    entry_idx_for_us, find_step_target, log_entry_description,
};

// ── Packet log types ─────────────────────────────────────────────────────

/// Where a packet came from / is going to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketSource {
    /// S→C packet from the `.uolog`, forwarded to the UO client.
    ReplayServer,
    /// C→S packet from the `.uolog` (original client actions in the recorded session).
    ReplayClient,
    /// C→S packet from the **current** live UO client (MoveRequest, ViewRange, etc.).
    LiveClient,
    /// Synthesized by the replay continuum (DrawGamePlayer from MoveAck, resync items, etc.).
    Synthesized,
}

/// A single packet observation, broadcast to web UI consumers.
#[derive(Debug, Clone, Serialize)]
pub struct PacketLogEntry {
    /// Timeline position in the replay (µs from start).
    /// `None` for live-client and some synthesized packets that have no
    /// meaningful replay-timeline position.
    pub timestamp_us: Option<u64>,
    /// Where this packet came from.
    pub source: PacketSource,
    /// Packet ID as hex string, e.g. `"0x78"`.
    pub id: String,
    /// Raw packet length in bytes.
    pub len: usize,
    /// Human-readable decoded description (via `PacketRegistry`).
    pub desc: String,
    /// Raw bytes as space-separated hex, e.g. `"78 00 2A 00 ..."`.
    pub hex: String,
}

// ── Headless channels ────────────────────────────────────────────────────

/// Bundled communication channels for headless playback.
///
/// Grouping them into a struct avoids threading 3+ extra parameters through
/// every helper function.
pub struct HeadlessChannels {
    /// Incoming commands from the external anima.
    pub command_rx: mpsc::Receiver<PlaybackCommand>,
    /// Outgoing playback-status updates.
    pub status_tx: broadcast::Sender<PlaybackStatus>,
    /// Outgoing packet log entries.
    pub packet_log_tx: broadcast::Sender<PacketLogEntry>,
}

// ── Helper: decode + broadcast a packet ──────────────────────────────────

/// Decode a raw UO packet and push a [`PacketLogEntry`] into the broadcast
/// channel.  Errors (no subscribers, closed) are silently ignored.
fn log_packet(
    tx: &broadcast::Sender<PacketLogEntry>,
    reg: &PacketRegistry,
    data: &[u8],
    source: PacketSource,
    direction: PacketDirection,
    timestamp_us: Option<u64>,
) {
    if data.is_empty() {
        return;
    }
    let id_byte = data[0];
    let hex = data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
    let desc = match reg.decode(id_byte, data, direction, OutputFormat::Debug) {
        DecodedResult::Ok(decoded) => decoded.into_string(),
        DecodedResult::DecodeError(e) => format!("[decode error: {e}]"),
        DecodedResult::Unknown => String::new(),
    };
    let _ = tx.send(PacketLogEntry {
        timestamp_us,
        source,
        id: format!("0x{id_byte:02X}"),
        len: data.len(),
        desc,
        hex,
    });
}

// ── Playback status / command types ──────────────────────────────────────

/// Snapshot of playback state — sent to external controllers (web UI)
/// whenever the state changes.
#[derive(Debug, Clone, Serialize)]
pub struct PlaybackStatus {
    /// Current position in the timeline (microseconds).
    pub current_us: u64,
    /// Total duration of the replay (microseconds).
    pub total_us: u64,
    /// Index of the next entry to dispatch.
    pub idx: usize,
    /// Total number of replay entries.
    pub total_entries: usize,
    /// Whether playback is paused.
    pub paused: bool,
    /// Current playback speed multiplier.
    pub speed: f64,
    /// Whether playback has finished (reached end of log).
    pub finished: bool,
}

impl PlaybackStatus {
    fn from_state(pb: &PlaybackState, total_entries: usize, finished: bool) -> Self {
        Self {
            current_us: pb.current_us,
            total_us: pb.total_us,
            idx: pb.idx,
            total_entries,
            paused: pb.paused,
            speed: pb.speed,
            finished,
        }
    }
}

/// Commands that an external anima can send to drive headless playback.
///
/// Time values in `SeekAbsolute`, `SeekRelative`, and `FastForward` are
/// expressed in **milliseconds** (the natural unit for external callers).
/// The handler converts to microseconds internally.
#[derive(Debug, Clone)]
pub enum PlaybackCommand {
    /// Toggle between paused and playing.
    TogglePause,
    /// Seek to an absolute timeline position (ms).
    SeekAbsolute(u64),
    /// Seek by a relative delta (ms, negative = rewind).
    SeekRelative(i64),
    /// Step by `count` entries (negative = backward, positive = forward).
    StepPacket(i32),
    /// Step by `count` client (C→S) entries.
    StepClientPacket(i32),
    /// Step by `count` server (S→C) entries.
    StepServerPacket(i32),
    /// Begin fast-forward by `delta_ms` from current position (ms).
    FastForward(i64),
    /// Stop playback (the loop exits, returning control to the caller).
    Stop,
    /// Restart playback from the beginning.
    Restart,
    /// Set playback speed multiplier (1.0 = normal).
    SetSpeed(f64),
}

/// Outcome of [`run_playback_headless`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessPlaybackResult {
    /// Playback reached the end of the log.
    Finished,
    /// An external command requested stop.
    Stopped,
    /// An external command requested restart.
    Restart,
    /// The UO client disconnected.
    Disconnected,
}

// ── perform_seek_headless ────────────────────────────────────────────────

/// Perform a seek without the shadow continuum — uses `LogPlayer`'s entity map
/// to resync the client's visible world.
async fn perform_seek_headless(
    target_entry_idx: usize,
    entries: &[ReplayEntry],
    log_entries: &[LogEntry],
    player: &mut LogPlayer,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    snapshots: &[LogPlayerSnapshot],
    _house_cache: &HashMap<u32, Bytes>,
    pkt_tx: &broadcast::Sender<PacketLogEntry>,
    reg: &PacketRegistry,
) -> fw_error::Result<()> {
    let target_log_idx = entries[target_entry_idx].log_idx;
    let target_us = entries[target_entry_idx].us_offset;

    let old_world = observer.session.current_world;
    let old_view_range = observer.view_range();
    let old_enable_features = observer.session.last_enable_features.clone();

    player.advance_to(log_entries, target_log_idx, snapshots);

    pb.pos = player.observer.pos;
    observer.session = framework::diorama::SessionView::new(0, 0, ClientViewRange::DEFAULT as u16);
    observer.session.current_world = player.observer.session.current_world;
    let new_view_range = player.observer.session.view_range();
    observer.session.visible.set_view_range(new_view_range);
    observer.session.last_enable_features = player.observer.session.last_enable_features.clone();
    pb.seeked = true;
    pb.seeked_world = observer.session.current_world;

    // If the seek crossed a world boundary, send a synthetic SetMap.
    if observer.session.current_world != old_world {
        info!(
            "[replay:headless] seek crossed world boundary: {} → {}",
            old_world, observer.session.current_world,
        );
        use packets::system::GeneralInfo;
        let set_map = GeneralInfo::SetMap { world: observer.session.current_world };
        let data = set_map.to_bytes();
        log_packet(pkt_tx, reg, &data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(target_us));
        client.send(RawPacket::s2c(data)).await?;
    }

    if new_view_range != old_view_range {
        let cvr = ClientViewRange::new(new_view_range as u8);
        let data = cvr.to_bytes();
        log_packet(pkt_tx, reg, &data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(target_us));
        client.send(RawPacket::s2c(data)).await?;
    }

    if observer.session.last_enable_features != old_enable_features {
        if let Some(ref ef_data) = observer.session.last_enable_features {
            log_packet(pkt_tx, reg, ef_data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(target_us));
            client.send(RawPacket::s2c(ef_data.clone())).await?;
        }
    }

    // Send DrawGamePlayer with current position.
    if pb.pos.is_ready() {
        let data = pb.pos.to_draw_game_player().to_bytes();
        log_packet(pkt_tx, reg, &data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(target_us));
        client.send(RawPacket::s2c(data)).await?;
    }

    // Stream visible entities from LogPlayer's entity map.
    observer.session.visible.update_view(pb.pos.x, pb.pos.y);
    let world = observer.session.current_world;
    let entities = player.take_entities_for_world(world);
    use framework::ecumene::Entity;
    let view_rect = *observer.view_rect();
    let mut sent_count = 0usize;
    for entity in &entities {
        let pos = Entity::pos(entity);
        if pos.x >= view_rect.x_min && pos.x <= view_rect.x_max
            && pos.y >= view_rect.y_min && pos.y <= view_rect.y_max
        {
            let raw = entity.to_raw_bytes();
            observer.session.ingest_packet(&raw);
            log_packet(pkt_tx, reg, &raw, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(target_us));
            client.send(RawPacket::s2c(raw)).await?;
            sent_count += 1;
        }
    }

    debug!(
        "[replay:headless] seek done — entry={} target={}µs pos ({},{}) world={} view_range={} sent {} items",
        target_entry_idx, target_us, pb.pos.x, pb.pos.y,
        observer.session.current_world, new_view_range, sent_count
    );

    Ok(())
}

// ── dispatch_entry_headless ──────────────────────────────────────────────

/// Send a single entry to the client — headless variant (no continuum mirroring).
///
/// Every packet (including C→S replay packets and synthesized MoveAck
/// responses) is logged to `pkt_tx` for the web packet inspector.
async fn dispatch_entry_headless(
    entry: &ReplayEntry,
    idx: usize,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    house_cache: &HashMap<u32, Bytes>,
    pkt_tx: &broadcast::Sender<PacketLogEntry>,
    reg: &PacketRegistry,
) -> fw_error::Result<()> {
    let ts = Some(entry.us_offset);

    match &entry.kind {
        EntryKind::Forward(data) => {
            // Intercept 0xBF sub 0x001D (HouseRevisionState).
            if data.len() >= 13 && data[0] == 0xBF {
                let sub = u16::from_be_bytes([data[3], data[4]]);
                if sub == 0x001D {
                    let serial = u32::from_be_bytes([data[5], data[6], data[7], data[8]]);
                    if !house_cache.contains_key(&serial) {
                        debug!(
                            "[playback:headless] 0xBF:001D HouseRevisionState serial={:#010X} — suppressed (no 0xD8 in cache)",
                            serial,
                        );
                        return Ok(());
                    }
                }
            }

            pb.pos.update_from_packet(data);
            observer.session.ingest_packet(data);
            log_packet(pkt_tx, reg, data, PacketSource::ReplayServer, PacketDirection::ServerToClient, ts);
            client.send(RawPacket::s2c(data.clone())).await?;
        }

        EntryKind::WorldInit(data) => {
            if pb.seeked && observer.session.current_world != pb.seeked_world {
                pb.seeked = false;
                pb.pos.update_from_packet(data);
                observer.session.ingest_packet(data);
                log_packet(pkt_tx, reg, data, PacketSource::ReplayServer, PacketDirection::ServerToClient, ts);
                client.send(RawPacket::s2c(data.clone())).await?;
            } else if pb.seeked {
                if !data.is_empty() && data[0] == LoginComplete::ID {
                    pb.seeked = false;
                } else {
                    let seek_xyz = (pb.pos.x, pb.pos.y, pb.pos.z);
                    pb.pos.update_from_packet(data);
                    pb.pos.x = seek_xyz.0;
                    pb.pos.y = seek_xyz.1;
                    pb.pos.z = seek_xyz.2;
                    if pb.pos.is_ready() {
                        let dgp = pb.pos.to_draw_game_player().to_bytes();
                        log_packet(pkt_tx, reg, &dgp, PacketSource::Synthesized, PacketDirection::ServerToClient, ts);
                        client.send(RawPacket::s2c(dgp)).await?;
                    }
                }
            } else {
                pb.pos.update_from_packet(data);
                observer.session.ingest_packet(data);
                log_packet(pkt_tx, reg, data, PacketSource::ReplayServer, PacketDirection::ServerToClient, ts);
                client.send(RawPacket::s2c(data.clone())).await?;
            }
        }

        EntryKind::MoveAck { direction } => {
            let _before = (pb.pos.x, pb.pos.y, pb.pos.z, pb.pos.facing);
            let moved = pb.pos.step(*direction);

            trace!(
                "[playback:headless] {} #{idx} dir={} ({},{},{}) → ({},{},{})",
                if moved { "step" } else { "turn" },
                direction,
                _before.0, _before.1, _before.2,
                pb.pos.x, pb.pos.y, pb.pos.z,
            );
            if pb.pos.is_ready() {
                let dgp = pb.pos.to_draw_game_player().to_bytes();
                log_packet(pkt_tx, reg, &dgp, PacketSource::Synthesized, PacketDirection::ServerToClient, ts);
                client.send(RawPacket::s2c(dgp)).await?;
            }
        }

        EntryKind::ClientPacket(data) => {
            // C→S packets from the original session — not forwarded to the
            // UO client but logged so the web inspector shows both directions.
            log_packet(pkt_tx, reg, data, PacketSource::ReplayClient, PacketDirection::ClientToServer, ts);
        }
    }
    Ok(())
}

// ── step_to_entry_headless ───────────────────────────────────────────────

/// Step-and-seek helper for headless playback.
async fn step_to_entry_headless(
    direction: i32,
    predicate: fn(&ReplayEntry) -> bool,
    label: &str,
    entries: &[ReplayEntry],
    log_entries: &[LogEntry],
    player: &mut LogPlayer,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
    pkt_tx: &broadcast::Sender<PacketLogEntry>,
    reg: &PacketRegistry,
) -> fw_error::Result<()> {
    let anchor = pb.idx.saturating_sub(1);
    if let Some(ei) = find_step_target(entries, anchor, direction, predicate) {
        let step_reg = PacketRegistry::default();

        if direction > 0 && ei > anchor + 1 {
            let skipped = ei - anchor - 1;
            info!("[replay:headless] step {} (dir={}) → entry={} (skipping {} entries)", label, direction, ei, skipped);
            for skip_idx in (anchor + 1)..ei {
                log_entry_description(&entries[skip_idx], &step_reg, "[skip]");
            }
        } else {
            info!("[replay:headless] step {} (dir={}) → entry={}", label, direction, ei);
        }

        log_entry_description(&entries[ei], &step_reg, "[step]");
        perform_seek_headless(ei, entries, log_entries, player, pb, observer, client, snapshots, house_cache, pkt_tx, reg).await?;
        pb.transition(PlaybackTransition::StepTo {
            entry_idx: ei,
            us: entries[ei].us_offset,
        });

        // Forward the target entry itself so the client sees it.
        if let EntryKind::Forward(data) = &entries[ei].kind {
            log_packet(pkt_tx, reg, data, PacketSource::ReplayServer, PacketDirection::ServerToClient, Some(entries[ei].us_offset));
            client.send(RawPacket::s2c(data.clone())).await?;
        }
    }
    Ok(())
}

// ── handle_headless_command ──────────────────────────────────────────────

/// Translate a [`PlaybackCommand`] into state transitions — headless variant.
///
/// Returns `Some(HeadlessPlaybackResult)` if the command terminates playback,
/// `None` if the loop should continue.
async fn handle_headless_command(
    cmd: PlaybackCommand,
    entries: &[ReplayEntry],
    log_entries: &[LogEntry],
    player: &mut LogPlayer,
    pb: &mut PlaybackState,
    observer: &mut ObserverPipeline,
    client: &mut Session,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
    ch: &HeadlessChannels,
    reg: &PacketRegistry,
) -> fw_error::Result<Option<HeadlessPlaybackResult>> {
    match cmd {
        PlaybackCommand::Stop => {
            info!("[replay:headless] stopped by external command");
            return Ok(Some(HeadlessPlaybackResult::Stopped));
        }

        PlaybackCommand::Restart => {
            info!("[replay:headless] restart requested");
            return Ok(Some(HeadlessPlaybackResult::Restart));
        }

        PlaybackCommand::TogglePause => {
            let action = if pb.paused { "resumed" } else { "paused" };
            pb.transition(PlaybackTransition::TogglePause);
            info!("[replay:headless] {} at {}µs", action, pb.current_us);
        }

        PlaybackCommand::SeekAbsolute(target_us) => {
            let target_us = (target_us * 1_000).min(pb.total_us);
            info!(
                "[replay:headless] seek absolute: {}µs → {}µs / {}µs",
                pb.current_us, target_us, pb.total_us,
            );
            let target_ei = entry_idx_for_us(entries, target_us);
            perform_seek_headless(target_ei, entries, log_entries, player, pb, observer, client, snapshots, house_cache, &ch.packet_log_tx, reg).await?;
            pb.transition(PlaybackTransition::Seek {
                target_us,
                entry_idx: target_ei,
            });
            if !pb.paused {
                pb.start += Duration::from_millis(800);
            }
        }

        PlaybackCommand::SeekRelative(delta_ms) => {
            let delta_us = delta_ms * 1_000;
            let target_us = pb.clamp_target(delta_us);
            info!(
                "[replay:headless] seek relative {} ms: {}µs → {}µs / {}µs",
                delta_ms, pb.current_us, target_us, pb.total_us,
            );
            let target_ei = entry_idx_for_us(entries, target_us);
            perform_seek_headless(target_ei, entries, log_entries, player, pb, observer, client, snapshots, house_cache, &ch.packet_log_tx, reg).await?;
            pb.transition(PlaybackTransition::Seek {
                target_us,
                entry_idx: target_ei,
            });
            if !pb.paused {
                pb.start += Duration::from_millis(800);
            }
        }

        PlaybackCommand::FastForward(delta_ms) => {
            let delta_us = delta_ms * 1_000;
            let target_us = pb.clamp_target(delta_us);
            info!(
                "[replay:headless] fast-forward {} ms: {}µs → {}µs / {}µs",
                delta_ms, pb.current_us, target_us, pb.total_us,
            );
            pb.transition(PlaybackTransition::StartFastForward { target_us });
        }

        PlaybackCommand::StepPacket(dir) => {
            step_to_entry_headless(
                dir, |_| true, "packet",
                entries, log_entries, player, pb, observer, client,
                snapshots, house_cache, &ch.packet_log_tx, reg,
            ).await?;
        }

        PlaybackCommand::StepClientPacket(dir) => {
            step_to_entry_headless(
                dir, |e| e.kind.is_client(), "client packet",
                entries, log_entries, player, pb, observer, client,
                snapshots, house_cache, &ch.packet_log_tx, reg,
            ).await?;
        }

        PlaybackCommand::StepServerPacket(dir) => {
            step_to_entry_headless(
                dir, |e| !e.kind.is_client(), "server packet",
                entries, log_entries, player, pb, observer, client,
                snapshots, house_cache, &ch.packet_log_tx, reg,
            ).await?;
        }

        PlaybackCommand::SetSpeed(speed) => {
            info!("[replay:headless] speed set to {}", speed);
            pb.speed = speed.max(0.1);
            let secs = pb.current_us as f64 / 1_000_000.0 / pb.speed;
            pb.start = tokio::time::Instant::now()
                .checked_sub(Duration::from_secs_f64(secs.max(0.0)))
                .unwrap_or_else(tokio::time::Instant::now);
            if pb.paused {
                pb.pause_start = Some(tokio::time::Instant::now());
            }
        }
    }

    // Broadcast status update after every command.
    let _ = ch.status_tx.send(PlaybackStatus::from_state(pb, entries.len(), false));
    Ok(None)
}

// ── run_playback_headless ────────────────────────────────────────────────

/// Main playback loop driven by external commands — no gumps, no shadow continuum.
///
/// Every dispatched packet is also broadcast via `channels.packet_log_tx`
/// for the web-based packet inspector.
pub async fn run_playback_headless(
    client: &mut Session,
    log_entries: &[LogEntry],
    entries: &[ReplayEntry],
    init_packets: Option<&[Bytes]>,
    observer: &mut ObserverPipeline,
    snapshots: &[LogPlayerSnapshot],
    house_cache: &HashMap<u32, Bytes>,
    static_data: Option<Arc<StaticWorldData>>,
    channels: &mut HeadlessChannels,
) -> fw_error::Result<HeadlessPlaybackResult> {
    use tokio::time::{Instant, sleep_until};
    let mut player = LogPlayer::new(static_data);
    let reg = PacketRegistry::default();

    if entries.is_empty() {
        info!("[replay:headless] no entries to replay");
        return Ok(HeadlessPlaybackResult::Finished);
    }

    // Send init packets (bootstrap) if provided.
    if let Some(pkts) = init_packets {
        debug!("[replay:headless] sending {} init packets (bootstrap)", pkts.len());
        for pkt in pkts {
            if !pkt.is_empty() && pkt[0] == 0x20 {
                trace!("[replay:headless] skipping 0x20 DrawGamePlayer in init_packets");
                continue;
            }
            observer.session.ingest_packet(pkt);
            log_packet(&channels.packet_log_tx, &reg, pkt, PacketSource::ReplayServer, PacketDirection::ServerToClient, Some(0));
            client.send(RawPacket::s2c(pkt.clone())).await?;
        }
    }

    let total_us = entries.last().map(|e| e.us_offset).unwrap_or(0);

    // Dispatch initial entries up to and including LoginComplete.
    // LoginComplete may arrive with a non-zero us_offset (a few ms after
    // the origin 0x78), so we scan up to 2 seconds into the timeline
    // instead of only us_offset == 0 entries.  The classic client ignores
    // packets (including gumps) received before LoginComplete.
    const PRE_GUMP_MAX_US: u64 = 2_000_000; // 2 seconds
    let mut start_idx: usize = 1;
    let mut pre_gump_found_login_complete = false;
    for (i, entry) in entries.iter().enumerate().skip(1) {
        if entry.us_offset > PRE_GUMP_MAX_US {
            break;
        }
        match &entry.kind {
            EntryKind::Forward(data) | EntryKind::WorldInit(data) => {
                observer.pos.update_from_packet(data);
                observer.session.ingest_packet(data);
                log_packet(&channels.packet_log_tx, &reg, data, PacketSource::ReplayServer, PacketDirection::ServerToClient, Some(0));
                client.send(RawPacket::s2c(data.clone())).await?;
            }
            _ => {}
        }
        start_idx = i + 1;
        if matches!(&entry.kind, EntryKind::WorldInit(d) if !d.is_empty() && d[0] == LoginComplete::ID)
        {
            pre_gump_found_login_complete = true;
            break;
        }
    }
    debug!(
        "[replay:headless] pre-dispatch: start_idx={}, LoginComplete {}",
        start_idx,
        if pre_gump_found_login_complete { "found" } else { "NOT found (was in init_packets)" },
    );

    let mut pb = PlaybackState {
        pos: observer.pos,
        start: Instant::now(),
        idx: start_idx,
        current_us: 0,
        total_us: total_us,
        seeked: false,
        seeked_world: 0,
        paused: false,
        pause_start: None,
        speed: 1.0,
        ff_target_us: None,
        saved_pause: None,
        view_mode: super::ViewMode::FirstPerson,
        replay_pos: observer.pos,
    };

    // Send initial status.
    let _ = channels.status_tx.send(PlaybackStatus::from_state(&pb, entries.len(), false));

    'playback: loop {
        // ── Paused: wait for external commands or client events ──────
        if pb.paused {
            tokio::select! {
                biased;

                Some(cmd) = channels.command_rx.recv() => {
                    if let Some(result) = handle_headless_command(
                        cmd, entries, log_entries,
                        &mut player, &mut pb, observer, client,
                        snapshots, house_cache, channels, &reg,
                    ).await? {
                        return Ok(result);
                    }
                }

                event = client.recv() => {
                    match event.event {
                        SessionEvent::Packet(p) => {
                            handle_client_packet_minimal(client, p, observer, &pb, house_cache, &channels.packet_log_tx, &reg).await?;
                        }
                        SessionEvent::Stopped | SessionEvent::Disconnected => {
                            return Ok(HeadlessPlaybackResult::Disconnected);
                        }
                        SessionEvent::Error(e) => return Err(e.into()),
                        _ => {}
                    }
                }
            }
            continue;
        }

        // ── Playing: compute next deadline, skip ClientPacket entries ─
        // (still log skipped ClientPacket entries to the packet log)
        while pb.idx < entries.len() && matches!(entries[pb.idx].kind, EntryKind::ClientPacket(_)) {
            if let EntryKind::ClientPacket(data) = &entries[pb.idx].kind {
                log_packet(&channels.packet_log_tx, &reg, data, PacketSource::ReplayClient, PacketDirection::ClientToServer, Some(entries[pb.idx].us_offset));
            }
            pb.idx += 1;
        }
        let Some(deadline) = (pb.idx < entries.len())
            .then(|| {
                let secs = entries[pb.idx].us_offset as f64 / 1_000_000.0 / pb.speed;
                pb.start + Duration::from_secs_f64(secs.max(0.0))
            })
        else {
            // Reached end of replay.
            if pb.ff_target_us.is_some() {
                info!("[replay:headless] fast-forward hit end at {}µs", pb.current_us);
                pb.current_us = pb.total_us;
                pb.transition(PlaybackTransition::EndFastForward);
                let _ = channels.status_tx.send(PlaybackStatus::from_state(&pb, entries.len(), false));
                if pb.paused {
                    continue;
                }
            }
            // Playback finished — enter paused state at end.
            pb.paused = true;
            pb.pause_start = Some(Instant::now());
            pb.current_us = pb.total_us;
            let _ = channels.status_tx.send(PlaybackStatus::from_state(&pb, entries.len(), true));
            info!("[replay:headless] playback finished at {}µs — paused at end", pb.total_us);

            // Wait for commands (restart / stop / seek / disconnect).
            loop {
                tokio::select! {
                    biased;

                    Some(cmd) = channels.command_rx.recv() => {
                        if let Some(result) = handle_headless_command(
                            cmd, entries, log_entries,
                            &mut player, &mut pb, observer, client,
                            snapshots, house_cache, channels, &reg,
                        ).await? {
                            return Ok(result);
                        }
                        if !pb.paused || pb.idx < entries.len() {
                            continue 'playback;
                        }
                    }

                    event = client.recv() => {
                        match event.event {
                            SessionEvent::Packet(p) => {
                                handle_client_packet_minimal(client, p, observer, &pb, house_cache, &channels.packet_log_tx, &reg).await?;
                            }
                            SessionEvent::Stopped | SessionEvent::Disconnected => {
                                return Ok(HeadlessPlaybackResult::Disconnected);
                            }
                            SessionEvent::Error(e) => return Err(e.into()),
                            _ => {}
                        }
                    }
                }
            }
        };

        tokio::select! {
            biased;

            Some(cmd) = channels.command_rx.recv() => {
                if let Some(result) = handle_headless_command(
                    cmd, entries, log_entries,
                    &mut player, &mut pb, observer, client,
                    snapshots, house_cache, channels, &reg,
                ).await? {
                    return Ok(result);
                }
            }

            event = client.recv() => {
                match event.event {
                    SessionEvent::Packet(p) => {
                        handle_client_packet_minimal(client, p, observer, &pb, house_cache, &channels.packet_log_tx, &reg).await?;
                    }
                    SessionEvent::Stopped | SessionEvent::Disconnected => {
                        return Ok(HeadlessPlaybackResult::Disconnected);
                    }
                    SessionEvent::Error(e) => return Err(e.into()),
                    _ => {}
                }
            }

            _ = sleep_until(deadline) => {
                pb.current_us = entries[pb.idx].us_offset;
                dispatch_entry_headless(&entries[pb.idx], pb.idx, &mut pb, observer, client, house_cache, &channels.packet_log_tx, &reg).await?;
                pb.idx += 1;

                // Check if fast-forward target has been reached.
                if let Some(target) = pb.ff_target_us {
                    if pb.current_us >= target {
                        info!("[replay:headless] fast-forward reached target {}µs", target);
                        pb.transition(PlaybackTransition::EndFastForward);
                        let _ = channels.status_tx.send(PlaybackStatus::from_state(&pb, entries.len(), false));
                    }
                }

                // Periodic status broadcast (every 50 entries to avoid flooding).
                if pb.idx % 50 == 0 {
                    let _ = channels.status_tx.send(PlaybackStatus::from_state(&pb, entries.len(), false));
                }
            }
        }
    }
}

// ── handle_client_packet_minimal ─────────────────────────────────────────

/// Minimal client packet handling during headless playback.
///
/// Only handles:
/// - `MoveRequest` — rejected (snap back to replay position)
/// - `ClientViewRange` — echoed back
/// - `0xBF:001E RequestHouseState` — responded from cache
/// - Everything else — silently dropped
///
/// All packets from the live client are logged as [`PacketSource::LiveClient`].
async fn handle_client_packet_minimal(
    client: &mut Session,
    packet: RawPacket,
    observer: &mut ObserverPipeline,
    pb: &PlaybackState,
    house_cache: &HashMap<u32, Bytes>,
    pkt_tx: &broadcast::Sender<PacketLogEntry>,
    reg: &PacketRegistry,
) -> fw_error::Result<()> {
    use packets::movement::{MoveReject, MoveRequest};

    // Log every live client packet.
    log_packet(pkt_tx, reg, &packet.data, PacketSource::LiveClient, PacketDirection::ClientToServer, Some(pb.current_us));

    // 0xBF sub 0x001E — RequestHouseState
    if packet.id() == 0xBF && packet.data.len() >= 9 {
        let sub = u16::from_be_bytes([packet.data[3], packet.data[4]]);
        if sub == 0x001E {
            let serial = u32::from_be_bytes([
                packet.data[5], packet.data[6], packet.data[7], packet.data[8],
            ]);
            if let Some(house_data) = house_cache.get(&serial) {
                log_packet(pkt_tx, reg, house_data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(pb.current_us));
                client.send(RawPacket::s2c(house_data.clone())).await?;
            }
            return Ok(());
        }
    }

    // ClientViewRange (0xC8)
    if packet.id() == ClientViewRange::ID {
        if let Ok(cvr) = ClientViewRange::from_bytes(&packet.data) {
            observer.session.visible.set_view_range(cvr.range as u16);
            log_packet(pkt_tx, reg, &packet.data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(pb.current_us));
            client.send(RawPacket::s2c(packet.data.clone())).await?;
        }
        return Ok(());
    }

    // MoveRequest — reject and snap back
    if packet.id() == MoveRequest::ID {
        if let Ok(req) = MoveRequest::from_bytes(&packet.data) {
            let reject = MoveReject {
                id: MoveReject::ID,
                sequence: req.sequence,
                x: pb.pos.x,
                y: pb.pos.y,
                direction: pb.pos.facing.raw(),
                z: pb.pos.z,
            };
            let reject_data = reject.to_bytes();
            log_packet(pkt_tx, reg, &reject_data, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(pb.current_us));
            client.send(RawPacket::s2c(reject_data)).await?;
            if pb.pos.is_ready() {
                let dgp = pb.pos.to_draw_game_player().to_bytes();
                log_packet(pkt_tx, reg, &dgp, PacketSource::Synthesized, PacketDirection::ServerToClient, Some(pb.current_us));
                client.send(RawPacket::s2c(dgp)).await?;
            }
        }
        return Ok(());
    }

    // Everything else — silently dropped (already logged above).
    Ok(())
}
