//! Transport-independent mirror ingestion core.
//!
//! [`MirrorIngestor`] receives raw S2C UO packets, tracks the current world
//! via `0xBF SetMap`, filters them through a configurable predicate, and
//! forwards accepted packets to the UO worker as
//! [`EngineCommand::IngestPacket`].
//!
//! The struct is generic over the worker command type `C` via
//! [`WrapEngineCommand`], so the same code works for both `DemoCommand`
//! (demo-server) and `PathServerCommand` (path-server) — each just passes
//! its own `worker_tx`.
//!
//! ## Extension points
//!
//! - **`is_ingestable`**: a plain `fn(u8) -> bool` predicate that the caller
//!   supplies at construction time.  Use [`default_ingestable`] for the
//!   standard entity-packet set (items, mobiles, deletes, HP/status) or pass
//!   a custom function for a richer packet set (e.g. demo-server can add
//!   container packets).
//! - **`emit_events`**: when `true` the worker emits `WorldEvent::Entity*`
//!   events so live UO clients see ingested entities immediately.  Set to
//!   `false` for silent bulk replay.
//!
//! ## Transport
//!
//! This module has **no axum / WebSocket dependency**.  The transport layer
//! (axum WS handler in path-server or demo-server) owns the socket and calls
//! [`MirrorIngestor::process_packet`] for each binary frame it receives.

use bytes::Bytes;
use log::{debug, info, warn};

use framework::continuum::WorkerCommand;

use packets::system::GeneralInfo;
use packets::traits::ManualPacket;

use super::entity::DemoEntity;
use super::handler::EngineCommand;
use super::rpc::WrapEngineCommand;

// ── MirrorAction ─────────────────────────────────────────────────────────

/// Outcome of feeding one packet to [`MirrorIngestor::process_packet`].
///
/// Returned to the transport layer for logging / statistics; the ingestor
/// itself has already acted on the packet before returning.
#[derive(Debug)]
pub enum MirrorAction {
    /// `0xBF SetMap` received — world has been updated for subsequent packets.
    WorldChanged { from: u8, to: u8 },
    /// Packet matched the `is_ingestable` filter and was forwarded to the worker.
    Ingested,
    /// Packet was silently dropped (not recognised / not ingestable).
    Skipped,
}

// ── MirrorIngestor ───────────────────────────────────────────────────────

/// Generic, transport-independent packet ingestor.
///
/// # Type parameter
/// `C` — the worker's command enum (e.g. `DemoCommand`, `PathServerCommand`).
/// It must implement [`WrapEngineCommand`] so the ingestor can wrap an
/// [`EngineCommand::IngestPacket`] into whatever type the worker channel expects.
pub struct MirrorIngestor<C: WrapEngineCommand> {
    worker_tx: tokio::sync::mpsc::Sender<WorkerCommand<DemoEntity, C>>,
    current_world: u8,
    /// Total binary frames received (including skipped / world-change ones).
    pub packet_count: u64,
    /// Frames actually forwarded to the worker as `IngestPacket`.
    pub ingested_count: u64,
    /// Packet-ID predicate — returns `true` for IDs the ingestor should forward.
    is_ingestable: fn(u8) -> bool,
    /// Passed through to [`EngineCommand::IngestPacket`].
    emit_events: bool,
}

impl<C: WrapEngineCommand> MirrorIngestor<C> {
    /// Create a new ingestor.
    ///
    /// - `worker_tx`      — sender half of the worker channel.
    /// - `start_world`    — initial world / facet (0 = Felucca).
    /// - `is_ingestable`  — filter function; use [`default_ingestable`] or a
    ///                      custom predicate.
    /// - `emit_events`    — whether ingested packets should trigger
    ///                      `WorldEvent::Entity*` broadcasts.
    pub fn new(
        worker_tx: tokio::sync::mpsc::Sender<WorkerCommand<DemoEntity, C>>,
        start_world: u8,
        is_ingestable: fn(u8) -> bool,
        emit_events: bool,
    ) -> Self {
        Self {
            worker_tx,
            current_world: start_world,
            packet_count: 0,
            ingested_count: 0,
            is_ingestable,
            emit_events,
        }
    }

    /// Current world / facet tracked by SetMap packets.
    pub fn current_world(&self) -> u8 {
        self.current_world
    }

