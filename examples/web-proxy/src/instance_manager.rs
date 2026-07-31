//! Proxy instance manager — CRUD, lifecycle, and JSON persistence.
//!
//! Manages multiple UO proxy instances at runtime:
//! - Create / update / delete instance configurations
//! - Start / stop individual instances
//! - Persist configurations to a JSON file
//! - Auto-start instances marked with `auto_start: true` on load

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use network::listener::ListenerControl;
use protocol::connector::ConnectorConfig;
use u_core::ProtocolVersion;

use crate::session_registry::SharedRegistry;
use crate::ProxyConfig;

// ── Public types ──────────────────────────────────────────────────────────

pub type InstanceId = u64;

static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

fn next_instance_id() -> InstanceId {
    NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Serialisable instance configuration — all fields are simple types
/// suitable for JSON round-tripping and REST API exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// Human-readable name for the UI.
    pub name: String,
    /// Real UO server address (e.g. `"127.0.0.1:2593"`).
    pub server: String,
    /// Address to listen on (e.g. `"0.0.0.0:2593"`).
    pub listen_addr: String,
    /// Public proxy IPv4 host written into 0x8C redirects.
    #[serde(default = "default_proxy_host")]
    pub proxy_host: String,
    /// Expected client version (e.g. `"3.0.8.0"`).
    #[serde(default = "default_client_version")]
    pub client_version: String,
    /// Whether to accept encrypted client connections.
    #[serde(default = "default_true")]
    pub encrypted: bool,
    /// Enable raw byte-level transport logging.
    #[serde(default)]
    pub raw_log: bool,
    /// SOCKS5 proxy configuration.
    #[serde(default)]
    pub connector: ConnectorConfigDto,
}

fn default_proxy_host() -> String { "127.0.0.1".to_string() }
fn default_client_version() -> String { "3.0.8.0".to_string() }
fn default_true() -> bool { true }

/// Serialisable connector configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectorConfigDto {
    /// Whether SOCKS5 is active. When `false` the proxy connects directly even
    /// if `socks5_addr` / credentials are stored.
    #[serde(default)]
    pub socks5_enabled: bool,
    /// SOCKS5 proxy address (e.g. `"127.0.0.1:1080"`).
    pub socks5_addr: Option<String>,
    /// SOCKS5 username.
    pub socks5_user: Option<String>,
    /// SOCKS5 password.
    pub socks5_pass: Option<String>,
}

/// Instance runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    Stopped,
    Running,
    Error,
}

/// Full instance information returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct InstanceInfo {
    pub id: InstanceId,
    pub config: InstanceConfig,
    pub auto_start: bool,
    pub state: InstanceState,
    /// Error message if `state == Error`.
    pub error: Option<String>,
}

// ── Persistence model ─────────────────────────────────────────────────────

/// A single entry in the JSON config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceRecord {
    id: InstanceId,
    auto_start: bool,
    config: InstanceConfig,
}

/// Root of the JSON config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstancesFile {
    instances: Vec<InstanceRecord>,
}

// ── Internal runtime state ────────────────────────────────────────────────

/// Result value sent over the status watch channel.
///
/// - `None`       — instance is running (task still alive)
/// - `Some(Ok)`   — instance stopped cleanly (ForceShutdown / graceful)
/// - `Some(Err)`  — instance crashed with an error message
type TaskResult = Option<Result<(), String>>;

struct ManagedInstance {
    config: InstanceConfig,
    auto_start: bool,
    /// Tracks whether the instance was explicitly started and not yet stopped.
    running: bool,
    /// Sender to signal shutdown to the running proxy listener.
    control_tx: Option<mpsc::Sender<ListenerControl>>,
    /// Handle to the spawned tokio task.
    task_handle: Option<JoinHandle<()>>,
    /// Watch channel: `None` = running, `Some(Ok)` = stopped, `Some(Err)` = crashed.
    status_rx: Option<watch::Receiver<TaskResult>>,
}

impl ManagedInstance {
    fn new(config: InstanceConfig, auto_start: bool) -> Self {
        Self {
            config,
            auto_start,
            running: false,
            control_tx: None,
            task_handle: None,
            status_rx: None,
        }
    }

