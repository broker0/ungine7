//! Session registry — tracks active proxy sessions and their log history.
//!
//! Each session gets a unique `SessionId` (atomic u64 counter), a ring buffer
//! of the last `HISTORY_CAPACITY` log entries, and a `broadcast` channel so
//! that WebSocket subscribers receive live entries without buffering issues.
//!
//! Log entries are represented by [`LogEntry`], an enum that holds either a
//! decoded [`PacketEntry`] or a raw byte-level [`RawEntry`].  When `--raw-log`
//! is active, both variants appear in the same chronological stream; otherwise
//! only `PacketEntry` values are recorded.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;
use serde::Serialize;

/// Maximum number of log entries kept per session (ring buffer capacity).
pub const HISTORY_CAPACITY: usize = 4000;

/// Broadcast channel capacity (number of in-flight entries before oldest is dropped).
const BROADCAST_CAPACITY: usize = 1024;

/// Capacity for the sessions-list broadcast channel.
const SESSIONS_BROADCAST_CAPACITY: usize = 16;

// ── SessionId ──────────────────────────────────────────────────────────────

pub type SessionId = u64;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_session_id() -> SessionId {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── PacketEntry ────────────────────────────────────────────────────────────

/// A single observed packet, serialisable to JSON.
#[derive(Debug, Clone, Serialize)]
pub struct PacketEntry {
    /// Unix timestamp in milliseconds.
    pub timestamp: u64,
    /// "C→S" or "S→C"
    pub direction: String,
    /// Packet id as hex string, e.g. "0xBF"
    pub id: String,
    /// Raw packet byte length.
    pub len: usize,
    /// Decoded description or empty string for unknown packets.
    pub desc: String,
    /// Raw bytes as hex string, e.g. "3A 00 1F ...".
    pub hex: String,
}

// ── RawStage ──────────────────────────────────────────────────────────────

/// Stage of the transport pipeline where bytes were observed.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RawStage {
    /// Encrypted bytes just read from the socket.
    RawRead,
    /// Bytes after decryption, before packet framing.
    Decrypted,
    /// Plaintext bytes about to be encrypted (outgoing packet).
    PreEncrypt,
    /// Encrypted bytes about to be written to the socket.
    RawWrite,
}

// ── RawEntry ──────────────────────────────────────────────────────────────

/// A single raw byte observation, serialisable to JSON.
#[derive(Debug, Clone, Serialize)]
pub struct RawEntry {
    /// Unix timestamp in milliseconds.
    pub timestamp: u64,
    /// Pipeline stage.
    pub stage: RawStage,
    /// Traffic direction: "C→S" or "S→C".
    pub direction: String,
    /// Number of bytes.
    pub len: usize,
    /// Raw bytes as hex string, e.g. "3A 00 1F ...".
    pub hex: String,
}

impl RawEntry {
    /// Create a new entry from raw bytes.
    ///
    /// `direction` should be `"C→S"` or `"S→C"`.
    pub fn new(stage: RawStage, direction: &str, data: &[u8]) -> Self {
        let hex = data.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            timestamp,
            stage,
            direction: direction.to_string(),
            len: data.len(),
            hex,
        }
    }
}

// ── SocketRole ────────────────────────────────────────────────────────────

/// Which side of the proxy this socket represents.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketRole {
    /// Connection between the UO client and this proxy.
    Client,
    /// Connection between this proxy and the real UO server.
    Server,
}

// ── ConnectionEvent ───────────────────────────────────────────────────────

/// What happened to the socket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionEvent {
    /// TCP connection established.
    Connected,
    /// Clean shutdown (EOF / graceful close).
    Disconnected,
    /// Connection closed due to an error.
    Error {
        /// Human-readable error description.
        message: String,
    },
}

// ── ConnectionEntry ───────────────────────────────────────────────────────

/// A single socket lifecycle event, serialisable to JSON.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionEntry {
    /// Unix timestamp in milliseconds.
    pub timestamp: u64,
    /// Which socket this event belongs to.
    pub role: SocketRole,
    /// Remote address of the peer.
    pub addr: String,
    /// What happened.
    #[serde(flatten)]
    pub event: ConnectionEvent,
}