    /// Process one raw S2C UO packet (binary frame, no framing wrapper).
    ///
    /// Returns a [`MirrorAction`] describing what happened.
    /// Returns `Err(data)` if the worker channel is closed (caller should
    /// terminate the connection).
    pub async fn process_packet(
        &mut self,
        data: Vec<u8>,
    ) -> Result<MirrorAction, Vec<u8>> {
        if data.is_empty() {
            return Ok(MirrorAction::Skipped);
        }

        self.packet_count += 1;
        let pkt_id = data[0];

        // ── SetMap detection (0xBF sub 0x0008) ───────────────────────────
        if pkt_id == 0xBF {
            if let Ok(GeneralInfo::SetMap { world }) = GeneralInfo::from_bytes(&data) {
                let from = self.current_world;
                self.current_world = world;
                return Ok(MirrorAction::WorldChanged { from, to: world });
            }
            // Other 0xBF sub-commands — not relevant.
            return Ok(MirrorAction::Skipped);
        }

        // ── Packet filter ─────────────────────────────────────────────────
        if !(self.is_ingestable)(pkt_id) {
            return Ok(MirrorAction::Skipped);
        }

        // ── Forward to worker ─────────────────────────────────────────────
        let bytes = Bytes::from(data.clone());
        let cmd = WorkerCommand::MapCommand(
            self.current_world,
            C::wrap(EngineCommand::IngestPacket {
                data: bytes,
                emit_events: self.emit_events,
            }),
        );
        if self.worker_tx.send(cmd).await.is_err() {
            return Err(data);
        }

        self.ingested_count += 1;
        Ok(MirrorAction::Ingested)
    }
}

// ── Default packet filter ─────────────────────────────────────────────────

/// Standard entity-packet filter: returns `true` for packet IDs handled by
/// `ingest_into_entity_map`.
///
/// Covers items, mobiles, deletes, HP/status/attribute packets, and speech
/// (for mobile-name extraction):
///
/// | ID   | Packet              |
/// |------|---------------------|
/// | 0x1A | ObjectInfo          |
/// | 0xF3 | ObjectInfoSA        |
/// | 0xF7 | PacketList          |
/// | 0x78 | DrawMobile          |
/// | 0xD3 | DrawMobileExtended  |
/// | 0x77 | UpdateMobile        |
/// | 0x1D | DeleteObject        |
/// | 0x11 | StatusBarInfo       |
/// | 0x2D | MobAttributes       |
/// | 0xA1 | UpdateHealth        |
/// | 0x1C | SendSpeech          |
/// | 0xAE | UnicodeSpeech       |
/// | 0xD6 | MegaClilocResponse  |
/// | 0xC1 | ClilocMessage       |
/// | 0x88 | OpenPaperdoll       |
pub fn default_ingestable(pkt_id: u8) -> bool {
    matches!(
        pkt_id,
        0x1A  // ObjectInfo (item/multi)
        | 0xF3  // ObjectInfoSA (item/multi, SA+ format)
        | 0xF7  // PacketList (batch ObjectInfoSA)
        | 0x78  // DrawMobile
        | 0xD3  // DrawMobileExtended (3D client)
        | 0x77  // UpdateMobile (position/direction)
        | 0x1D  // DeleteObject
        | 0x11  // StatusBarInfo (name/HP)
        | 0x2D  // MobAttributes (HP/mana/stam)
        | 0xA1  // UpdateHealth
        | 0x1C  // SendSpeech (object name, ASCII)
        | 0xAE  // UnicodeSpeech (object name, UTF-16)
        | 0xD6  // MegaClilocResponse (full tooltip: name + properties)
        | 0xC1  // ClilocMessage (localized overhead label)
        | 0x88  // OpenPaperdoll (character name/title)
    )
}

// ── Shared connection-loop logging helpers ────────────────────────────────

/// Log a mirror connection event (connected).
pub fn log_connected(mirror_id: u64, world: u8) {
    info!("[mirror {}] connected, starting in world {}", mirror_id, world);
}

/// Log the outcome of `process_packet` at the appropriate log level.
pub fn log_action(mirror_id: u64, action: &MirrorAction, packet_count: u64, ingested_count: u64) {
    match action {
        MirrorAction::WorldChanged { from, to } => {
            info!(
                "[mirror {}] world changed: {} -> {} (after {} packets, {} ingested)",
                mirror_id, from, to, packet_count, ingested_count,
            );
        }
        MirrorAction::Ingested => {
            if ingested_count % 1000 == 0 {
                debug!(
                    "[mirror {}] progress: {} packets received, {} ingested, world={}",
                    mirror_id, packet_count, ingested_count, 0u8, // world printed by caller if needed
                );
            }
        }
        MirrorAction::Skipped => {}
    }
}

/// Log a mirror disconnection event.
pub fn log_disconnected(mirror_id: u64, packet_count: u64, ingested_count: u64) {
    info!(
        "[mirror {}] disconnected ({} packets received, {} ingested)",
        mirror_id, packet_count, ingested_count,
    );
}

/// Log a worker-channel-closed error.
pub fn log_send_failed(mirror_id: u64) {
    warn!("[mirror {}] worker send failed — channel closed", mirror_id);
}

// ── Connection ID counter ─────────────────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_MIRROR_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a unique monotonic mirror connection ID.
pub fn next_mirror_id() -> u64 {
    NEXT_MIRROR_ID.fetch_add(1, Ordering::Relaxed)
}
