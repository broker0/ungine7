//! mirror-proxy — a UO proxy that relays a game client to the real server and
//! mirrors every server-to-client packet to an external WebSocket endpoint
//! (e.g. path-server's `/ws/mirror`).
//!
//! Usage:
//!
//! ```text
//! mirror-proxy \
//!     --server 127.0.0.1:2593 \
//!     --proxy-host 127.0.0.1 --proxy-port 2595 \
//!     --client-version 7.0.0.0 --encrypted=false \
//!     --mirror-url ws://127.0.0.1:8080/ws/mirror
//! ```
//!
//! Optional proactive probing (disabled by default):
//!
//! ```text
//! mirror-proxy ... \
//!     --probe-objects \
//!     --probe-mode auto \
//!     --probe-interval-ms 250 \
//!     --probe-batch 20 \
//!     --probe-tooltip-chunk 10 \
//!     --probe-pps 0 \
//!     --probe-burst 0 \
//!     --probe-ttl-ms 0 \
//!     --probe-max-dist 0
//! ```

mod mirror;
mod probe;
mod proxy;

use clap::Parser;
use log::info;

use common::args::{ProxyArgs, VerbosityArgs};
use network::listener::{ListenerConfig, Listener};
use u_core::ProtocolVersion;

use crate::probe::ProbeModeArg;
use crate::proxy::MirrorProxy;

#[derive(Parser, Debug)]
#[command(name = "mirror-proxy", about = "UO proxy that mirrors S2C packets to a WebSocket endpoint")]
struct Config {
    #[command(flatten)]
    proxy: ProxyArgs,

    /// WebSocket URL of the mirror endpoint to stream S2C packets to.
    ///
    /// Every game-phase session opens an outgoing WebSocket connection to this
    /// URL and forwards all server-to-client packets as binary frames.
    /// Example: `ws://127.0.0.1:8080/ws/mirror`
    #[arg(long, value_name = "URL")]
    mirror_url: String,

    /// Do not mirror 0x1D DeleteObject packets.
    ///
    /// The UO server sends 0x1D both for real removals (item picked up /
    /// destroyed) and for objects merely leaving the player's view range.
    /// With this flag set, 0x1D is dropped from the mirror stream so once-seen
    /// objects accumulate on the mirror endpoint (e.g. path-server) instead of
    /// despawning.  The real client still receives 0x1D unchanged.
    ///
    /// Default (flag absent): every S2C packet is mirrored, matching
    /// `rpc-proxy`.
    #[arg(long, default_value_t = false)]
    block_delete: bool,

    // ── Probing ────────────────────────────────────────────────────────────

    /// Enable proactive object probing and forced custom-house requests.
    ///
    /// When set, the proxy watches S2C traffic for newly-seen item objects
    /// (0x1A, 0xF3, 0xF7, 0x25, 0x3C, 0x2E) and periodically sends
    /// single-click (0x09) and/or tooltip requests (0xD6) to the server so
    /// item properties are collected even when the client never interacts with
    /// them.  Mobiles and multi-objects (houses, boats) are ignored.
    ///
    /// Ground items are probed only while they are within the player's view
    /// range (tracked in real time via the built-in world observer).  Items
    /// that have left view are silently discarded from the probe queue.
    ///
    /// Container and equipped items are probed unconditionally (no world
    /// coordinates available); use --probe-ttl-ms to discard stale entries.
    ///
    /// Additionally, whenever a 0xBF:0x001D HouseRevisionState arrives from
    /// the server, a 0xBF:0x001E RequestHouseState is sent back immediately
    /// (deduped per house) to force the full custom-house design through the
    /// connection even when the client has it cached.
    ///
    /// Requests the real client already sends (0x09/0x06/0xD6) are never
    /// duplicated.  Each serial is probed at most once per session.
    #[arg(long, default_value_t = false)]
    probe_objects: bool,

    /// Probe request type: auto | single | tooltip | both.
    ///
    /// `auto` (default): send 0xD6 MegaClilocRequest for AOS+ clients
    /// (version >= 4.0.0.0), 0x09 SingleClick for older clients.
    /// `single`: always send 0x09 SingleClick.
    /// `tooltip`: always send 0xD6 MegaClilocRequest.
    /// `both`: send both 0x09 and 0xD6 for every new object.
    ///
    /// Only meaningful when --probe-objects is set.
    #[arg(long, value_name = "MODE", default_value = "auto")]
    probe_mode: ProbeModeArg,

