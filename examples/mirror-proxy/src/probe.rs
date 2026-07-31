//! Proactive object-probing support for `mirror-proxy`.
//!
//! When the user passes `--probe-objects`, the proxy watches S2C traffic for
//! newly-seen **item** objects (not mobiles) and proactively sends a
//! single-click (`0x09`) and/or tooltip request (`0xD6 MegaClilocRequest`) to
//! the server so the item's properties are collected even when the real client
//! never interacts with it.
//!
//! Additionally, whenever a `0xBF:0x001D HouseRevisionState` packet arrives
//! from the server, a `0xBF:0x001E RequestHouseState` is sent back to the
//! server (once per house, deduped) to force the full custom-house design
//! through the connection — even when the real client already has it cached.
//!
//! # Pieces
//!
//! - [`ProbeMode`] / [`ProbeModeArg`] — which requests to send (auto-detected
//!   from client version, or forced via CLI).
//! - [`RateLimiter`] — token-bucket rate limiter on outgoing probe packets.
//! - [`ProbeKind`] / [`QueuedProbe`] — per-entry metadata: source (ground vs
//!   container) and enqueue time (for TTL).
//! - [`ProbeState`] — shared mutable state (queue, seen/handled sets, house
//!   dedup) protected by `Arc<Mutex<>>`.
//! - [`extract_item_serials`] — decode a raw S2C packet and return item
//!   serials with their [`ProbeKind`], skipping mobiles and multis.
//! - [`ObjectDetector`] — `PacketHandler` on the **S2C** chain; feeds new
//!   item serials into `ProbeState` and handles SetMap queue-clear.
//! - [`ClientProbeWatcher`] — `PacketHandler` on the **C2S** chain; marks
//!   serials the real client already clicked/queried so the proxy never
//!   duplicates those requests.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::debug;
use u_core::{PacketDirection, ProtocolVersion};

use network::handler::packet_handler::{HandlerAction, PacketHandler};
use protocol::RawPacket;

use packets::interaction::{
    AddItemToContainer, ContainerContent, DoubleClick, EquipItem, SingleClick,
};
use packets::tooltip::MegaClilocRequest;
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::{ObjectDataType, ObjectInfo, ObjectInfoSA, PacketList};

// ── ProbeMode ─────────────────────────────────────────────────────────────

/// Which C2S probe request(s) to send for each newly-discovered item object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMode {
    /// Send `0x09 SingleClick` only (for pre-AOS clients).
    Single,
    /// Send `0xD6 MegaClilocRequest` only (for AOS+ clients).
    Tooltip,
    /// Send both `0x09 SingleClick` and `0xD6 MegaClilocRequest`.
    Both,
}

/// Raw CLI choice for `--probe-mode`.
///
/// `Auto` resolves to [`ProbeMode::Tooltip`] for AOS+ clients
/// (`client_version >= 4.0.0.0`) and [`ProbeMode::Single`] for older ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProbeModeArg {
    #[default]
    Auto,
    Single,
    Tooltip,
    Both,
}

impl ProbeModeArg {
    /// Resolve the final [`ProbeMode`] given the connected client version.
    pub fn resolve(self, version: ProtocolVersion) -> ProbeMode {
        match self {
            Self::Single => ProbeMode::Single,
            Self::Tooltip => ProbeMode::Tooltip,
            Self::Both => ProbeMode::Both,
            Self::Auto => {
                if version >= ProtocolVersion::AOS_CLIENT {
                    ProbeMode::Tooltip
                } else {
                    ProbeMode::Single
                }
            }
        }
    }
}

impl FromStr for ProbeModeArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto"    => Ok(Self::Auto),
            "single"  => Ok(Self::Single),
            "tooltip" => Ok(Self::Tooltip),
            "both"    => Ok(Self::Both),
            other => Err(format!(
                "unknown probe mode '{other}'; expected one of: auto, single, tooltip, both"
            )),
        }
    }
}

impl fmt::Display for ProbeModeArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto    => write!(f, "auto"),
            Self::Single  => write!(f, "single"),
            Self::Tooltip => write!(f, "tooltip"),
            Self::Both    => write!(f, "both"),
        }
    }
}

// ── RateLimiter ───────────────────────────────────────────────────────────

/// Token-bucket rate limiter for outgoing probe packets.
///
/// Measures rate in **packets per second** (each `0x09` or `0xD6` chunk = 1).
/// When `pps == 0` the limiter is disabled and [`allow`](Self::allow) always
/// grants the full request.
#[derive(Debug)]
pub struct RateLimiter {
    /// Tokens added per second.  0 = disabled (unlimited).
    pps: f64,
    /// Maximum token accumulation (burst capacity).
    capacity: f64,
    /// Current token balance.
    tokens: f64,
    /// Timestamp of the last refill.
    last: Instant,
}

