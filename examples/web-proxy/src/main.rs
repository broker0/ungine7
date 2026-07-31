//! CLI runner for the UO web-proxy.
//!
//! Parses command-line arguments, starts the shared web server, the
//! periodic sessions-broadcast tick, and optionally a UO proxy listener.
//!
//! Instance management is done via the REST API (`/api/instances/*`).
//! Instances are persisted in a JSON config file and auto-started on launch.
//!
//! On Ctrl-C all components are shut down gracefully.

use std::sync::Arc;

use clap::Parser;
use log::info;
use tokio::sync::watch;

use common::args::VerbosityArgs;
use web_proxy::instance_manager::{ConnectorConfigDto, InstanceConfig, ProxyInstanceManager};
use web_proxy::session_registry::{SessionRegistry, SharedRegistry};
use web_proxy::web::SharedManager;

// ── CLI ───────────────────────────────────────────────────────────────────

/// UO proxy with a web-based packet inspector and instance manager.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(flatten)]
    verbosity: VerbosityArgs,

    /// Port to serve the web inspector UI on.
    #[arg(long, default_value_t = 8080)]
    web_port: u16,

    /// Enable raw byte-level logging on both transport sides.
    #[arg(long)]
    raw_log: bool,

    /// Directory containing `index.html` and/or `manager.html` to serve from
    /// disk instead of the built-in copies.  Files are re-read on every request
    /// so changes take effect immediately — useful for frontend development.
    #[arg(long)]
    dev_html: Option<std::path::PathBuf>,

    /// Path to the JSON config file for proxy instance persistence.
    #[arg(long, default_value = "instances.json")]
    config: std::path::PathBuf,

    // ── Optional proxy instance from CLI (backward-compatible) ───────

    /// Real UO server address to connect to.
    /// When provided together with other proxy options, a proxy instance is
    /// created and started automatically.
    #[arg(long)]
    server: Option<String>,

    /// Client version to expect (format: major.minor.patch.build).
    #[arg(long = "client-version", default_value = "3.0.8.0")]
    client_version: String,

    /// Accept encrypted client connections.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set,
          num_args = 0..=1, default_missing_value = "true")]
    encrypted: bool,

    /// Port to listen for UO client connections.
    #[arg(long, default_value_t = 2593)]
    proxy_port: u16,

    /// Public IPv4 address of this proxy, written into the 0x8C redirect
    /// packet that is sent to the UO client.
    #[arg(long = "proxy-host", default_value = "127.0.0.1")]
    proxy_host: String,

    /// SOCKS5 proxy address to route server connections through.
    #[arg(long)]
    socks5: Option<String>,

    /// Username for SOCKS5 authentication (requires --socks5).
    #[arg(long, requires = "socks5")]
    socks5_user: Option<String>,

    /// Password for SOCKS5 authentication (requires --socks5-user).
    #[arg(long, requires = "socks5_user")]
    socks5_pass: Option<String>,

    /// Use a single-threaded tokio runtime instead of the default multi-threaded one.
    /// Reduces OS thread count and may lower idle resource usage.
    #[arg(long)]
    single_thread: bool,
}

impl Args {
    /// Build an `InstanceConfig` from CLI arguments, if `--server` was given.
    fn cli_instance_config(&self) -> Option<InstanceConfig> {
        let server = self.server.as_ref()?;
        Some(InstanceConfig {
            name: format!("CLI ({})", server),
            server: server.clone(),
            listen_addr: format!("127.0.0.1:{}", self.proxy_port),
            proxy_host: self.proxy_host.clone(),
            client_version: self.client_version.clone(),
            encrypted: self.encrypted,
            raw_log: self.raw_log,
            connector: ConnectorConfigDto {
                socks5_enabled: self.socks5.is_some(),
                socks5_addr: self.socks5.clone(),
                socks5_user: self.socks5_user.clone(),
                socks5_pass: self.socks5_pass.clone(),
            },
        })
    }
}

// ── main ──────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    args.verbosity.logger(["web_proxy"]).build()?;

    let runtime = if args.single_thread {
        info!("Using single-threaded tokio runtime");
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

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {

    info!("Web port: {}", args.web_port);

    // ── Shared state ─────────────────────────────────────────────────
    let registry: SharedRegistry = Arc::new(SessionRegistry::new());

    // ── Instance manager ─────────────────────────────────────────────
    let mut manager = ProxyInstanceManager::new(registry.clone(), &args.config);

    // Load persisted instances.
    if let Err(e) = manager.load() {
        log::error!("Failed to load config: {e}");
    }

    // Auto-start instances marked with auto_start=true.
    manager.auto_start_all().await;

    // If --server was given, create and start a CLI instance.
    if let Some(cli_config) = args.cli_instance_config() {
        info!("Creating proxy instance from CLI args: server={}", cli_config.server);
        match manager.add_from_cli(cli_config, true).await {
            Ok(id) => info!("CLI instance #{id} started"),
            Err(e) => log::error!("Failed to start CLI instance: {e}"),
        }
    }

    let manager: SharedManager = Arc::new(tokio::sync::Mutex::new(manager));

    // Shutdown signal: `false` → running, `true` → shutting down.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // ── Web UI server ────────────────────────────────────────────────
    let web_registry = registry.clone();
    let web_manager = manager.clone();
    let web_addr = format!("0.0.0.0:{}", args.web_port);
    let dev_html = args.dev_html.clone();
    let mut web_shutdown_rx = shutdown_rx.clone();
    let web_handle = tokio::spawn(async move {
        web_proxy::web::run_server(
            web_registry,
            web_manager,
            &web_addr,
            dev_html,
            async move { let _ = web_shutdown_rx.wait_for(|&v| v).await; },
        ).await;
    });

    // ── Periodic sessions-list broadcast ─────────────────────────────
    /*let tick_registry = registry.clone();
    let mut tick_shutdown_rx = shutdown_rx.clone();
    let tick_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    tick_registry.notify_sessions_pub();
                }
                _ = tick_shutdown_rx.wait_for(|&v| v) => break,
            }
        }
    });*/

    // ── Ctrl-C handler ───────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;
    info!("Ctrl-C received, shutting down…");

    // Signal web server and tick to stop.
    let _ = shutdown_tx.send(true);

    // Stop all proxy instances.
    {
        let mut mgr = manager.lock().await;
        mgr.shutdown_all().await;
    }

    // Wait for web server and tick to finish.
    let _ = tokio::join!(web_handle/*, tick_handle*/);

    info!("Shutdown complete");
    Ok(())
}
