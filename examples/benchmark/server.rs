//! Benchmark server — accepts game connections and exchanges packets.
//!
//! Modes:
//! - Default (send+recv): blasts packets in batches, receives in batches.
//! - `--no-send`: receive-only sink.
//! - `--echo`: receives a packet, immediately sends it back (for round-trip latency).
//!
//! No login handshake — client sends seed, then raw 0xBF packets.
//!
//! Usage: cargo run -p benchmark --bin bench-server -- \[OPTIONS\]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use log::{debug, error, info};
use tokio::net::TcpListener;

use protocol::protocol::{Protocol, ProtocolVersion};
use protocol::transport::builder::TransportBuilder;
use protocol::transport::{PacketTransport, TransportError, TransportEvent};

use common::logging::init_logger;


// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "bench-server", about = "UO protocol benchmark server")]
struct Args {
    /// Listen address
    #[arg(short, long, default_value = "0.0.0.0:2593")]
    listen: String,

    /// Disable sending packets (receive-only sink)
    #[arg(long, default_value_t = false)]
    no_send: bool,

    /// Echo mode: receive a packet, send it back immediately (for round-trip)
    #[arg(long, default_value_t = false)]
    echo: bool,

    /// Packet total size in bytes (min 5 for 0xBF header)
    #[arg(short, long, default_value_t = 64)]
    packet_size: usize,

    /// Enable protocol encryption
    #[arg(short, long, default_value_t = false)]
    encrypted: bool,

    /// Statistics print interval in seconds
    #[arg(long, default_value_t = 5)]
    stats_interval: u64,

    /// Number of packets to send/recv per batch (ignored in echo mode)
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
    connections: AtomicU64,
}

impl Stats {
    fn new() -> Self {
        Self {
            recv_packets: AtomicU64::new(0),
            recv_bytes: AtomicU64::new(0),
            send_packets: AtomicU64::new(0),
            send_bytes: AtomicU64::new(0),
            connections: AtomicU64::new(0),
        }
    }

    fn snapshot_and_reset(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.recv_packets.swap(0, Ordering::Relaxed),
            self.recv_bytes.swap(0, Ordering::Relaxed),
            self.send_packets.swap(0, Ordering::Relaxed),
            self.send_bytes.swap(0, Ordering::Relaxed),
            self.connections.load(Ordering::Relaxed),
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
const AUTH_KEY: u32 = 0xCAFE0000;


// ── Mode enum ──────────────────────────────────────────────────────────────

#[derive(Clone)]
enum Mode {
    /// Blast packets in batches, receive in batches
    SendRecv { packet: Bytes, batch_size: usize },
    /// Receive only, don't send
    RecvOnly,
    /// Echo: receive packet, send it back immediately
    Echo,
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
    let mode = if args.echo {
        Mode::Echo
    } else if args.no_send {
        Mode::RecvOnly
    } else {
        Mode::SendRecv {
            packet: build_bench_packet(args.packet_size),
            batch_size: args.batch_size,
        }
    };

    let mode_name = match &mode {
        Mode::SendRecv { batch_size, .. } => format!("send+recv (batch={})", batch_size),
        Mode::RecvOnly => "recv-only".to_string(),
        Mode::Echo => "echo".to_string(),
    };

    let stats = Arc::new(Stats::new());
    let encrypted = args.encrypted;

    let listener = TcpListener::bind(&args.listen).await?;
    info!("bench-server listening on {}", args.listen);
    info!("mode: {mode_name}, packet_size={}, encrypted={encrypted}", args.packet_size);

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
            info!(
                "[stats] conns={conns} | recv: {:.0} pps ({}) | send: {:.0} pps ({})",
                rp as f64 / elapsed, format_rate(rb as f64 / elapsed),
                sp as f64 / elapsed, format_rate(sb as f64 / elapsed),
            );
        }
    });

    loop {
        let (stream, addr) = listener.accept().await?;
        let stats = stats.clone();
        let mode = mode.clone();

        tokio::spawn(async move {
            stats.connections.fetch_add(1, Ordering::Relaxed);
            info!("[{addr}] connected");

            let protocol = Protocol::game(SEED, AUTH_KEY, VERSION, encrypted);
            let (mut transport, _) = match TransportBuilder::server(stream, &protocol).build() {
                Ok(t) => t,
                Err(e) => { error!("[{addr}] transport build: {e}"); return; }
            };

            // Read seed
            match transport.recv().await {
                Ok(TransportEvent::Seed(s)) => debug!("[{addr}] seed ({} bytes)", s.len()),
                Ok(_) => { error!("[{addr}] expected seed"); return; }
                Err(e) => { error!("[{addr}] seed recv: {e}"); return; }
            }

            let result = match &mode {
                Mode::SendRecv { packet, batch_size } => {
                    loop_send_recv(&addr, &mut transport, &stats, packet, *batch_size).await
                }
                Mode::RecvOnly => loop_recv_only(&addr, &mut transport, &stats).await,
                Mode::Echo => loop_echo(&addr, &mut transport, &stats).await,
            };

            if let Err(e) = result {
                error!("[{addr}] error: {e}");
            }
            transport.close().await;
            stats.connections.fetch_sub(1, Ordering::Relaxed);
            info!("[{addr}] disconnected");
        });
    }
}


// ── Loops ──────────────────────────────────────────────────────────────────

async fn loop_send_recv(
    addr: &SocketAddr,
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
                Err(TransportError::Closed) => { debug!("[{addr}] disconnected"); return Ok(()); }
                Err(e) => anyhow::bail!("recv: {e}"),
            }
        }
    }
}

async fn loop_recv_only(
    addr: &SocketAddr,
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
            Err(TransportError::Closed) => { debug!("[{addr}] disconnected"); break; }
            Err(e) => { error!("[{addr}] error: {e}"); break; }
        }
    }
    Ok(())
}

async fn loop_echo(
    addr: &SocketAddr,
    transport: &mut Box<dyn PacketTransport>,
    stats: &Stats,
) -> anyhow::Result<()> {
    loop {
        match transport.recv().await {
            Ok(TransportEvent::Packet(data)) => {
                let len = data.len() as u64;
                stats.recv_packets.fetch_add(1, Ordering::Relaxed);
                stats.recv_bytes.fetch_add(len, Ordering::Relaxed);

                transport.send(TransportEvent::Packet(data)).await?;
                transport.flush().await?;
                stats.send_packets.fetch_add(1, Ordering::Relaxed);
                stats.send_bytes.fetch_add(len, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(TransportError::Closed) => { debug!("[{addr}] disconnected"); break; }
            Err(e) => { error!("[{addr}] error: {e}"); break; }
        }
    }
    Ok(())
}
