//! Movement benchmark — full login sequence + periodic MoveRequest for N clients.
//!
//! Each client:
//! 1. Connects to the login server, authenticates with serial account/password
//!    (bench1/bench1, bench2/bench2, ...), selects server, follows redirect
//! 2. Enters the game world (GameLogin → CharacterList → LoginCharacter → LoginComplete)
//! 3. Sends MoveRequest every --move-interval ms, measures MoveAck round-trip
//!
//! Movement is limited by `--max-pending` (default 2): no new MoveRequest is
//! sent until earlier ones are acknowledged or rejected (like the real UO
//! client).  On MoveReject the direction is changed immediately.
//!
//! After walking 3-20 steps the client either picks a new direction or
//! pauses for 2-5 seconds (50/50 chance), simulating natural player idle.
//!
//! Metrics: login time, time-to-first-move, MoveRequest→MoveAck RTT, PPS.
//!
//! Usage:
//!   cargo run -p benchmark --bin bench-movement -- -s 127.0.0.1:2593 -c 10
//!
//! Works with any UO server that accepts login + movement (e.g. `examples/server`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use log::{debug, error, info, trace, warn};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use tokio::sync::Mutex;

use protocol::RawPacket;
use protocol::connector::ConnectorConfig;
use protocol::packets::system::Ping;
use packets::traits::{encode_packet, BasicPacket};
use u_core::ProtocolVersion;

use network::client::{ClientConfig, PacketClient};
use network::session::SessionEvent;

use packets::movement::{MoveAck, MoveReject, MoveRequest};

use common::logging::init_logger;


// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "bench-movement", about = "UO login + movement benchmark")]
struct Args {
    /// Server address (login server)
    #[arg(short, long, default_value = "127.0.0.1:2593")]
    server: String,

    /// Number of parallel clients
    #[arg(short, long, default_value_t = 1)]
    connections: usize,

    /// Account name prefix (clients get test1, test2, ...)
    #[arg(short, long, default_value = "test")]
    account: String,

    /// Client version
    #[arg(long, default_value = "3.0.8.0")]
    client_version: String,

    /// Enable protocol encryption
    #[arg(short, long, default_value_t = false)]
    encrypted: bool,

    /// MoveRequest interval in milliseconds
    #[arg(long, default_value_t = 100)]
    move_interval: u64,

    /// Maximum unacknowledged MoveRequests per client (1-4)
    #[arg(long, default_value_t = 2)]
    max_pending: usize,

    /// Probability of sending a move on each timer tick (0 = never move, 100 = always move)
    #[arg(long, default_value_t = 100)]
    move_rate: u8,

    /// Continue the current step series after a MoveReject instead of stopping immediately
    #[arg(long)]
    ignore_reject: bool,

    /// Step series length range [MIN MAX] — after each series the client turns or pauses.
    /// Both values must be >= 1 and MIN <= MAX. (default: 3 20)
    #[arg(long, num_args = 2, value_names = ["MIN", "MAX"], default_values_t = vec![3u32, 20u32])]
    run_length: Vec<u32>,

    /// Pause duration range in seconds [MIN MAX] between step series (ignored when --move-rate=100).
    /// (default: 2 5)
    #[arg(long, num_args = 2, value_names = ["MIN", "MAX"], default_values_t = vec![2u64, 5u64])]
    pause_duration: Vec<u64>,

    /// Login seed
    #[arg(long, default_value_t = 0xDEADBEEF)]
    seed: u32,

    /// Server index to select (0-based)
    #[arg(long, default_value_t = 0)]
    server_index: u16,

    /// Statistics print interval in seconds
    #[arg(long, default_value_t = 5)]
    stats_interval: u64,

    /// Use a single-threaded tokio runtime
    #[arg(long)]
    single_thread: bool,