    /// Interval between probe-request flushes to the server, in milliseconds.
    ///
    /// Newly-discovered serials accumulate in a queue and are sent in batches
    /// at this interval.  Only meaningful when --probe-objects is set.
    #[arg(long, value_name = "MS", default_value_t = 250)]
    probe_interval_ms: u64,

    /// Maximum number of probe *packets* to send per interval tick.
    ///
    /// A single 0x09 SingleClick and each 0xD6 MegaClilocRequest chunk each
    /// count as one packet.  Only meaningful when --probe-objects is set.
    #[arg(long, value_name = "N", default_value_t = 20)]
    probe_batch: usize,

    /// Maximum number of serials packed into a single 0xD6 MegaClilocRequest.
    ///
    /// e.g. `--probe-batch 20 --probe-tooltip-chunk 10` sends at most two 0xD6
    /// packets per tick (10 serials each).  Minimum effective value is 1 (one
    /// serial per packet, matching real client behaviour).  Only meaningful
    /// when --probe-objects is set and the resolved probe mode includes tooltip
    /// requests.
    #[arg(long, value_name = "N", default_value_t = 10)]
    probe_tooltip_chunk: usize,

    /// Rate limit: maximum probe packets sent per second (0 = unlimited).
    ///
    /// Uses a token-bucket algorithm.  Set to a low value (e.g. 5–10) when
    /// the server complains about too-frequent requests.  See also
    /// --probe-burst.  Only meaningful when --probe-objects is set.
    #[arg(long, value_name = "N", default_value_t = 0)]
    probe_pps: u32,

    /// Token-bucket burst capacity for the probe rate limiter.
    ///
    /// Allows a short burst of up to this many packets before the steady-state
    /// --probe-pps rate kicks in.  0 (default) sets burst equal to pps (no
    /// extra burst headroom).  Ignored when --probe-pps is 0.
    #[arg(long, value_name = "N", default_value_t = 0)]
    probe_burst: u32,

    /// Discard queued probe serials older than this many milliseconds (0 = keep forever).
    ///
    /// Useful to avoid sending stale requests when the probe queue grows
    /// faster than it is drained (e.g. at very low --probe-pps).  Container
    /// and equipped items rely entirely on this for relevance filtering.
    /// Only meaningful when --probe-objects is set.
    #[arg(long, value_name = "MS", default_value_t = 0)]
    probe_ttl_ms: u64,

    /// Maximum Chebyshev distance from the player for a ground item to be probed.
    ///
    /// Ground items farther than this (in tiles) are discarded from the queue
    /// without being probed.  0 (default) uses the current server-reported
    /// view range.  Only meaningful when --probe-objects is set.
    #[arg(long, value_name = "TILES", default_value_t = 0)]
    probe_max_dist: u16,

    #[command(flatten)]
    verbosity: VerbosityArgs,
}

impl Config {
    /// Build the `with_allowed` list for the listener.
    fn allowed(&self) -> Vec<(Option<ProtocolVersion>, Option<bool>)> {
        vec![(Some(self.proxy.client_version), Some(self.proxy.encrypted))]
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse();
    config
        .verbosity
        .logger(["mirror_proxy", "common"])
        .build()?;

    info!("=== mirror-proxy starting ===");
    config.proxy.log_info();
    info!("Mirror URL: {}", config.mirror_url);
    info!("Block 0x1D: {}", config.block_delete);
    info!("Probe objects: {}", config.probe_objects);
    if config.probe_objects {
        info!(
            "  mode={}, interval={}ms, batch={}, tooltip-chunk={}",
            config.probe_mode,
            config.probe_interval_ms,
            config.probe_batch,
            config.probe_tooltip_chunk,
        );
        info!(
            "  pps={}, burst={}, ttl={}ms, max-dist={}",
            config.probe_pps,
            config.probe_burst,
            config.probe_ttl_ms,
            config.probe_max_dist,
        );
    }

    let proxy_addr = config.proxy.proxy_addr();
    let listen_addr = format!("0.0.0.0:{}", config.proxy.proxy_port);

    let listener_config =
        ListenerConfig::new(listen_addr).with_allowed(config.allowed());

    let handler = MirrorProxy::new(
        proxy_addr,
        config.proxy.server.clone(),
        config.mirror_url.clone(),
        config.block_delete,
        config.probe_objects,
        config.probe_mode,
        config.probe_interval_ms,
        config.probe_batch,
        config.probe_tooltip_chunk,
        config.probe_pps,
        config.probe_burst,
        config.probe_ttl_ms,
        config.probe_max_dist,
    );

    Listener::new(listener_config, handler).run().await?;
    Ok(())
}
