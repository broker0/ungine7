//! [`MirrorProxy`] — a UO proxy that relays a client to the real server while
//! mirroring every S2C packet to an external WebSocket endpoint.
//!
//! Built on top of [`network`]'s [`network::listener::Listener`]/[`relay`] infrastructure.
//! When `--probe-objects` is enabled, the proxy also:
//!
//! - Detects newly-seen item objects (0x1A, 0xF3, 0xF7, 0x25, 0x3C, 0x2E).
//! - Sends proactive single-click (`0x09`) and/or tooltip requests (`0xD6`)
//!   to the server on a periodic timer, checking that each item is still
//!   within view of the player before sending (tracked via `ObserverPipeline`).
//! - Applies optional rate limiting (token bucket, `--probe-pps`) and TTL
//!   filtering on queued serials.
//! - Forces custom-house design fetches (`0xBF:0x001E`) whenever a
//!   `0xBF:0x001D HouseRevisionState` arrives from the server.
//!
//! When `--probe-objects` is absent the behaviour is identical to the original
//! code: the standard `relay::relay` is used and no additional packets are
//! ever sent.

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use log::{debug, info};
use tokio::net::TcpStream;
use tokio::time;

use framework::diorama::ObserverPipeline;

use protocol::transport::builder::TransportBuilder;
use protocol::Protocol;

use common::handlers::SubcommandFilter;
use network::error;
use network::handler::redirect::RedirectHandler;
use network::handler::HandlerChain;
use network::listener::{ConnectionContext, ListenerHandler, SessionPhase};
use network::relay;
use network::session::{Session, SessionEvent};
use protocol::RawPacket;

use packets::interaction::SingleClick;
use packets::system::GeneralInfo;
use packets::tooltip::MegaClilocRequest;
use packets::traits::{ManualPacket, BasicPacket};

use crate::mirror::{spawn_mirror_streamer, MirrorTap};
use crate::probe::{
    ClientProbeWatcher, ObjectDetector, ProbeKind, ProbeMode, ProbeModeArg,
    ProbeState, RateLimiter,
};

/// UO proxy that mirrors S2C packets to a WebSocket endpoint and optionally
/// probes newly-seen objects for additional data.
pub struct MirrorProxy {
    /// Public address of this proxy, written into the 0x8C redirect packet.
    proxy_addr: SocketAddrV4,
    /// Real upstream UO server address (used for login phase / fallback).
    server_addr: String,
    /// WebSocket URL of the mirror endpoint (e.g. path-server `/ws/mirror`).
    mirror_url: String,
    /// When `true`, 0x1D DeleteObject packets are excluded from the mirror
    /// stream so objects accumulate on the mirror endpoint.
    block_delete: bool,
    /// Enable proactive object probing and forced custom-house requests.
    probe_enabled: bool,
    /// How to determine which probe request type to send.
    probe_mode_arg: ProbeModeArg,
    /// Interval between probe-batch flushes to the server (milliseconds).
    probe_interval_ms: u64,
    /// Maximum number of probe *packets* sent per interval tick.
    probe_batch: usize,
    /// Maximum serials per `0xD6 MegaClilocRequest` packet.
    probe_tooltip_chunk: usize,
    /// Rate-limit: probe packets per second (0 = unlimited).
    probe_pps: u32,
    /// Token-bucket burst capacity (0 = same as pps).
    probe_burst: u32,
    /// TTL for queued probe serials in milliseconds (0 = disabled).
    probe_ttl_ms: u64,
    /// Max Chebyshev distance from player for a ground item to be probed
    /// (0 = use current view range).
    probe_max_dist: u16,
    /// Per-connection probe state, keyed by client socket address.
    ///
    /// Created in `configure_handlers` (called for the client-facing session
    /// before `handle_session`) and looked up in `handle_session` to attach
    /// the same state to the server-side `ObjectDetector`.  Entries are
    /// removed when the session ends.
    states: Arc<Mutex<HashMap<SocketAddr, Arc<Mutex<ProbeState>>>>>,
}