    /// Derive the current state and error by peeking at the watch channel.
    fn current_state(&self) -> (InstanceState, Option<String>) {
        if !self.running {
            return (InstanceState::Stopped, None);
        }
        match &self.status_rx {
            None => (InstanceState::Running, None),
            Some(rx) => match &*rx.borrow() {
                None => (InstanceState::Running, None),
                Some(Ok(())) => (InstanceState::Stopped, None),
                Some(Err(e)) => (InstanceState::Error, Some(e.clone())),
            },
        }
    }

    fn to_info(&self, id: InstanceId) -> InstanceInfo {
        let (state, error) = self.current_state();
        InstanceInfo {
            id,
            config: self.config.clone(),
            auto_start: self.auto_start,
            state,
            error,
        }
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────

impl InstanceConfig {
    /// Convert to the internal `ProxyConfig` used by `run_proxy`.
    pub fn to_proxy_config(&self) -> Result<ProxyConfig, String> {
        let proxy_host: Ipv4Addr = self.proxy_host.parse()
            .map_err(|_| format!("invalid proxy_host '{}': expected an IPv4 address", self.proxy_host))?;

        let listen_addr: SocketAddr = self.listen_addr.parse()
            .map_err(|_| format!("invalid listen_addr '{}': expected host:port", self.listen_addr))?;

        let proxy_port = match listen_addr {
            SocketAddr::V4(a) => a.port(),
            SocketAddr::V6(a) => a.port(),
        };
        let proxy_addr = SocketAddrV4::new(proxy_host, proxy_port);

        // Validate server address — must be parseable as host:port.
        parse_host_port(&self.server)
            .map_err(|e| format!("invalid server '{}': {e}", self.server))?;

        let client_version = ProtocolVersion::from_str(&self.client_version)
            .map_err(|e| format!("invalid client_version '{}': {e}", self.client_version))?;

        // Validate SOCKS5 address if enabled.
        if self.connector.socks5_enabled {
            if let Some(ref addr) = self.connector.socks5_addr {
                parse_host_port(addr)
                    .map_err(|e| format!("invalid socks5_addr '{}': {e}", addr))?;
            } else {
                return Err("socks5 is enabled but socks5_addr is not set".to_string());
            }
        }

        let connector = self.connector.to_connector_config();

        Ok(ProxyConfig {
            proxy_addr,
            server: self.server.clone(),
            listen_addr: self.listen_addr.clone(),
            raw_log: self.raw_log,
            connector,
            client_version,
            encrypted: self.encrypted,
        })
    }
}

/// Parse a `"host:port"` string — host may be a hostname or IP, port must be
/// a valid u16.  Does **not** perform DNS resolution.
fn parse_host_port(s: &str) -> Result<(), String> {
    // rsplit on ':' to handle IPv6 addresses like "[::1]:2593"
    let (host, port_str) = s.rsplit_once(':')
        .ok_or_else(|| "missing port (expected host:port)".to_string())?;
    let port: u16 = port_str.parse()
        .map_err(|_| format!("invalid port '{}': must be 1–65535", port_str))?;
    if port == 0 {
        return Err("port 0 is not allowed".to_string());
    }
    if host.is_empty() {
        return Err("host part is empty".to_string());
    }
    Ok(())
}

impl ConnectorConfigDto {
    fn to_connector_config(&self) -> ConnectorConfig {
        if !self.socks5_enabled {
            return ConnectorConfig::Direct;
        }
        match &self.socks5_addr {
            Some(proxy_addr) => {
                let auth = match (&self.socks5_user, &self.socks5_pass) {
                    (Some(user), Some(pass)) => Some((user.clone(), pass.clone())),
                    _ => None,
                };
                ConnectorConfig::Socks5 { proxy_addr: proxy_addr.clone(), auth }
            }
            None => ConnectorConfig::Direct,
        }
    }
}

// ── ProxyInstanceManager ──────────────────────────────────────────────────

/// Manages the lifecycle of multiple proxy instances.
///
/// All mutating operations automatically persist the configuration to disk.
/// This struct is **not** `Clone` — wrap it in `Arc<tokio::sync::Mutex<_>>`
/// for shared access from Axum handlers.
pub struct ProxyInstanceManager {
    instances: HashMap<InstanceId, ManagedInstance>,
    registry: SharedRegistry,
    config_path: PathBuf,
}

impl ProxyInstanceManager {
    /// Create a new empty manager.
    pub fn new(registry: SharedRegistry, config_path: impl Into<PathBuf>) -> Self {
        Self {
            instances: HashMap::new(),
            registry,
            config_path: config_path.into(),
        }
    }

    /// Load instance configurations from the JSON file.
    ///
    /// Does **not** start any instances — call [`Self::auto_start_all`] afterwards.
    pub fn load(&mut self) -> Result<(), String> {
        let path = &self.config_path;
        if !path.exists() {
            info!("No config file at {}, starting with empty instance list", path.display());
            return Ok(());
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let file: InstancesFile = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

        let mut max_id: u64 = 0;
        for record in file.instances {
            max_id = max_id.max(record.id);
            self.instances.insert(record.id, ManagedInstance::new(record.config, record.auto_start));
        }

        // Ensure the ID counter is past any loaded IDs.
        let current = NEXT_INSTANCE_ID.load(Ordering::Relaxed);
        if max_id >= current {
            NEXT_INSTANCE_ID.store(max_id + 1, Ordering::Relaxed);
        }

        info!("Loaded {} instance(s) from {}", self.instances.len(), path.display());
        Ok(())
    }

    /// Start all instances that have `auto_start == true`.
    pub async fn auto_start_all(&mut self) {
        let ids: Vec<InstanceId> = self.instances.iter()
            .filter(|(_, inst)| inst.auto_start && !inst.running)
            .map(|(id, _)| *id)
            .collect();

        for id in ids {
            info!("Auto-starting instance #{id}");
            if let Err(e) = self.start(id).await {
                warn!("Failed to auto-start instance #{id}: {e}");
            }
        }
    }

    /// Save the current instance configurations to the JSON file.
    fn save(&self) -> Result<(), String> {
        let records: Vec<InstanceRecord> = self.instances.iter()
            .map(|(id, inst)| InstanceRecord {
                id: *id,
                auto_start: inst.auto_start,
                config: inst.config.clone(),
            })
            .collect();

        let file = InstancesFile { instances: records };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("failed to serialize config: {e}"))?;

        std::fs::write(&self.config_path, json)
            .map_err(|e| format!("failed to write {}: {e}", self.config_path.display()))?;

        Ok(())
    }

    // ── CRUD ──────────────────────────────────────────────────────────

    /// Add a new instance (initially stopped). Returns the assigned ID.
    pub fn add(&mut self, config: InstanceConfig, auto_start: bool) -> Result<InstanceId, String> {
        // Validate the config can be converted.
        config.to_proxy_config()?;

        let id = next_instance_id();
        self.instances.insert(id, ManagedInstance::new(config, auto_start));
        self.save()?;
        info!("Added instance #{id}");
        Ok(id)
    }

    /// Remove an instance. Stops it first if running.
    pub async fn remove(&mut self, id: InstanceId) -> Result<(), String> {
        if let Some(inst) = self.instances.get(&id) {
            if inst.running {
                self.stop(id).await?;
            }
        }
        self.instances.remove(&id)
            .ok_or_else(|| format!("instance #{id} not found"))?;
        self.save()?;
        info!("Removed instance #{id}");
        Ok(())
    }

    /// Update the configuration (and auto_start flag) of an instance.
    ///
    /// If the instance is currently running it is stopped first, the config is
    /// applied, and then it is started again.  Returns `true` if a restart was
    /// performed, `false` if the instance was already stopped.
    pub async fn update(
        &mut self,
        id: InstanceId,
        config: InstanceConfig,
        auto_start: bool,
    ) -> Result<bool, String> {
        // Validate before touching anything.
        config.to_proxy_config()?;

        let was_running = self.instances.get(&id)
            .ok_or_else(|| format!("instance #{id} not found"))?
            .running;

        if was_running {
            self.stop(id).await?;
        }

        let inst = self.instances.get_mut(&id)
            .ok_or_else(|| format!("instance #{id} not found"))?;
        inst.config = config;
        inst.auto_start = auto_start;
        inst.running = false;
        inst.status_rx = None;
        self.save()?;
        info!("Updated instance #{id}");

        if was_running {
            self.start(id).await?;
            info!("Restarted instance #{id} after config update");
        }

        Ok(was_running)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    /// Start a stopped instance.
    pub async fn start(&mut self, id: InstanceId) -> Result<(), String> {
        let inst = self.instances.get_mut(&id)
            .ok_or_else(|| format!("instance #{id} not found"))?;

        let (state, _) = inst.current_state();
        if state == InstanceState::Running {
            return Err(format!("instance #{id} is already running"));
        }

        let proxy_config = inst.config.to_proxy_config()?;
        let (control_tx, control_rx) = mpsc::channel::<ListenerControl>(1);
        let (status_tx, status_rx) = watch::channel::<TaskResult>(None);
        let registry = self.registry.clone();

        let task_handle = tokio::spawn(async move {
            let result = crate::run_proxy(proxy_config, registry, control_rx).await;
            match &result {
                Ok(()) => { let _ = status_tx.send(Some(Ok(()))); }
                Err(e) => {
                    let msg = e.to_string();
                    error!("Proxy instance error: {msg}");
                    let _ = status_tx.send(Some(Err(msg)));
                }
            }
        });

        inst.control_tx = Some(control_tx);
        inst.task_handle = Some(task_handle);
        inst.status_rx = Some(status_rx);
        inst.running = true;
        info!(
            "Started instance #{id} \
             name={:?} listen={} server={} proxy_host={} client_version={} encrypted={} raw_log={} socks5_enabled={} socks5={:?}",
            inst.config.name,
            inst.config.listen_addr,
            inst.config.server,
            inst.config.proxy_host,
            inst.config.client_version,
            inst.config.encrypted,
            inst.config.raw_log,
            inst.config.connector.socks5_enabled,
            inst.config.connector.socks5_addr,
        );
        Ok(())
    }

    /// Stop a running instance.
    ///
    /// Sends [`ListenerControl::StopListening`] — the TCP accept loop closes
    /// immediately (no new connections are accepted) but any active relay tasks
    /// keep running on the Tokio runtime until the client or server disconnects.
    /// Sessions therefore remain visible in the inspector and disappear naturally
    /// via `on_disconnect` / the 10-second cleanup timer.
    pub async fn stop(&mut self, id: InstanceId) -> Result<(), String> {
        let inst = self.instances.get_mut(&id)
            .ok_or_else(|| format!("instance #{id} not found"))?;

        if !inst.running {
            return Err(format!("instance #{id} is not running"));
        }

        // Stop accepting new connections; active relays continue independently.
        if let Some(tx) = inst.control_tx.take() {
            let _ = tx.send(ListenerControl::StopListening).await;
        }

        // The listener task returns quickly once it stops accepting — await it.
        if let Some(handle) = inst.task_handle.take() {
            let _ = handle.await;
        }

        inst.running = false;
        inst.status_rx = None;
        info!("Stopped instance #{id} (active relays continue until naturally closed)");
        Ok(())
    }

    // ── Queries ───────────────────────────────────────────────────────

    /// List all instances.
    pub fn list(&self) -> Vec<InstanceInfo> {
        let mut result: Vec<InstanceInfo> = self.instances.iter()
            .map(|(id, inst)| inst.to_info(*id))
            .collect();
        result.sort_by_key(|i| i.id);
        result
    }

    /// Get a single instance by ID.
    pub fn get(&self, id: InstanceId) -> Option<InstanceInfo> {
        self.instances.get(&id).map(|inst| inst.to_info(id))
    }

    // ── Shutdown ──────────────────────────────────────────────────────

    /// Stop all running instances gracefully. Called during application shutdown.
    pub async fn shutdown_all(&mut self) {
        let running_ids: Vec<InstanceId> = self.instances.iter()
            .filter(|(_, inst)| inst.running)
            .map(|(id, _)| *id)
            .collect();

        for id in running_ids {
            info!("Shutting down instance #{id}");
            if let Err(e) = self.stop(id).await {
                warn!("Error stopping instance #{id}: {e}");
            }
        }

        if let Err(e) = self.save() {
            warn!("Error saving config on shutdown: {e}");
        }
    }

    /// Create an instance from CLI arguments and optionally start it.
    ///
    /// Used when `--server` / `--proxy-port` etc. are passed on the command line.
    pub async fn add_from_cli(
        &mut self,
        config: InstanceConfig,
        start: bool,
    ) -> Result<InstanceId, String> {
        let id = self.add(config, false)?;
        if start {
            self.start(id).await?;
        }
        Ok(id)
    }
}
