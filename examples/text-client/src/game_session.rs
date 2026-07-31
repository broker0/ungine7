//! Game session — wraps a single character's connection, observer, and state.
//!
//! Designed for multi-head: `App` holds `Vec<GameSession>` (currently just one).

use std::sync::Arc;

use framework::diorama::ObserverPipeline;
use framework::ecumene::StaticDataProvider;
use network::client::{CharacterLoginInfo, GameConnection};
use protocol::RawPacket;
use u_core::position::Facing;

use crate::movement::MovementState;

/// Per-character stats derived from `ObserverEvent`s.
#[derive(Debug, Clone, Default)]
pub struct PlayerStats {
    pub hits: u16,
    pub max_hits: u16,
    pub mana: u16,
    pub max_mana: u16,
    pub stamina: u16,
    pub max_stamina: u16,
}

/// One character session — connection + world state + movement.
#[allow(dead_code)] // Fields reserved for multi-head future.
pub struct GameSession {
    pub id: usize,
    pub name: String,
    pub game: GameConnection,
    pub observer: ObserverPipeline,
    pub movement: MovementState,
    pub stats: PlayerStats,
    pub static_data: Option<Arc<dyn StaticDataProvider>>,
    pub connected: bool,
    /// Pending pong packet to send (queued from sync network handler).
    pub pending_pong: Option<RawPacket>,
    /// Pending packets to send (queued from sync network handler).
    pub pending_replies: Vec<RawPacket>,
    /// Client version string for 0xBD responses (e.g. "3.0.8").
    pub version_string: String,
}

impl GameSession {
    #[allow(dead_code)]
    pub fn new(
        id: usize,
        name: String,
        game: GameConnection,
        static_data: Option<Arc<dyn StaticDataProvider>>,
    ) -> Self {
        let observer = ObserverPipeline::new(static_data.clone());
        Self {
            id,
            name,
            game,
            observer,
            movement: MovementState::new(),
            stats: PlayerStats::default(),
            static_data,
            connected: true,
            pending_pong: None,
            pending_replies: Vec::new(),
            version_string: String::new(),
        }
    }

    /// Create with a pre-built observer that already ingested login packets.
    pub fn new_with_observer(
        id: usize,
        name: String,
        game: GameConnection,
        static_data: Option<Arc<dyn StaticDataProvider>>,
        observer: ObserverPipeline,
    ) -> Self {
        Self {
            id,
            name,
            game,
            observer,
            movement: MovementState::new(),
            stats: PlayerStats::default(),
            static_data,
            connected: true,
            pending_pong: None,
            pending_replies: Vec::new(),
            version_string: String::new(),
        }
    }

    /// Player serial (0 = not yet initialised).
    pub fn serial(&self) -> u32 {
        self.observer.pos.serial
    }

    /// Player position tuple.
    pub fn position(&self) -> (u16, u16, i8) {
        (self.observer.pos.x, self.observer.pos.y, self.observer.pos.z)
    }

    /// Current world index.
    pub fn world(&self) -> u8 {
        self.observer.session.current_world
    }

    /// Current facing direction as raw u8 (0-7).
    #[allow(dead_code)]
    pub fn facing_raw(&self) -> u8 {
        self.observer.pos.facing.heading() as u8
    }

    /// Initialize the observer's position tracker from login info.
    ///
    /// `enter_world()` consumes all packets up to 0x55 LoginComplete,
    /// so the observer never sees them.  This seeds the position from
    /// the data that `enter_world` already parsed.
    #[allow(dead_code)]
    pub fn apply_login_info(&mut self, info: &CharacterLoginInfo) {
        self.observer.pos.serial = info.serial;
        self.observer.pos.x = info.x;
        self.observer.pos.y = info.y;
        self.observer.pos.z = info.z;
        self.observer.pos.facing = Facing::new(info.facing);
        self.observer.pos.body_type = info.body_type;
    }
}