    /// Verbosity (-v = debug, -vv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}


// ── Stats ──────────────────────────────────────────────────────────────────

struct Stats {
    /// Clients currently in steady-state (post-login)
    active: AtomicU64,
    /// Successful logins
    logins_ok: AtomicU64,
    /// Failed logins
    logins_failed: AtomicU64,
    /// MoveRequests sent this interval
    moves_sent: AtomicU64,
    /// MoveAcks received this interval
    moves_acked: AtomicU64,
    /// MoveRejects received this interval
    moves_rejected: AtomicU64,
    /// Moves skipped because pending queue was full
    moves_skipped: AtomicU64,
    /// Login time samples (microseconds)
    login_times: Mutex<Vec<u64>>,
    /// MoveRequest→MoveAck RTT samples (microseconds)
    move_rtts: Mutex<Vec<u64>>,
}

impl Stats {
    fn new() -> Self {
        Self {
            active: AtomicU64::new(0),
            logins_ok: AtomicU64::new(0),
            logins_failed: AtomicU64::new(0),
            moves_sent: AtomicU64::new(0),
            moves_acked: AtomicU64::new(0),
            moves_rejected: AtomicU64::new(0),
            moves_skipped: AtomicU64::new(0),
            login_times: Mutex::new(Vec::new()),
            move_rtts: Mutex::new(Vec::new()),
        }
    }

    fn reset_counters(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.moves_sent.swap(0, Ordering::Relaxed),
            self.moves_acked.swap(0, Ordering::Relaxed),
            self.moves_rejected.swap(0, Ordering::Relaxed),
            self.moves_skipped.swap(0, Ordering::Relaxed),
            self.logins_failed.swap(0, Ordering::Relaxed),
        )
    }

    async fn push_login_time(&self, micros: u64) {
        self.login_times.lock().await.push(micros);
    }

    async fn push_move_rtt(&self, micros: u64) {
        self.move_rtts.lock().await.push(micros);
    }

    async fn drain_login_times(&self) -> Vec<u64> {
        std::mem::take(&mut *self.login_times.lock().await)
    }

    async fn drain_move_rtts(&self) -> Vec<u64> {
        std::mem::take(&mut *self.move_rtts.lock().await)
    }
}

fn format_latency(samples: &mut [u64]) -> String {
    if samples.is_empty() {
        return "-".to_string();
    }
    samples.sort_unstable();
    let count = samples.len();
    let min = samples[0];
    let max = samples[count - 1];
    let sum: u64 = samples.iter().sum();
    let avg = sum / count as u64;
    let p50 = samples[count * 50 / 100];
    let p99 = samples[count * 99 / 100];

    if max >= 100_000 {
        format!(
            "min={:.1}ms avg={:.1}ms p50={:.1}ms p99={:.1}ms max={:.1}ms (n={})",
            min as f64 / 1000.0, avg as f64 / 1000.0,
            p50 as f64 / 1000.0, p99 as f64 / 1000.0,
            max as f64 / 1000.0, count,
        )
    } else {
        format!(
            "min={}us avg={}us p50={}us p99={}us max={}us (n={})",
            min, avg, p50, p99, max, count,
        )
    }
}


// ── Main ───────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let log_level = match args.verbose {
        0 => log::LevelFilter::Info,
        1 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    init_logger().level(log_level).build()?;

    let runtime = if args.single_thread {
        info!("using single-threaded runtime");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
    };

    runtime.block_on(run(args))
}