impl RateLimiter {
    /// Create a new limiter.
    ///
    /// - `pps = 0`: unlimited — [`allow`](Self::allow) always grants everything.
    /// - `burst = 0`: treated as equal to `pps` (no extra burst headroom).
    pub fn new(pps: u32, burst: u32) -> Self {
        let pps_f = pps as f64;
        let cap = if burst == 0 { pps_f } else { burst as f64 }.max(pps_f);
        Self {
            pps: pps_f,
            capacity: cap,
            // Start with a full bucket so the first tick fires immediately.
            tokens: cap,
            last: Instant::now(),
        }
    }

    /// Request `want` token(s).  Returns how many are granted (0 ≤ result ≤ want).
    ///
    /// When the limiter is disabled (`pps == 0`), `want` is always returned.
    pub fn allow(&mut self, want: usize) -> usize {
        if self.pps == 0.0 {
            return want;
        }

        // Refill tokens based on elapsed time.
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.pps).min(self.capacity);

        let granted = (self.tokens as usize).min(want);
        self.tokens -= granted as f64;
        granted
    }
}

// ── ProbeKind ─────────────────────────────────────────────────────────────

/// Whether an item serial came from a ground/world packet or a container.
///
/// Used at probe-send time to decide whether to check the item's position in
/// the visible world (ground items must still be visible and within range) or
/// to send unconditionally (container items have no world coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    /// Item is on the ground / world (`0x1A`, `0xF3`, `0xF7`).
    Ground,
    /// Item is inside a container (`0x25`, `0x3C`) or equipped (`0x2E`).
    Container,
}

// ── QueuedProbe ───────────────────────────────────────────────────────────

/// A pending probe entry in the [`ProbeState`] queue.
#[derive(Debug, Clone)]
pub struct QueuedProbe {
    pub serial: u32,
    /// Where the item was first seen.
    pub kind: ProbeKind,
    /// When this entry was enqueued (for TTL filtering).
    pub enqueued_at: Instant,
}

// ── ProbeState ────────────────────────────────────────────────────────────

/// Shared, mutex-protected state for the proactive probing system.
///
/// Allocated once per game-phase session and shared between:
/// - [`ObjectDetector`] (S2C handler chain) — feeds new serials in.
/// - [`ClientProbeWatcher`] (C2S handler chain) — marks client-queried serials.
/// - The probe relay loop (timer arm) — drains the queue and sends requests.
#[derive(Debug, Default)]
pub struct ProbeState {
    /// Every item serial ever observed in this session (prevents double-
    /// queueing after the object leaves and re-enters view).
    seen: HashSet<u32>,
    /// Serials already handled: either sent by us or by the real client.
    /// Entries here are never probed again within the same session.
    handled: HashSet<u32>,
    /// Queue of new serials waiting to be probed in the next timer tick.
    queue: VecDeque<QueuedProbe>,
    /// Custom-house serials for which a `0xBF:0x001E RequestHouseState` has
    /// already been sent.  Prevents duplicate house-design requests.
    pub requested_houses: HashSet<u32>,
}

impl ProbeState {
    /// Record a newly-seen item serial.
    ///
    /// If `serial` has not been seen before and is not already handled,
    /// it is pushed onto the probe queue with the current timestamp.
    pub fn observe_object(&mut self, serial: u32, kind: ProbeKind) {
        if serial == 0 {
            return;
        }
        if self.seen.insert(serial) && !self.handled.contains(&serial) {
            self.queue.push_back(QueuedProbe {
                serial,
                kind,
                enqueued_at: Instant::now(),
            });
        }
    }

    /// Mark a serial as having been handled (by us or the client).
    ///
    /// Handled serials are filtered at drain time so even if they are already
    /// enqueued they won't produce a duplicate request.
    pub fn mark_handled(&mut self, serial: u32) {
        self.handled.insert(serial);
    }

