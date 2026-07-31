use log::{debug, trace, warn};
use std::any::Any;
use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use u_core::ProtocolVersion;
use crate::logs;

/// Everything we know after the login phase completes.
/// Stored when 0x8C is intercepted/generated, consumed when 0x91 arrives.
#[derive(Debug)]
pub struct PendingConnection {
    /// The auth key that will appear in the game login packet (0x91)
    pub auth_key: u32,

    /// Client version (determines packet table & encryption)
    pub client_version: ProtocolVersion,

    /// Where the real game server is (for proxy mode).
    /// `None` if we ARE the server.
    pub game_server_address: Option<SocketAddrV4>,

    pub encrypted: bool,
    pub seed_size: usize, // 4 or MAX_SEED_SIZE (for 0xEF extended seed)

    /// When this entry was created (for expiration)
    pub created_at: Instant,

    /// Arbitrary application data attached to this pending connection.
    /// The framework doesn't interpret this — it's for the application layer.
    pub context: Box<dyn Any + Send + Sync>,
}

/// Result of binding a game connection to a pending login.
#[derive(Debug)]
pub struct BoundConnection {
    pub auth_key: u32,
    pub client_version: ProtocolVersion,
    pub game_server_address: Option<SocketAddrV4>,
    pub encrypted: bool,
    pub seed_size: usize,
    pub context: Box<dyn Any + Send + Sync>,
}

/// Configuration for the binder.
#[derive(Debug, Clone)]
pub struct BinderConfig {
    /// How long a pending connection lives before being garbage-collected.
    /// Default: 30 seconds. UO clients typically reconnect within a few seconds.
    pub ttl: Duration,

    /// Maximum number of pending connections.
    /// Prevents memory exhaustion from malicious/broken clients.
    pub max_pending: usize,
}

impl Default for BinderConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(30),
            max_pending: 1024,
        }
    }
}

/// Universal binder that correlates login-phase and game-phase connections
/// using the auth key from packet 0x8C / 0x91.
///
/// This is a framework-level component, not application-specific.
/// It doesn't know about sessions, proxies, or game logic —
/// it only maps `auth_key → PendingConnection`.
///
/// Thread-safe: uses interior mutability via `Mutex`.
/// Cheaply cloneable: wraps an `Arc`.
///
/// # Usage
///
/// ```rust,ignore
/// let binder = ConnectionBinder::new(BinderConfig::default());
///
/// // After intercepting/generating 0x8C during login phase:
/// binder.register(PendingConnection {
///     auth_key: 0xDEADBEEF,
///     client_version: version,
///     game_server_address: Some(real_server_addr),
///     created_at: Instant::now(),
///     context: Box::new(my_session_id),
/// });
///
/// // When a game connection arrives with 0x91:
/// if let Some(bound) = binder.bind(0xDEADBEEF) {
///     // bound.context can be downcast to recover my_session_id
///     let session_id = bound.context.downcast::<SessionId>().unwrap();
/// }
/// ```
#[derive(Clone)]
pub struct ConnectionBinder {
    config: BinderConfig,
    inner: Arc<Mutex<BinderInner>>,
}

struct BinderInner {
    pending: HashMap<u32, PendingConnection>,
}

impl ConnectionBinder {
    pub fn new(config: BinderConfig) -> Self {
        Self {
            config,
            inner: Arc::new(Mutex::new(BinderInner {
                pending: HashMap::new(),
            })),
        }
    }

    /// Register a pending game connection after login phase completes.
    ///
    /// If a connection with the same `auth_key` already exists, it is replaced.
    /// Returns `Err` if the maximum number of pending connections is reached.
    pub fn register(&self, pending: PendingConnection) -> Result<(), BinderError> {
        let mut inner = self.inner.lock().unwrap();

        // GC only when we're at capacity — not on every call
        if inner.pending.len() >= self.config.max_pending {
            self.gc_locked(&mut inner);

            // Still at capacity after GC?
            if inner.pending.len() >= self.config.max_pending {
                warn!(
                    target: logs::BINDER,
                    "binder: max pending ({}) reached, rejecting key=0x{:08X}",
                    self.config.max_pending, pending.auth_key
                );
                return Err(BinderError::CapacityExceeded);
            }
        }

        debug!(target: logs::BINDER, "binder: registered key=0x{:08X}", pending.auth_key);
        inner.pending.insert(pending.auth_key, pending);
        Ok(())
    }

    /// Try to bind a game connection using the auth key from packet 0x91.
    ///
    /// On success, removes and returns the pending connection.
    /// The key is consumed — subsequent calls with the same key return `None`.
    pub fn bind(&self, auth_key: u32) -> Option<BoundConnection> {
        let mut inner = self.inner.lock().unwrap();

        let pending = inner.pending.remove(&auth_key)?;

        // Check expiration
        if pending.created_at.elapsed() > self.config.ttl {
            debug!(target: logs::BINDER, "binder: key=0x{auth_key:08X} expired, discarding");
            return None;
        }

        debug!(target: logs::BINDER, "binder: bound key=0x{auth_key:08X}");

        Some(BoundConnection {
            auth_key: pending.auth_key,
            client_version: pending.client_version,
            game_server_address: pending.game_server_address,
            encrypted: pending.encrypted,
            seed_size: pending.seed_size,
            context: pending.context,
        })
    }

    /// Check if a key is registered (without consuming it).
    pub fn contains(&self, auth_key: u32) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.pending.contains_key(&auth_key)
    }

    /// Peek at the client version for a pending connection without consuming it.
    ///
    /// Returns `None` if the key is not registered or has expired.
    pub fn peek_version(&self, auth_key: u32) -> Option<ProtocolVersion> {
        let inner = self.inner.lock().unwrap();
        let pending = inner.pending.get(&auth_key)?;
        if pending.created_at.elapsed() > self.config.ttl {
            return None;
        }
        Some(pending.client_version)
    }

    /// Number of currently pending connections.
    pub fn pending_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.pending.len()
    }

    /// Manually remove a pending connection (e.g. on timeout from app layer).
    pub fn remove(&self, auth_key: u32) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.pending.remove(&auth_key).is_some()
    }

    /// Remove all expired entries.
    pub fn gc(&self) {
        let mut inner = self.inner.lock().unwrap();
        self.gc_locked(&mut inner);
    }

    fn gc_locked(&self, inner: &mut BinderInner) {
        let ttl = self.config.ttl;
        let before = inner.pending.len();

        inner.pending.retain(|key, pending| {
            let alive = pending.created_at.elapsed() <= ttl;
            if !alive {
                trace!(target: logs::BINDER, "binder: gc expired key=0x{key:08X}");
            }
            alive
        });

        let removed = before - inner.pending.len();
        if removed > 0 {
            debug!(target: logs::BINDER, "binder: gc removed {removed} expired entries");
        }
    }
}

// Debug without exposing internals
impl std::fmt::Debug for ConnectionBinder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionBinder")
            .field("config", &self.config)
            .field("pending_count", &self.pending_count())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinderError {
    /// Maximum number of pending connections reached.
    CapacityExceeded,
}

impl std::fmt::Display for BinderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityExceeded => write!(f, "connection binder capacity exceeded"),
        }
    }
}

impl std::error::Error for BinderError {}
