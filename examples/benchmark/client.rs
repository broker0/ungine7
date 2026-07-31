//! Benchmark client — connects to a bench-server and exchanges packets.
//!
//! Modes:
//! - Default (send+recv): blasts packets in batches, receives in batches.
//! - `--no-send`: receive-only.
//! - `--round-trip`: send one packet, wait for echo, measure latency.
//!   Prints min/avg/max/p99 latency every stats interval.
//!   Use with `bench-server --echo`.
//!
//! No login handshake — sends seed, then raw 0xBF packets.
//!
//! Usage: cargo run -p benchmark --bin bench-client -- \[OPTIONS\]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use clap::Parser;
use log::{debug, error, info, warn};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use protocol::protocol::{Protocol, ProtocolVersion};
use protocol::transport::builder::TransportBuilder;
use protocol::transport::{PacketTransport, TransportError, TransportEvent};

use common::logging::init_logger;


// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "bench-client", about = "UO protocol benchmark client")]
struct Args {
    /// Server address
    #[arg(short, long, default_value = "127.0.0.1:2593")]
    server: String,

    /// Disable sending packets (receive-only mode)
    #[arg(long, default_value_t = false)]
    no_send: bool,

    /// Round-trip mode: send one packet, wait for echo, measure latency.
    /// Use with `bench-server --echo`.
    #[arg(long, default_value_t = false)]
    round_trip: bool,

    /// Packet total size in bytes (min 5 for 0xBF header)
    #[arg(short, long, default_value_t = 64)]
    packet_size: usize,

    /// Number of parallel connections
    #[arg(short, long, default_value_t = 1)]
    connections: usize,

    /// Enable protocol encryption
    #[arg(short, long, default_value_t = false)]
    encrypted: bool,

    /// Statistics print interval in seconds
    #[arg(long, default_value_t = 5)]
    stats_interval: u64,

    /// Number of packets to send/recv per batch (ignored in round-trip mode)
    #[arg(long, default_value_t = 64)]
    batch_size: usize,

    /// Verbosity (-v = debug, -vv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Use a single-threaded tokio runtime instead of the default multi-threaded one
    #[arg(long)]
    single_thread: bool,
}


// ── Stats ──────────────────────────────────────────────────────────────────

struct Stats {
    recv_packets: AtomicU64,
    recv_bytes: AtomicU64,
    send_packets: AtomicU64,
    send_bytes: AtomicU64,
    active_connections: AtomicU64,
    /// Latency samples in microseconds, collected per stats interval
    latencies: Mutex<Vec<u64>>,
}

impl Stats {
    fn new() -> Self {
        Self {
            recv_packets: AtomicU64::new(0),
            recv_bytes: AtomicU64::new(0),
            send_packets: AtomicU64::new(0),
            send_bytes: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            latencies: Mutex::new(Vec::new()),
        }
    }

    fn snapshot_and_reset(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.recv_packets.swap(0, Ordering::Relaxed),
            self.recv_bytes.swap(0, Ordering::Relaxed),
            self.send_packets.swap(0, Ordering::Relaxed),
            self.send_bytes.swap(0, Ordering::Relaxed),
            self.active_connections.load(Ordering::Relaxed),
        )
    }

    async fn push_latency(&self, micros: u64) {
        self.latencies.lock().await.push(micros);
    }

    async fn drain_latencies(&self) -> Vec<u64> {
        let mut lock = self.latencies.lock().await;
        std::mem::take(&mut *lock)
    }
}

fn format_latency_stats(samples: &mut Vec<u64>) -> String {
    if samples.is_empty() {
        return "no samples".to_string();
    }
    samples.sort_unstable();
    let count = samples.len();
    let min = samples[0];
    let max = samples[count - 1];
    let sum: u64 = samples.iter().sum();
    let avg = sum / count as u64;
    let p50 = samples[count * 50 / 100];
    let p99 = samples[count * 99 / 100];

    if max >= 1_000_000 {
        // Show in ms
        format!(
            "{count} samples | min={:.2}ms avg={:.2}ms p50={:.2}ms p99={:.2}ms max={:.2}ms",
            min as f64 / 1000.0, avg as f64 / 1000.0,
            p50 as f64 / 1000.0, p99 as f64 / 1000.0,
            max as f64 / 1000.0,
        )
    } else {
        format!(
            "{count} samples | min={min}us avg={avg}us p50={p50}us p99={p99}us max={max}us",
        )
    }
}