async fn run(args: Args) -> anyhow::Result<()> {
    let version: ProtocolVersion = args.client_version.parse()
        .map_err(|_| anyhow::anyhow!("invalid client version: {}", args.client_version))?;

    let max_pending = args.max_pending.clamp(1, 4);

    let run_len_min = args.run_length[0].max(1);
    let run_len_max = args.run_length[1].max(run_len_min);

    let pause_min = args.pause_duration[0];
    let pause_max = args.pause_duration[1].max(pause_min);

    let stats = Arc::new(Stats::new());

    info!("bench-movement starting");
    info!(
        "server={}, connections={}, account_prefix={}, version={}, encrypted={}, move_interval={}ms, max_pending={}, move_rate={}%, ignore_reject={}, run_length={}..={}, pause_duration={}..={}s",
        args.server, args.connections, args.account, version, args.encrypted, args.move_interval, max_pending, args.move_rate, args.ignore_reject, run_len_min, run_len_max, pause_min, pause_max,
    );

    // Stats printer
    let stats_clone = stats.clone();
    let stats_interval = args.stats_interval;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(stats_interval));
        interval.tick().await;
        loop {
            interval.tick().await;
            let active = stats_clone.active.load(Ordering::Relaxed);
            let (sent, acked, rejected, skipped, login_errs) = stats_clone.reset_counters();
            let elapsed = stats_interval as f64;

            let mut login_samples = stats_clone.drain_login_times().await;
            let mut rtt_samples = stats_clone.drain_move_rtts().await;

            let login_str = if login_samples.is_empty() {
                String::new()
            } else {
                format!(" | login: {}", format_latency(&mut login_samples))
            };

            let skip_str = if skipped > 0 {
                format!(" {:.0} skip/s", skipped as f64 / elapsed)
            } else {
                String::new()
            };

            info!(
                "[stats] active={active} | move: {:.0} sent/s {:.0} ack/s {:.0} reject/s{skip_str} | rtt: {} | errors={login_errs}{login_str}",
                sent as f64 / elapsed,
                acked as f64 / elapsed,
                rejected as f64 / elapsed,
                format_latency(&mut rtt_samples),
            );
        }
    });

    // Spawn clients
    let mut handles = Vec::new();
    for i in 0..args.connections {
        let server = args.server.clone();
        let account = format!("{}{}", args.account, i + 1);
        let password = account.clone();
        let stats = stats.clone();
        let seed = args.seed.wrapping_add(i as u32);
        let server_index = args.server_index;
        let move_interval = args.move_interval;
        let encrypted = args.encrypted;
        let move_rate = args.move_rate.clamp(0, 100);
        let ignore_reject = args.ignore_reject;

        handles.push(tokio::spawn(async move {
            // Stagger connections
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(i as u64 * 50)).await;
            }
            if let Err(e) = run_client(
                i, &server, &account, &password, version, encrypted,
                seed, server_index, move_interval, max_pending, move_rate, ignore_reject,
                run_len_min, run_len_max, pause_min, pause_max, stats,
            ).await {
                error!("[client-{i}] {e}");
            }
        }));
    }

    for handle in handles {
        handle.await?;
    }

    // Final summary
    let ok = stats.logins_ok.load(Ordering::Relaxed);
    let failed = stats.logins_failed.load(Ordering::Relaxed);
    info!("bench-movement finished: {ok}/{} logins succeeded, {failed} failed",
        ok + failed);
    Ok(())
}


// ── Client ─────────────────────────────────────────────────────────────────