    /// Drain up to `max` candidate serials, skipping already-handled entries
    /// and entries older than `ttl` (if set).
    ///
    /// **Does not** mark candidates as handled — the caller must do so after
    /// deciding which candidates are actually sent (actuality check may still
    /// discard some).
    pub fn drain_candidates(&mut self, max: usize, ttl: Option<Duration>) -> Vec<QueuedProbe> {
        let now = Instant::now();
        let mut out = Vec::with_capacity(max.min(self.queue.len()));

        while out.len() < max {
            let entry = match self.queue.pop_front() {
                None => break,
                Some(e) => e,
            };
            // Skip serials the client already handled while this was queued.
            if self.handled.contains(&entry.serial) {
                continue;
            }
            // Discard stale entries.
            if let Some(ttl) = ttl {
                if now.duration_since(entry.enqueued_at) > ttl {
                    debug!(
                        "probe: serial={:#010X} expired (queued {:?} ago)",
                        entry.serial,
                        now.duration_since(entry.enqueued_at),
                    );
                    // Mark as handled so we don't re-queue on a future observe.
                    self.handled.insert(entry.serial);
                    continue;
                }
            }
            out.push(entry);
        }
        out
    }

    /// Clear all pending (not-yet-sent) probe requests.
    ///
    /// Called on `SetMap` (world change): ground objects from the previous
    /// facet are no longer in scope.  The `seen` and `handled` sets are
    /// preserved so that objects already probed in this session are not
    /// probed again if re-encountered later.
    pub fn clear_pending(&mut self) {
        self.queue.clear();
    }

    /// Record that we want a custom-house design for `serial`.
    ///
    /// Returns `true` the first time a given serial is seen (caller should
    /// send the `0xBF:0x001E RequestHouseState`), `false` on duplicates.
    pub fn note_house(&mut self, serial: u32) -> bool {
        self.requested_houses.insert(serial)
    }
}

// ── extract_item_serials ──────────────────────────────────────────────────

/// Decode a raw S2C packet and return `(serial, ProbeKind)` pairs for every
/// item it contains.
///
/// Only item-bearing packet types are processed.  Mobile packets and all
/// other packet types return an empty vec.  Multi-objects (houses, boats) are
/// skipped — they are probed separately via house-revision logic.
///
/// Decode errors are logged at `debug` level and silently ignored so a
/// malformed packet never stalls the relay.
pub fn extract_item_serials(data: &[u8]) -> Vec<(u32, ProbeKind)> {
    let id = match data.first() {
        Some(&b) => b,
        None => return Vec::new(),
    };

    match id {
        // ── 0x1A ObjectInfo (classic world item) ──────────────────────────
        0x1A => match ObjectInfo::from_bytes(data) {
            Ok(pkt) if !pkt.is_multi() => vec![(pkt.object_id, ProbeKind::Ground)],
            Ok(_) => Vec::new(), // multi — handled by house logic
            Err(e) => {
                debug!("probe: 0x1A decode error: {e}");
                Vec::new()
            }
        },

        // ── 0xF3 ObjectInfoSA (SA world item) ─────────────────────────────
        0xF3 => match ObjectInfoSA::from_bytes(data) {
            Ok(pkt) if pkt.data_type == ObjectDataType::Item => {
                vec![(pkt.serial, ProbeKind::Ground)]
            }
            Ok(_) => Vec::new(), // Multi — handled by house logic
            Err(e) => {
                debug!("probe: 0xF3 decode error: {e}");
                Vec::new()
            }
        },

        // ── 0xF7 PacketList (batch of ObjectInfoSA, High Seas) ────────────
        0xF7 => match PacketList::from_bytes(data) {
            Ok(pkt) => pkt
                .items
                .iter()
                .filter(|i| i.data_type == ObjectDataType::Item)
                .map(|i| (i.serial, ProbeKind::Ground))
                .collect(),
            Err(e) => {
                debug!("probe: 0xF7 decode error: {e}");
                Vec::new()
            }
        },

        // ── 0x25 AddItemToContainer ───────────────────────────────────────
        0x25 => match AddItemToContainer::from_bytes(data) {
            Ok(pkt) => vec![(pkt.serial(), ProbeKind::Container)],
            Err(e) => {
                debug!("probe: 0x25 decode error: {e}");
                Vec::new()
            }
        },

        // ── 0x3C ContainerContent ─────────────────────────────────────────
        0x3C => match ContainerContent::from_bytes(data) {
            Ok(pkt) => pkt
                .item_serials()
                .into_iter()
                .map(|s| (s, ProbeKind::Container))
                .collect(),
            Err(e) => {
                debug!("probe: 0x3C decode error: {e}");
                Vec::new()
            }
        },

        // ── 0x2E EquipItem (item equipped on a mobile) ────────────────────
        0x2E => match EquipItem::from_bytes(data) {
            Ok(pkt) => vec![(pkt.item_serial, ProbeKind::Container)],
            Err(e) => {
                debug!("probe: 0x2E decode error: {e}");
                Vec::new()
            }
        },

        // ── All other ids (mobiles, system packets, …) ────────────────────
        _ => Vec::new(),
    }
}