// ── Helpers ────────────────────────────────────────────────────────────────

fn build_bench_packet(total_size: usize) -> Bytes {
    let size = total_size.max(5);
    let mut buf = vec![0u8; size];
    buf[0] = 0xBF;
    let len = size as u16;
    buf[1] = (len >> 8) as u8;
    buf[2] = (len & 0xFF) as u8;
    buf[3] = 0xFF;
    buf[4] = 0xFF;
    for i in 5..size {
        buf[i] = (i & 0xFF) as u8;
    }
    Bytes::from(buf)
}

fn format_rate(bytes_per_sec: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes_per_sec >= GB {
        format!("{:.2} GB/s", bytes_per_sec / GB)
    } else if bytes_per_sec >= MB {
        format!("{:.2} MB/s", bytes_per_sec / MB)
    } else if bytes_per_sec >= KB {
        format!("{:.2} KB/s", bytes_per_sec / KB)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}


// ── Constants ──────────────────────────────────────────────────────────────

const VERSION: ProtocolVersion = ProtocolVersion::new(7, 0, 95, 0);
const SEED: u32 = 0xDEADBEEF;
const AUTH_KEY: u32 = 0xCAFE_0000;


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
    let stats = Arc::new(Stats::new());
    let packet = build_bench_packet(args.packet_size);
    let round_trip = args.round_trip;

    let mode_name = if round_trip {
        "round-trip"
    } else if args.no_send {
        "recv-only"
    } else {
        "send+recv"
    };

    info!("bench-client starting");
    info!(
        "target={}, connections={}, mode={mode_name}, packet_size={}, encrypted={}, batch_size={}",
        args.server, args.connections, args.packet_size, args.encrypted, args.batch_size,
    );

    // Stats printer
    let stats_clone = stats.clone();
    let stats_interval = args.stats_interval;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(stats_interval));
        interval.tick().await;
        loop {
            interval.tick().await;
            let (rp, rb, sp, sb, conns) = stats_clone.snapshot_and_reset();
            let elapsed = stats_interval as f64;

            if round_trip {
                let mut samples = stats_clone.drain_latencies().await;
                info!(
                    "[stats] conns={conns} | rtt: {} | {:.0} round-trips/s",
                    format_latency_stats(&mut samples),
                    rp as f64 / elapsed,
                );
            } else {
                info!(
                    "[stats] conns={conns} | recv: {:.0} pps ({}) | send: {:.0} pps ({})",
                    rp as f64 / elapsed, format_rate(rb as f64 / elapsed),
                    sp as f64 / elapsed, format_rate(sb as f64 / elapsed),
                );
            }
        }
    });

    let mut handles = Vec::new();
    let batch_size = args.batch_size;
    let encrypted = args.encrypted;
    let no_send = args.no_send;

    for i in 0..args.connections {
        let server = args.server.clone();
        let stats = stats.clone();
        let packet = packet.clone();

        handles.push(tokio::spawn(async move {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(i as u64 * 10)).await;
            }
            if let Err(e) = run_client(
                i, &server, encrypted, stats, packet,
                batch_size, round_trip, no_send,
            ).await {
                error!("[conn-{i}] error: {e}");
            }
        }));
    }

    for handle in handles {
        handle.await?;
    }

    info!("bench-client finished");
    Ok(())
}


// ── Client connection ──────────────────────────────────────────────────────