async fn run_client(
    id: usize,
    server: &str,
    account: &str,
    password: &str,
    version: ProtocolVersion,
    encrypted: bool,
    seed: u32,
    server_index: u16,
    move_interval_ms: u64,
    max_pending: usize,
    move_rate: u8,
    ignore_reject: bool,
    run_len_min: u32,
    run_len_max: u32,
    pause_min: u64,
    pause_max: u64,
    stats: Arc<Stats>,
) -> anyhow::Result<()> {
    let tag = format!("client-{id}");

    let client = PacketClient::new(ClientConfig {
        version,
        encrypted,
        connector: ConnectorConfig::Direct,
    });

    // ── Login phase ────────────────────────────────────────────────────

    let login_start = Instant::now();

    trace!("[{tag}] connecting to {server} as '{account}'...");

    let mut login = match client.connect_login(server, seed).await {
        Ok(l) => l,
        Err(e) => {
            stats.logins_failed.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("connect failed: {e}");
        }
    };

    if let Err(e) = login.authenticate(account, password).await {
        stats.logins_failed.fetch_add(1, Ordering::Relaxed);
        anyhow::bail!("authenticate failed: {e}");
    }

    let redirect = match login.select_server(server_index).await {
        Ok(r) => r,
        Err(e) => {
            stats.logins_failed.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("select_server failed: {e}");
        }
    };

    trace!(
        "[{tag}] redirect to {} (auth_key=0x{:08X})",
        redirect.address(), redirect.auth_key
    );

    let mut game = match login.into_game(&redirect).await {
        Ok(g) => g,
        Err(e) => {
            stats.logins_failed.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("into_game failed: {e}");
        }
    };

    let char_info = match game.enter_world(account, password).await {
        Ok(info) => info,
        Err(e) => {
            stats.logins_failed.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("enter_world failed: {e}");
        }
    };

    let login_time = login_start.elapsed();
    stats.push_login_time(login_time.as_micros() as u64).await;
    stats.logins_ok.fetch_add(1, Ordering::Relaxed);
    trace!(
        "[{tag}] logged in as '{}' at ({},{},{}) in {:.1}ms",
        char_info.name, char_info.x, char_info.y, char_info.z,
        login_time.as_secs_f64() * 1000.0,
    );

    stats.active.fetch_add(1, Ordering::Relaxed);

    // ── Steady state: MoveRequest loop ─────────────────────────────────

    let result = movement_loop(
        &tag, &mut game, &stats, move_interval_ms, max_pending, move_rate, ignore_reject,
        run_len_min, run_len_max, pause_min, pause_max,
    ).await;

    stats.active.fetch_sub(1, Ordering::Relaxed);
    game.close().await;

    if let Err(ref e) = result {
        warn!("[{tag}] movement loop ended: {e}");
    }
    trace!("[{tag}] disconnected");
    Ok(())
}