impl ConnectionEntry {
    pub fn new(role: SocketRole, addr: impl Into<String>, event: ConnectionEvent) -> Self {
        Self {
            timestamp: now_ms(),
            role,
            addr: addr.into(),
            event,
        }
    }
}

// ── LogEntry ──────────────────────────────────────────────────────────────

/// A single log entry — either a decoded packet, a raw byte observation, or
/// a socket lifecycle event.
///
/// Serialised with `#[serde(tag = "type")]` so JSON carries a `"type"` field:
/// - `{"type": "packet", "timestamp": ..., "direction": ..., ...}`
/// - `{"type": "raw", "timestamp": ..., "stage": ..., ...}`
/// - `{"type": "connection", "timestamp": ..., "role": ..., "kind": ..., ...}`
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogEntry {
    Packet(PacketEntry),
    Raw(RawEntry),
    Connection(ConnectionEntry),
}

// ── SessionInfo ────────────────────────────────────────────────────────────

/// Summary info returned by `GET /api/sessions`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub addr: String,
    pub started_at: u64,
    pub packet_count: usize,
    pub phase: String,
    /// Account name, populated once the 0x80 / 0x91 login packet is seen.
    pub account: Option<String>,
    /// Character name, populated once the 0x5D LoginCharacter packet is seen.
    pub character: Option<String>,
}

// ── Internal state ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct SessionState {
    addr: String,
    started_at: u64,
    phase: String,
    account: Option<String>,
    character: Option<String>,
    /// Whether raw byte-level logging is enabled for this session.
    raw_log: bool,
    /// Unified ring buffer: packets and raw entries interleaved chronologically.
    history: VecDeque<LogEntry>,
    /// Count of `LogEntry::Packet` entries pushed (for sidebar display).
    packet_count: usize,
    tx: broadcast::Sender<LogEntry>,
}

// ── SessionRegistry ────────────────────────────────────────────────────────