async fn run_client(
    id: usize,
    server: &str,
    encrypted: bool,
    stats: Arc<Stats>,
    packet: Bytes,
    batch_size: usize,
    round_trip: bool,
    no_send: bool,
) -> anyhow::Result<()> {
    info!("[conn-{id}] connecting to {server}...");

    let stream = TcpStream::connect(server).await?;
    info!("[conn-{id}] connected to {}", stream.peer_addr()?);

    let protocol = Protocol::game(SEED, AUTH_KEY, VERSION, encrypted);
    let (mut transport, _) = TransportBuilder::client(stream, &protocol).build()?;

    transport.send(TransportEvent::Seed(Bytes::copy_from_slice(&SEED.to_be_bytes()))).await?;
    transport.flush().await?;

    info!("[conn-{id}] seed sent, starting benchmark");
    stats.active_connections.fetch_add(1, Ordering::Relaxed);

    let result = if round_trip {
        loop_round_trip(id, &mut transport, &stats, &packet).await
    } else if no_send {
        loop_recv_only(id, &mut transport, &stats).await
    } else {
        loop_send_recv(id, &mut transport, &stats, &packet, batch_size).await
    };

    stats.active_connections.fetch_sub(1, Ordering::Relaxed);
    transport.close().await;

    if let Err(ref e) = result {
        warn!("[conn-{id}] bench loop ended: {e}");
    }
    info!("[conn-{id}] disconnected");
    result
}


// ── Loops ──────────────────────────────────────────────────────────────────

async fn loop_send_recv(
    id: usize,
    transport: &mut Box<dyn PacketTransport>,
    stats: &Stats,
    packet: &Bytes,
    batch_size: usize,
) -> anyhow::Result<()> {
    let pkt_len = packet.len() as u64;
    loop {
        for _ in 0..batch_size {
            transport.send(TransportEvent::Packet(packet.clone())).await?;
            stats.send_packets.fetch_add(1, Ordering::Relaxed);
            stats.send_bytes.fetch_add(pkt_len, Ordering::Relaxed);
        }
        transport.flush().await?;

        for _ in 0..batch_size {
            match transport.recv().await {
                Ok(TransportEvent::Packet(data)) => {
                    stats.recv_packets.fetch_add(1, Ordering::Relaxed);
                    stats.recv_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(TransportError::Closed) => { debug!("[conn-{id}] disconnected"); return Ok(()); }
                Err(e) => anyhow::bail!("recv: {e}"),
            }
        }
    }
}

async fn loop_recv_only(
    id: usize,
    transport: &mut Box<dyn PacketTransport>,
    stats: &Stats,
) -> anyhow::Result<()> {
    loop {
        match transport.recv().await {
            Ok(TransportEvent::Packet(data)) => {
                stats.recv_packets.fetch_add(1, Ordering::Relaxed);
                stats.recv_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(TransportError::Closed) => { debug!("[conn-{id}] disconnected"); break; }
            Err(e) => anyhow::bail!("recv: {e}"),
        }
    }
    Ok(())
}

async fn loop_round_trip(
    id: usize,
    transport: &mut Box<dyn PacketTransport>,
    stats: &Stats,
    packet: &Bytes,
) -> anyhow::Result<()> {
    let pkt_len = packet.len() as u64;

    loop {
        let t0 = Instant::now();

        transport.send(TransportEvent::Packet(packet.clone())).await?;
        transport.flush().await?;
        stats.send_packets.fetch_add(1, Ordering::Relaxed);
        stats.send_bytes.fetch_add(pkt_len, Ordering::Relaxed);

        // Wait for echo
        loop {
            match transport.recv().await {
                Ok(TransportEvent::Packet(data)) => {
                    let rtt = t0.elapsed();
                    stats.recv_packets.fetch_add(1, Ordering::Relaxed);
                    stats.recv_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                    stats.push_latency(rtt.as_micros() as u64).await;
                    break;
                }
                Ok(_) => {}
                Err(TransportError::Closed) => {
                    debug!("[conn-{id}] disconnected");
                    return Ok(());
                }
                Err(e) => anyhow::bail!("recv: {e}"),
            }
        }
    }
}