/// Steady-state loop: send MoveRequest on timer, receive all packets,
/// track MoveAck RTT.
///
/// Walks in one direction for `run_len_min..=run_len_max` steps, then
/// either picks a new random direction immediately or pauses for 2-5
/// seconds (50/50 chance).  On MoveReject the direction is changed
/// immediately.
///
/// Limits the number of unacknowledged moves to `max_pending` — when
/// the pending queue is full, timer ticks are skipped (like the real
/// UO client).
async fn movement_loop(
    tag: &str,
    game: &mut network::client::GameConnection,
    stats: &Stats,
    move_interval_ms: u64,
    max_pending: usize,
    move_rate: u8,
    ignore_reject: bool,
    run_len_min: u32,
    run_len_max: u32,
    pause_min: u64,
    pause_max: u64,
) -> anyhow::Result<()> {
    let mut rng = SmallRng::from_os_rng();
    let mut sequence: u8 = 0;
    let mut direction: u8 = rng.random_range(0..8);
    let mut steps_remaining: u32 = rng.random_range(run_len_min..=run_len_max);

    // Pending move timestamps indexed by sequence number.
    let mut pending_times: [Option<Instant>; 256] = [const { None }; 256];
    // Count of currently unacknowledged moves.
    let mut pending_count: usize = 0;

    // Pause state: when Some, the client is idling until this instant.
    let mut pause_until: Option<Instant> = None;

    let mut move_timer = tokio::time::interval(Duration::from_millis(move_interval_ms));
    move_timer.tick().await; // skip first immediate tick

    loop {
        tokio::select! {
            biased;

            // Timer: send MoveRequest (only if pending queue has room)
            _ = move_timer.tick() => {
                // If pausing, check whether the pause has elapsed.
                if let Some(resume_at) = pause_until {
                    if Instant::now() < resume_at {
                        continue;
                    }
                    // Pause is over — pick a new direction and resume walking.
                    pause_until = None;
                    direction = rng.random_range(0..8) | 128;
                    steps_remaining = rng.random_range(run_len_min..=run_len_max);
                    debug!("[{tag}] pause ended, resuming dir={direction} steps={steps_remaining}");
                }

                if pending_count >= max_pending {
                    stats.moves_skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                // move_rate: 0 = never move, 100 = always move
                if move_rate < 100 && !rng.random_bool(move_rate as f64 / 100.0) {
                    stats.moves_skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                let req = MoveRequest {
                    id: MoveRequest::ID,
                    direction,
                    sequence,
                    fastwalk_key: 0,
                };
                pending_times[sequence as usize] = Some(Instant::now());
                pending_count += 1;

                game.send(RawPacket::c2s(encode_packet(&req))).await?;
                stats.moves_sent.fetch_add(1, Ordering::Relaxed);

                sequence = sequence.wrapping_add(1);
                steps_remaining -= 1;
                if steps_remaining == 0 {
                    // At move_rate=100 never pause — just pick a new direction immediately.
                    // Otherwise 50/50: new direction or pause for pause_min..=pause_max s.
                    if move_rate < 100 && rng.random_bool(0.5) {
                        let pause_secs = rng.random_range(pause_min..=pause_max);
                        pause_until = Some(Instant::now() + Duration::from_secs(pause_secs));
                        debug!("[{tag}] pausing for {pause_secs}s");
                    } else {
                        direction = rng.random_range(0..8) | 128;
                        steps_remaining = rng.random_range(run_len_min..=run_len_max);
                    }
                }
            }

            // Receive packets
            event = game.recv() => {
                match event.event {
                    SessionEvent::Packet(p) => {
                        match p.id() {
                            // MoveAck
                            0x22 => {
                                if let Ok(ack) = MoveAck::from_bytes(&p.data) {
                                    stats.moves_acked.fetch_add(1, Ordering::Relaxed);
                                    if let Some(sent_at) = pending_times[ack.sequence as usize].take() {
                                        pending_count = pending_count.saturating_sub(1);
                                        let rtt = sent_at.elapsed();
                                        stats.push_move_rtt(rtt.as_micros() as u64).await;
                                    }
                                }
                            }
                            // MoveReject — change direction, clear all pending
                            0x21 => {
                                if let Ok(reject) = MoveReject::from_bytes(&p.data) {
                                    stats.moves_rejected.fetch_add(1, Ordering::Relaxed);

                                    // Clear all pending moves (server invalidates them)
                                    for slot in pending_times.iter_mut() {
                                        *slot = None;
                                    }
                                    pending_count = 0;

                                    // Pick a new random direction
                                    direction = rng.random_range(0..8) | 128;

                                    if !ignore_reject {
                                        // Default: reset the step series on reject
                                        steps_remaining = rng.random_range(run_len_min..=run_len_max);
                                    }
                                    // If ignore_reject is set, keep steps_remaining unchanged
                                    // so the current series continues until it naturally ends.

                                    debug!(
                                        "[{tag}] move rejected seq={} snap=({},{},{}) new_dir={} steps_remaining={}",
                                        reject.sequence, reject.x, reject.y, reject.z, direction, steps_remaining,
                                    );
                                }
                            }
                            // Ping → Pong
                            0x73 => {
                                if let Ok(ping) = Ping::from_bytes(&p.data) {
                                    debug!("[{tag}] ping seq={}", ping.sequence);
                                    game.send(RawPacket::c2s(encode_packet(&ping))).await?;
                                }
                            }
                            _ => {
                                debug!("[{tag}] recv 0x{:02X} ({} bytes)", p.id(), p.len());
                            }
                        }
                    }
                    SessionEvent::Disconnected => {
                        info!("[{tag}] server disconnected");
                        break;
                    }
                    SessionEvent::Error(e) => {
                        anyhow::bail!("session error: {e}");
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