/// Thread-safe registry of all active proxy sessions.
///
/// Shared between the UO listener tasks (write side) and the Axum web server
/// (read side) via `Arc<SessionRegistry>`.
#[derive(Debug)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<SessionId, SessionState>>,
    sessions_tx: broadcast::Sender<Vec<SessionInfo>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        let (sessions_tx, _) = broadcast::channel(SESSIONS_BROADCAST_CAPACITY);
        Self {
            inner: Mutex::new(HashMap::new()),
            sessions_tx,
        }
    }

    /// Register a new session and return its assigned `SessionId`.
    pub fn register(
        &self,
        addr: SocketAddr,
        phase: impl Into<String>,
        raw_log: bool,
    ) -> SessionId {
        let id = next_session_id();
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

        let state = SessionState {
            addr: addr.to_string(),
            started_at: now_ms(),
            phase: phase.into(),
            account: None,
            character: None,
            raw_log,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            packet_count: 0,
            tx,
        };
        self.inner.lock().unwrap().insert(id, state);
        self.notify_sessions();
        id
    }

    /// Whether raw logging is enabled for the given session.
    pub fn session_raw_log(&self, id: SessionId) -> bool {
        self.inner.lock().unwrap()
            .get(&id)
            .map(|s| s.raw_log)
            .unwrap_or(false)
    }

    /// Push a packet entry into the session's ring buffer and broadcast it.
    pub fn push_packet(&self, id: SessionId, entry: PacketEntry) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&id) {
            if state.history.len() >= HISTORY_CAPACITY {
                // If the evicted entry was a Packet, decrement the counter.
                if matches!(state.history.front(), Some(LogEntry::Packet(_))) {
                    state.packet_count = state.packet_count.saturating_sub(1);
                }
                state.history.pop_front();
            }
            state.packet_count += 1;
            let log_entry = LogEntry::Packet(entry);
            state.history.push_back(log_entry.clone());
            let _ = state.tx.send(log_entry);
        }
    }

    /// Push a connection lifecycle event into the session's ring buffer and broadcast it.
    pub fn push_connection(&self, id: SessionId, entry: ConnectionEntry) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&id) {
            if state.history.len() >= HISTORY_CAPACITY {
                if matches!(state.history.front(), Some(LogEntry::Packet(_))) {
                    state.packet_count = state.packet_count.saturating_sub(1);
                }
                state.history.pop_front();
            }
            let log_entry = LogEntry::Connection(entry);
            state.history.push_back(log_entry.clone());
            let _ = state.tx.send(log_entry);
        }
    }

    /// Push a raw byte entry into the session's ring buffer and broadcast it.
    ///
    /// No-op if raw logging is disabled for this session or the session does not exist.
    pub fn push_raw(&self, id: SessionId, entry: RawEntry) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&id) {
            if !state.raw_log { return; }
            if state.history.len() >= HISTORY_CAPACITY {
                if matches!(state.history.front(), Some(LogEntry::Packet(_))) {
                    state.packet_count = state.packet_count.saturating_sub(1);
                }
                state.history.pop_front();
            }
            let log_entry = LogEntry::Raw(entry);
            state.history.push_back(log_entry.clone());
            let _ = state.tx.send(log_entry);
        }
    }

    /// Update the phase label of a session (e.g. "Login" → "Game").
    pub fn set_phase(&self, id: SessionId, phase: impl Into<String>) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&id) {
            state.phase = phase.into();
        }
        drop(guard);
        self.notify_sessions();
    }

    /// Set the account name for a session once the login packet is observed.
    ///
    /// Only updates if the account is not already set, so the first seen value
    /// (from 0x80 `AccountLogin`) wins over the later 0x91 `GameLogin`.
    pub fn set_account(&self, id: SessionId, account: impl Into<String>) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&id) {
            if state.account.is_none() {
                state.account = Some(account.into());
            }
        }
        drop(guard);
        self.notify_sessions();
    }

    /// Set the character name for a session once `0x5D LoginCharacter` is seen.
    ///
    /// Always overwrites — the player may log out and pick a different character
    /// within the same game TCP connection.
    pub fn set_character(&self, id: SessionId, character: impl Into<String>) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&id) {
            state.character = Some(character.into());
        }
        drop(guard);
        self.notify_sessions();
    }

    /// Remove a session from the registry (called on disconnect).
    pub fn unregister(&self, id: SessionId) {
        self.inner.lock().unwrap().remove(&id);
        self.notify_sessions();
    }

    /// Remove all sessions from the registry at once.
    ///
    /// Called when a proxy instance is force-stopped: because `abort_all()`
    /// kills tokio tasks without running their cleanup code, `on_disconnect`
    /// is never invoked for active connections, so sessions would otherwise
    /// remain visible in the inspector indefinitely.
    pub fn unregister_all(&self) {
        self.inner.lock().unwrap().clear();
        self.notify_sessions();
    }

    /// Return summary info for all active sessions (used by REST API).
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let guard = self.inner.lock().unwrap();
        guard
            .iter()
            .map(|(id, s)| SessionInfo {
                id: *id,
                addr: s.addr.clone(),
                started_at: s.started_at,
                packet_count: s.packet_count,
                phase: s.phase.clone(),
                account: s.account.clone(),
                character: s.character.clone(),
            })
            .collect()
    }

    /// Return the full log history snapshot and a live broadcast receiver for a
    /// specific session. Returns `None` if the session does not exist.
    pub fn subscribe(
        &self,
        id: SessionId,
    ) -> Option<(Vec<LogEntry>, broadcast::Receiver<LogEntry>)> {
        let guard = self.inner.lock().unwrap();
        guard.get(&id).map(|s| {
            let history: Vec<LogEntry> = s.history.iter().cloned().collect();
            let rx = s.tx.subscribe();
            (history, rx)
        })
    }

    /// Return the current sessions list and a receiver that fires whenever
    /// the list changes (session added, removed, or phase updated).
    pub fn subscribe_sessions(
        &self,
    ) -> (Vec<SessionInfo>, broadcast::Receiver<Vec<SessionInfo>>) {
        let rx = self.sessions_tx.subscribe();
        let list = self.list_sessions();
        (list, rx)
    }

    /// Broadcast the current sessions list to all `/ws/sessions` subscribers.
    fn notify_sessions(&self) {
        let list = self.list_sessions();
        let _ = self.sessions_tx.send(list);
    }

    /// Public variant used by the periodic tick task in `main`.
    pub fn notify_sessions_pub(&self) {
        self.notify_sessions();
    }
}

/// Convenience alias so callers can hold a cloneable handle.
pub type SharedRegistry = Arc<SessionRegistry>;