impl MirrorProxy {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proxy_addr: SocketAddrV4,
        server_addr: impl Into<String>,
        mirror_url: impl Into<String>,
        block_delete: bool,
        probe_enabled: bool,
        probe_mode_arg: ProbeModeArg,
        probe_interval_ms: u64,
        probe_batch: usize,
        probe_tooltip_chunk: usize,
        probe_pps: u32,
        probe_burst: u32,
        probe_ttl_ms: u64,
        probe_max_dist: u16,
    ) -> Self {
        Self {
            proxy_addr,
            server_addr: server_addr.into(),
            mirror_url: mirror_url.into(),
            block_delete,
            probe_enabled,
            probe_mode_arg,
            probe_interval_ms,
            probe_batch,
            probe_tooltip_chunk,
            probe_pps,
            probe_burst,
            probe_ttl_ms,
            probe_max_dist,
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get (or create) the per-connection [`ProbeState`] for `addr`.
    fn state_for(&self, addr: SocketAddr) -> Arc<Mutex<ProbeState>> {
        self.states
            .lock()
            .unwrap()
            .entry(addr)
            .or_insert_with(|| Arc::new(Mutex::new(ProbeState::default())))
            .clone()
    }

    /// Remove the per-connection state when the session ends.
    fn remove_state(&self, addr: SocketAddr) {
        self.states.lock().unwrap().remove(&addr);
    }

    /// Build the server-side handler chain for the given phase.
    fn server_handlers(
        &self,
        phase: SessionPhase,
        ctx: &ConnectionContext,
        tag: &str,
        mirror_tx: Option<tokio::sync::mpsc::Sender<bytes::Bytes>>,
    ) -> (HandlerChain, HandlerChain) {
        let mut inbound = HandlerChain::new();
        let outbound = HandlerChain::new();

        match phase {
            SessionPhase::LoginServer => {
                let (client_version, encrypted, seed_size) = match &ctx.protocol {
                    Protocol::Login(info) => (info.client_version, info.encrypted, info.seed_size),
                    Protocol::Game(info) => (info.client_version, info.encrypted, info.seed_size),
                };
                inbound.add(Box::new(RedirectHandler::new(
                    self.proxy_addr,
                    ctx.binder.clone(),
                    client_version,
                    encrypted,
                    seed_size,
                )));
            }
            SessionPhase::GameServer => {
                // Drop non-standard 0xBF sub-commands first, then mirror the
                // remaining S2C stream.
                inbound.add(Box::new(SubcommandFilter));

                if let Some(tx) = mirror_tx {
                    inbound.add(Box::new(MirrorTap::new(
                        tag.to_string(),
                        tx,
                        self.block_delete,
                    )));
                }

                // Object detector collects new item serials and handles SetMap.
                if self.probe_enabled {
                    let state = self.state_for(ctx.addr);
                    inbound.add(Box::new(ObjectDetector::new(tag, state)));
                }
            }
            SessionPhase::LoginClient | SessionPhase::GameClient => {}
        }

        (inbound, outbound)
    }
}

#[async_trait]
impl ListenerHandler for MirrorProxy {
    fn configure_handlers(
        &self,
        phase: SessionPhase,
        ctx: &ConnectionContext,
    ) -> (HandlerChain, HandlerChain) {
        // For the game-client phase, when probing is enabled, add the
        // ClientProbeWatcher to the client inbound (C2S) chain so the
        // proxy never duplicates requests the real client already sends.
        if phase == SessionPhase::GameClient && self.probe_enabled {
            let tag = format!("[{}]", ctx.addr);
            let state = self.state_for(ctx.addr);
            let mut inbound = HandlerChain::new();
            inbound.add(Box::new(ClientProbeWatcher::new(tag, state)));
            return (inbound, HandlerChain::new());
        }

        (HandlerChain::new(), HandlerChain::new())
    }

    async fn handle_session(
        &self,
        ctx: &ConnectionContext,
        mut client_session: Session,
    ) -> error::Result<()> {
        let addr = ctx.addr;
        let tag = format!("[{addr}]");

        let server_phase = match &ctx.protocol {
            Protocol::Login(_) => SessionPhase::LoginServer,
            Protocol::Game(_) => SessionPhase::GameServer,
        };

        let target = ctx.upstream_addr(&self.server_addr);

        log::info!("{tag} {server_phase} -> connecting to {target}");

        let server_stream = TcpStream::connect(&target).await?;
        let (transport, direction) =
            TransportBuilder::client(server_stream, &ctx.protocol).build()?;

        // Spawn a per-session mirror streamer only for the game phase.
        let mirror_tx = match server_phase {
            SessionPhase::GameServer => {
                Some(spawn_mirror_streamer(tag.clone(), self.mirror_url.clone()))
            }
            _ => None,
        };

        let (inbound, outbound) =
            self.server_handlers(server_phase, ctx, &tag, mirror_tx);
        let mut server_session =
            Session::with_handlers(transport, direction, inbound, outbound);

        let result = if self.probe_enabled && server_phase == SessionPhase::GameServer {
            let version = ctx.protocol.client_version();
            let mode = self.probe_mode_arg.resolve(version);
            info!(
                "{tag} probe: enabled, mode={mode:?}, interval={}ms, batch={}, \
                 tooltip_chunk={}, pps={}, burst={}, ttl={}ms, max_dist={}",
                self.probe_interval_ms,
                self.probe_batch,
                self.probe_tooltip_chunk,
                self.probe_pps,
                self.probe_burst,
                self.probe_ttl_ms,
                self.probe_max_dist,
            );

            let state = self.state_for(addr);
            let limiter = RateLimiter::new(self.probe_pps, self.probe_burst);
            let ttl = (self.probe_ttl_ms > 0)
                .then(|| Duration::from_millis(self.probe_ttl_ms));

            run_probe_relay(
                &tag,
                &mut client_session,
                &mut server_session,
                state,
                mode,
                self.probe_interval_ms,
                self.probe_batch,
                self.probe_tooltip_chunk,
                limiter,
                ttl,
                self.probe_max_dist,
            )
            .await
        } else {
            relay::relay(&tag, &mut client_session, &mut server_session, None).await
        };

        // Clean up per-connection state regardless of how the session ended.
        self.remove_state(addr);

        result
    }
}

// ── Probe relay loop ───────────────────────────────────────────────────────

/// Custom relay loop used when `--probe-objects` is active.
///
/// Extends the standard `relay::relay` behaviour with three extra
/// responsibilities:
///
/// 1. **World tracking via [`ObserverPipeline`]**: every S2C packet is fed
///    into the pipeline (which updates the player position, visible item set,
///    view range, etc.) and every C2S packet updates movement prediction.
///    This is used at probe-send time to check whether a ground item is still
///    within view of the player.
///
/// 2. **Forced custom-house requests**: whenever the server sends a
///    `0xBF:0x001D HouseRevisionState`, immediately send a C2S
///    `0xBF:0x001E RequestHouseState` back to the server (once per house,
///    deduped via [`ProbeState::note_house`]).
///
/// 3. **Periodic probe injection**: on each timer tick, drain candidate
///    serials from [`ProbeState`], check actuality against the pipeline, apply
///    the rate limiter, and send the appropriate C2S probe packet(s) to the
///    server in a single `send_all` call.
#[allow(clippy::too_many_arguments)]
async fn run_probe_relay(
    tag: &str,
    client: &mut Session,
    server: &mut Session,
    state: Arc<Mutex<ProbeState>>,
    mode: ProbeMode,
    interval_ms: u64,
    batch: usize,
    tooltip_chunk: usize,
    mut limiter: RateLimiter,
    ttl: Option<Duration>,
    max_dist: u16,
) -> error::Result<()> {
    let mut tick = time::interval(Duration::from_millis(interval_ms));
    // Don't accumulate missed ticks when the server is briefly slow.
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    // World observer — no static data needed (we only need positions, not Z).
    let mut observer = ObserverPipeline::new(None);

    let result: error::Result<()> = async {
        loop {
            tokio::select! {
                // ── Client → Server ────────────────────────────────────────
                recv = client.recv() => {
                    let event = recv.event;
                    for reply in recv.replies {
                        client.send(reply).await?;
                    }
                    match event {
                        SessionEvent::Packet(p) => {
                            // Feed into observer for movement tracking.
                            observer.ingest_c2s(&p.data);
                            server.send(p).await?;
                        }
                        SessionEvent::Seed(s) => {
                            server.send_seed(s).await?;
                        }
                        SessionEvent::Stopped => {
                            debug!(target: "relay", "{tag} client stopped by handler");
                            break;
                        }
                        SessionEvent::Disconnected => {
                            debug!(target: "relay", "{tag} client disconnected");
                            break;
                        }
                        SessionEvent::Error(e) => {
                            log::error!(target: "relay", "{tag} client error: {e}");
                            break;
                        }
                    }
                }

                // ── Server → Client ────────────────────────────────────────
                recv = server.recv() => {
                    let event = recv.event;
                    for reply in recv.replies {
                        server.send(reply).await?;
                    }
                    match event {
                        SessionEvent::Packet(p) => {
                            // Feed into observer before forwarding — this
                            // updates positions, visible world, view range, etc.
                            observer.ingest_s2c(&p.data);

                            // Detect 0xBF:0x001D HouseRevisionState and
                            // proactively request the full design from the
                            // server before forwarding to the client.
                            if p.id() == 0xBF && p.data.len() >= 13 {
                                let sub = u16::from_be_bytes([p.data[3], p.data[4]]);
                                if sub == 0x001D {
                                    let house_serial = u32::from_be_bytes([
                                        p.data[5], p.data[6], p.data[7], p.data[8],
                                    ]);
                                    if state.lock().unwrap().note_house(house_serial) {
                                        let req = GeneralInfo::RequestHouseState {
                                            house_serial,
                                        };
                                        server
                                            .send(RawPacket::c2s(req.to_bytes()))
                                            .await?;
                                        info!(
                                            "{tag} probe: requesting house design \
                                             serial={house_serial:#010X}"
                                        );
                                    }
                                }
                            }

                            client.send(p).await?;
                        }
                        SessionEvent::Seed(s) => {
                            client.send_seed(s).await?;
                        }
                        SessionEvent::Stopped => {
                            debug!(target: "relay", "{tag} server stopped by handler");
                            break;
                        }
                        SessionEvent::Disconnected => {
                            debug!(target: "relay", "{tag} server disconnected");
                            break;
                        }
                        SessionEvent::Error(e) => {
                            log::error!(target: "relay", "{tag} server error: {e}");
                            break;
                        }
                    }
                }

                // ── Probe timer ────────────────────────────────────────────
                _ = tick.tick() => {
                    let packets = build_probe_tick(
                        tag,
                        &state,
                        &observer,
                        &mut limiter,
                        mode,
                        batch,
                        tooltip_chunk,
                        ttl,
                        max_dist,
                    );
                    if !packets.is_empty() {
                        server.send_all(packets).await?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    client.close().await;
    server.close().await;

    result
}

// ── Probe tick ────────────────────────────────────────────────────────────

/// Evaluate one probe-timer tick and return the C2S packets to send to the
/// server (may be empty).
///
/// This is a pure synchronous step that:
/// 1. Asks the [`RateLimiter`] how many packets may be sent this tick.
/// 2. Drains TTL-filtered candidates from [`ProbeState`].
/// 3. Checks each ground-item candidate against the [`ObserverPipeline`]'s
///    visible set and player position; discards out-of-range / evicted items.
/// 4. Trims the resulting serial list to fit the packet budget.
/// 5. Builds `0x09 SingleClick` and/or `0xD6 MegaClilocRequest` packets.
/// 6. Marks sent serials as handled in [`ProbeState`].
///
/// The caller is responsible for forwarding the returned packets to the server
/// (via `server.send_all`).
#[allow(clippy::too_many_arguments)]
fn build_probe_tick(
    tag: &str,
    state: &Arc<Mutex<ProbeState>>,
    observer: &ObserverPipeline,
    limiter: &mut RateLimiter,
    mode: ProbeMode,
    batch: usize,
    tooltip_chunk: usize,
    ttl: Option<Duration>,
    max_dist: u16,
) -> Vec<RawPacket> {
    // ── 1. Rate-limiter budget ─────────────────────────────────────────────
    let budget = limiter.allow(batch);
    if budget == 0 {
        return Vec::new();
    }

    // ── 2. Drain TTL-filtered candidates ──────────────────────────────────
    // Over-fetch to leave room for ground items that fail the actuality check.
    let over_fetch = (budget * 3).max(batch);
    let candidates = state.lock().unwrap().drain_candidates(over_fetch, ttl);
    if candidates.is_empty() {
        return Vec::new();
    }

    // ── 3. Actuality check ─────────────────────────────────────────────────
    let dist_limit = if max_dist == 0 { observer.view_range() } else { max_dist };
    let px = observer.pos.x;
    let py = observer.pos.y;

    let mut to_probe: Vec<u32> = Vec::new();
    // Serials to mark handled without sending (out-of-range / evicted ground).
    let mut to_discard: Vec<u32> = Vec::new();

    for entry in candidates {
        let serial = entry.serial;
        match entry.kind {
            ProbeKind::Ground => {
                match observer.session.visible.get(serial) {
                    Some(entity) if entity.is_item() => {
                        let dx = (entity.x() as i32 - px as i32).unsigned_abs();
                        let dy = (entity.y() as i32 - py as i32).unsigned_abs();
                        let chebyshev = dx.max(dy) as u16;
                        if chebyshev <= dist_limit {
                            to_probe.push(serial);
                        } else {
                            debug!(
                                "{tag} probe: discard ground serial={serial:#010X} \
                                 dist={chebyshev} > limit={dist_limit}"
                            );
                            to_discard.push(serial);
                        }
                    }
                    Some(_) => {
                        // Serial is now a mobile or multi — not an item.
                        to_discard.push(serial);
                    }
                    None => {
                        // Item has left the visible set.
                        debug!(
                            "{tag} probe: discard ground serial={serial:#010X} not in visible"
                        );
                        to_discard.push(serial);
                    }
                }
            }
            ProbeKind::Container => {
                // No world coordinates available; TTL is the only filter.
                to_probe.push(serial);
            }
        }
    }

    // Mark discarded serials handled so they are never re-queued this session.
    if !to_discard.is_empty() {
        let mut st = state.lock().unwrap();
        for s in to_discard {
            st.mark_handled(s);
        }
    }

    if to_probe.is_empty() {
        return Vec::new();
    }

    // ── 4. Trim to packet budget ───────────────────────────────────────────
    let serials = if packets_for_serials(mode, tooltip_chunk, to_probe.len()) <= budget {
        to_probe
    } else {
        let max_s = serials_for_budget(mode, tooltip_chunk, budget);
        to_probe[..max_s.min(to_probe.len())].to_vec()
    };

    debug!("{tag} probe: sending {} serial(s) (mode={mode:?})", serials.len());

    // ── 5. Build packets ───────────────────────────────────────────────────
    let mut packets: Vec<RawPacket> = Vec::new();

    // SingleClick: one 0x09 per serial.
    if matches!(mode, ProbeMode::Single | ProbeMode::Both) {
        for &serial in &serials {
            packets.push(RawPacket::c2s(
                SingleClick { id: SingleClick::ID, serial }.to_bytes(),
            ));
        }
    }

    // MegaClilocRequest: group serials into chunks of tooltip_chunk.
    if matches!(mode, ProbeMode::Tooltip | ProbeMode::Both) {
        let chunk = tooltip_chunk.max(1);
        for group in serials.chunks(chunk) {
            packets.push(RawPacket::c2s(
                MegaClilocRequest::with_serials(group.to_vec()).to_bytes(),
            ));
        }
    }

    // ── 6. Mark sent serials as handled ───────────────────────────────────
    {
        let mut st = state.lock().unwrap();
        for serial in &serials {
            st.mark_handled(*serial);
        }
    }

    packets
}

// ── Packet-budget helpers ──────────────────────────────────────────────────

/// Number of probe packets produced for `n` serials in `mode` with the given
/// `tooltip_chunk` size.
fn packets_for_serials(mode: ProbeMode, chunk: usize, n: usize) -> usize {
    let chunk = chunk.max(1);
    let clicks = if matches!(mode, ProbeMode::Single | ProbeMode::Both) { n } else { 0 };
    let tooltips = if matches!(mode, ProbeMode::Tooltip | ProbeMode::Both) {
        n.div_ceil(chunk)
    } else {
        0
    };
    clicks + tooltips
}

/// Maximum number of serials that fit within a packet budget of `budget` for
/// the given mode and chunk size.
fn serials_for_budget(mode: ProbeMode, chunk: usize, budget: usize) -> usize {
    let chunk = chunk.max(1);
    if budget == 0 {
        return 0;
    }
    match mode {
        ProbeMode::Single => budget,
        ProbeMode::Tooltip => budget * chunk,
        ProbeMode::Both => {
            // Each serial costs 1 click + ceil(1/chunk) tooltip fractions.
            // We want max n such that n + ceil(n/chunk) <= budget.
            // Conservatively: n * (1 + 1/chunk) ≤ budget → n ≤ budget / (1 + 1/chunk).
            // Use integer arithmetic: n ≤ budget * chunk / (chunk + 1).
            budget * chunk / (chunk + 1)
        }
    }
}
