use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{RwLock, broadcast, mpsc};

use u_core::ProtocolVersion;
use framework::ecumene::StaticDataProvider;

use crate::session::commands::ClientCommand;
use framework::diorama::ObserverEvent;
use crate::types::{ClientRole, PacketFrame, SessionId};

#[cfg(feature = "lua")]
use crate::lua_script::LuaCommand;

// ── SessionEntry ──────────────────────────────────────────────────────────

/// Shared state for one logical UO session (one Source + N Mirrors).
pub struct SessionEntry {
    pub id: SessionId,

    /// Single channel for sending commands to the HeadlessClient loop.
    /// Used by VirtualClients, WS handlers, and bot logic.
    pub command_tx: mpsc::Sender<ClientCommand>,

    /// Broadcast channel for raw packet frames (used by Mirrors and WS
    /// observers to receive a copy of every packet passing through the Source).
    pub packet_tx: broadcast::Sender<PacketFrame>,

    /// Typed proxy event broadcast (S→C packets → [`ObserverEvent`]).
    /// Consumed by Lua scripts and (in the future) high-level WS observers.
    pub event_tx: broadcast::Sender<ObserverEvent>,

    /// Channel for controlling the Lua script manager.
    #[cfg(feature = "lua")]
    pub lua_cmd_tx: mpsc::Sender<LuaCommand>,
    /// Receiver end — taken once when spawning the Lua manager task.
    #[cfg(feature = "lua")]
    pub lua_cmd_rx: tokio::sync::Mutex<Option<mpsc::Receiver<LuaCommand>>>,

    /// Static world data for Z resolution (terrain + statics).
    /// `None` when started without `--data-dir`.
    pub static_data: Option<Arc<dyn StaticDataProvider>>,

    /// Client protocol version — determines which packet formats to use
    /// (e.g. `ObjectInfo` 0x1A for pre-SA vs `ObjectInfoSA` 0xF3 for SA+).
    pub client_version: ProtocolVersion,

    /// The upstream game-server address the Source is connected to.
    /// Set when the Source enters game phase; used by Mirrors to connect
    /// to the same upstream.
    pub game_server_address: RwLock<Option<std::net::SocketAddrV4>>,

    /// Character name chosen during login (extracted from 0x5D LoginCharacter).
    /// Used by JoinExisting clients to display the real name in CharacterList.
    pub character_name: Arc<RwLock<Option<String>>>,

    /// Path to the UO client data directory (for loading diff files).
    /// `None` when started without `--data-dir`.
    pub data_dir: Option<PathBuf>,

    /// WebSocket URL of a mirror endpoint (e.g. path-server) to stream
    /// S2C packets to.  `None` when `--mirror-url` is not set.
    pub mirror_url: Option<String>,

    pub is_active: bool,
    pub created_at: Instant,
}

impl SessionEntry {
    fn new(
        id: SessionId,
        command_tx: mpsc::Sender<ClientCommand>,
        static_data: Option<Arc<dyn StaticDataProvider>>,
        client_version: ProtocolVersion,
        data_dir: Option<PathBuf>,
        mirror_url: Option<String>,
    ) -> Self {
        let (packet_tx, _) = broadcast::channel(256);
        let (event_tx, _) = broadcast::channel(256);
        #[cfg(feature = "lua")]
        let (lua_cmd_tx, lua_cmd_rx) = mpsc::channel(16);

        Self {
            id,
            command_tx,
            packet_tx,
            event_tx,
            #[cfg(feature = "lua")]
            lua_cmd_tx,
            #[cfg(feature = "lua")]
            lua_cmd_rx: tokio::sync::Mutex::new(Some(lua_cmd_rx)),
            static_data,
            client_version,
            game_server_address: RwLock::new(None),
            character_name: Arc::new(RwLock::new(None)),
            data_dir,
            mirror_url,
            is_active: true,
            created_at: Instant::now(),
        }
    }
}

// ── SessionRegistry ───────────────────────────────────────────────────────

pub type SharedSessionRegistry = Arc<RwLock<SessionRegistry>>;

pub struct SessionRegistry {
    sessions: HashMap<SessionId, Arc<SessionEntry>>,
    next_id: u64,
    static_data: Option<Arc<dyn StaticDataProvider>>,
    client_version: ProtocolVersion,
    data_dir: Option<PathBuf>,
    mirror_url: Option<String>,
}

impl SessionRegistry {
    pub fn new(
        static_data: Option<Arc<dyn StaticDataProvider>>,
        client_version: ProtocolVersion,
        data_dir: Option<PathBuf>,
        mirror_url: Option<String>,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            next_id: 1,
            static_data,
            client_version,
            data_dir,
            mirror_url,
        }
    }

    pub fn shared(self) -> SharedSessionRegistry {
        Arc::new(RwLock::new(self))
    }

    // ── Registration ──────────────────────────────────────────────────

    /// Called when a new client connects after login.
    ///
    /// - **Source** → always creates a fresh session and returns it.
    /// - **Mirror** → joins the most recently created active session.
    ///   If none exists, falls back to creating a new Source-like entry
    ///   (edge case: mirror arrived before any source).
    ///
    /// Returns `(session_id, entry, command_rx)`.
    /// `command_rx` is `Some` only for newly created sessions (Source);
    /// Mirror clients use `entry.command_tx` to send commands.
    pub fn register_or_join(
        &mut self,
        addr: SocketAddr,
        role: ClientRole,
    ) -> (SessionId, Arc<SessionEntry>, Option<mpsc::Receiver<ClientCommand>>) {
        match role {
            ClientRole::Source => {
                let (id, entry, cmd_rx) = self.create_session(addr);
                (id, entry, Some(cmd_rx))
            }
            ClientRole::Mirror => self
                .find_active_session()
                .map(|entry| (entry.id, entry, None))
                .unwrap_or_else(|| {
                    let (id, entry, cmd_rx) = self.create_session(addr);
                    (id, entry, Some(cmd_rx))
                }),
        }
    }

    /// Look up a session by id.
    pub fn get(&self, id: SessionId) -> Option<Arc<SessionEntry>> {
        self.sessions.get(&id).cloned()
    }

    /// Mark a session inactive (called when Source disconnects).
    pub fn deactivate(&mut self, id: SessionId) {
        if let Some(entry) = self.sessions.get_mut(&id) {
            Arc::get_mut(entry).map(|e| e.is_active = false);
        }
    }

    /// Iterate over all active session entries.
    pub fn active_sessions(&self) -> impl Iterator<Item = &Arc<SessionEntry>> {
        self.sessions.values().filter(|e| e.is_active)
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn next_session_id(&mut self) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id += 1;
        id
    }

    fn create_session(
        &mut self,
        addr: SocketAddr,
    ) -> (SessionId, Arc<SessionEntry>, mpsc::Receiver<ClientCommand>) {
        let id = self.next_session_id();
        let (command_tx, command_rx) = mpsc::channel(64);
        let entry = Arc::new(SessionEntry::new(
            id, command_tx,
            self.static_data.clone(), self.client_version,
            self.data_dir.clone(),
            self.mirror_url.clone(),
        ));
        self.sessions.insert(id, entry.clone());
        let _ = addr;
        (id, entry, command_rx)
    }

    fn find_active_session(&self) -> Option<Arc<SessionEntry>> {
        self.sessions
            .values()
            .filter(|e| e.is_active)
            .max_by_key(|e| e.created_at)
            .cloned()
    }
}