// ── ObjectDetector ────────────────────────────────────────────────────────

/// [`PacketHandler`] placed on the **server-inbound (S2C) chain**.
///
/// For every S2C packet, calls [`extract_item_serials`] and records any
/// newly-seen item serials into the shared [`ProbeState`] probe queue.
///
/// Also watches for `0xBF:0x0008 SetMap` and calls
/// [`ProbeState::clear_pending`] so stale ground-item requests from the
/// previous facet are not sent after a world change.
///
/// The packet is always forwarded unchanged.
#[derive(Debug)]
pub struct ObjectDetector {
    tag: String,
    state: Arc<Mutex<ProbeState>>,
}

impl ObjectDetector {
    pub fn new(tag: impl Into<String>, state: Arc<Mutex<ProbeState>>) -> Self {
        Self { tag: tag.into(), state }
    }
}

impl PacketHandler for ObjectDetector {
    fn name(&self) -> &str {
        "probe-detector"
    }

    fn handle(&mut self, dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if dir == PacketDirection::ServerToClient {
            // Detect world change — clear the pending queue.
            if packet.data.len() >= 5 && packet.data[0] == 0xBF {
                let sub = u16::from_be_bytes([packet.data[3], packet.data[4]]);
                if sub == 0x0008 {
                    let mut st = self.state.lock().unwrap();
                    let before = st.queue.len();
                    st.clear_pending();
                    if before > 0 {
                        debug!(
                            "{} probe-detector: SetMap — cleared {} pending probe(s)",
                            self.tag, before,
                        );
                    }
                }
            }

            let items = extract_item_serials(&packet.data);
            if !items.is_empty() {
                let mut st = self.state.lock().unwrap();
                for (serial, kind) in &items {
                    st.observe_object(*serial, *kind);
                }
                debug!(
                    "{} probe-detector: {} serial(s) from 0x{:02X}",
                    self.tag,
                    items.len(),
                    packet.data[0],
                );
            }
        }
        HandlerAction::Forward(packet)
    }
}

// ── ClientProbeWatcher ────────────────────────────────────────────────────

/// [`PacketHandler`] placed on the **client-inbound (C2S) chain**.
///
/// Inspects single-click (`0x09`), double-click (`0x06`), and tooltip-request
/// (`0xD6`) packets sent by the real client.  Any serial the client itself
/// targets is marked handled in [`ProbeState`] so the proxy never sends a
/// duplicate probe for it.
///
/// The packet is always forwarded to the server unchanged.
#[derive(Debug)]
pub struct ClientProbeWatcher {
    tag: String,
    state: Arc<Mutex<ProbeState>>,
}

impl ClientProbeWatcher {
    pub fn new(tag: impl Into<String>, state: Arc<Mutex<ProbeState>>) -> Self {
        Self { tag: tag.into(), state }
    }
}

impl PacketHandler for ClientProbeWatcher {
    fn name(&self) -> &str {
        "probe-client-watcher"
    }

    fn handle(&mut self, dir: PacketDirection, packet: RawPacket) -> HandlerAction {
        if dir == PacketDirection::ClientToServer {
            match packet.data.first().copied() {
                // 0x09 SingleClick — single serial
                Some(0x09) => {
                    if let Ok(pkt) = SingleClick::from_bytes(&packet.data) {
                        debug!(
                            "{} probe-watcher: client 0x09 serial={:#010X}",
                            self.tag, pkt.serial,
                        );
                        self.state.lock().unwrap().mark_handled(pkt.serial);
                    }
                }
                // 0x06 DoubleClick — single serial
                Some(0x06) => {
                    if let Ok(pkt) = DoubleClick::from_bytes(&packet.data) {
                        debug!(
                            "{} probe-watcher: client 0x06 serial={:#010X}",
                            self.tag, pkt.serial,
                        );
                        self.state.lock().unwrap().mark_handled(pkt.serial);
                    }
                }
                // 0xD6 MegaClilocRequest — batch of serials
                Some(0xD6) => {
                    if let Ok(pkt) = MegaClilocRequest::from_bytes(&packet.data) {
                        debug!(
                            "{} probe-watcher: client 0xD6 {} serial(s)",
                            self.tag,
                            pkt.serials.len(),
                        );
                        let mut st = self.state.lock().unwrap();
                        for s in pkt.serials {
                            st.mark_handled(s);
                        }
                    }
                }
                _ => {}
            }
        }
        HandlerAction::Forward(packet)
    }
}
