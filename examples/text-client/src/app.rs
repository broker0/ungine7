//! App state — central structure that owns sessions, chat log, and UI state.
//!
//! Designed for multi-head: `sessions: Vec<GameSession>` with `active` index.

use std::time::Instant;

use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::layout::Rect;

use u_core::ProtocolVersion;

use framework::diorama::ObserverEvent;
use files::radarcol::RadarColors;

use crate::config;
use crate::game_session::GameSession;
use crate::input::HeldKeys;

// ── App screen (login vs game) ───────────────────────────────────────────

/// Which screen the application is currently showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppScreen {
    Login,
    Game,
}

/// Which field is focused on the login form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginField {
    Address,
    Account,
    Password,
}

impl LoginField {
    pub fn next(self) -> Self {
        match self {
            Self::Address  => Self::Account,
            Self::Account  => Self::Password,
            Self::Password => Self::Address,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Address  => Self::Password,
            Self::Account  => Self::Address,
            Self::Password => Self::Account,
        }
    }
}

/// State for the login screen.
pub struct LoginState {
    pub address: String,
    pub account: String,
    pub password: String,
    pub client_version: ProtocolVersion,
    pub encrypted: bool,
    pub server_index: u16,
    pub focused: LoginField,
    /// Status message displayed below the form.
    pub status: String,
    /// True while an async login attempt is in progress.
    pub connecting: bool,
}

impl LoginState {
    pub fn new(address: String, client_version: ProtocolVersion, encrypted: bool) -> Self {
        Self {
            address,
            account: config::ACCOUNT.to_string(),
            password: config::PASSWORD.to_string(),
            client_version,
            encrypted,
            server_index: config::SERVER_INDEX,
            focused: LoginField::Account,
            status: String::new(),
            connecting: false,
        }
    }
}

// ── Chat log ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChatEntry {
    pub tag: String,
    pub tag_color: Color,
    pub message: String,
}

impl ChatEntry {
    pub fn system(msg: impl Into<String>) -> Self {
        Self {
            tag: "system".into(),
            tag_color: Color::DarkGray,
            message: msg.into(),
        }
    }

    pub fn chat(name: &str, msg: &str) -> Self {
        Self {
            tag: name.to_string(),
            tag_color: Color::Cyan,
            message: msg.to_string(),
        }
    }

    pub fn combat(msg: impl Into<String>) -> Self {
        Self {
            tag: "combat".into(),
            tag_color: Color::Red,
            message: msg.into(),
        }
    }

    pub fn world(msg: impl Into<String>) -> Self {
        Self {
            tag: "world".into(),
            tag_color: Color::Green,
            message: msg.into(),
        }
    }

    pub fn to_line(&self) -> Line<'_> {
        Line::from(vec![
            Span::styled(
                format!("[{}] ", self.tag),
                ratatui::style::Style::default().fg(self.tag_color),
            ),
            Span::raw(&self.message),
        ])
    }
}

// ── App ───────────────────────────────────────────────────────────────────

pub struct App {
    /// Current screen (login form or game).
    pub screen: AppScreen,
    /// Login form state.
    pub login: LoginState,

    /// Active game sessions (multi-head ready — currently just one).
    pub sessions: Vec<GameSession>,
    /// Index of the currently controlled session.
    pub active: usize,

    /// Chat / event log.
    pub chat_log: Vec<ChatEntry>,
    /// Chat scroll offset (0 = bottom).
    pub chat_scroll: usize,
    /// Maximum chat log entries.
    pub max_chat: usize,

    /// Input mode (true = typing in input bar).
    pub input_mode: bool,
    /// Current input text.
    pub input_buffer: String,

    /// Held movement keys.
    pub held_keys: HeldKeys,

    /// Last left-click time (for double-click detection).
    pub last_click: Option<(Instant, u16, u16)>,

    /// Cached map area rectangle (for mouse-to-world translation).
    pub map_area: Rect,

    /// Should the application exit?
    pub should_quit: bool,

    /// Radar colors for land tile coloring (optional).
    pub radar_colors: Option<RadarColors>,
}

impl App {
    pub fn new(login: LoginState) -> Self {
        Self {
            screen: AppScreen::Login,
            login,
            sessions: Vec::new(),
            active: 0,
            chat_log: Vec::new(),
            chat_scroll: 0,
            max_chat: 500,
            input_mode: false,
            input_buffer: String::new(),
            held_keys: HeldKeys::default(),
            last_click: None,
            map_area: Rect::default(),
            should_quit: false,
            radar_colors: None,
        }
    }

    /// Get the active session (if any).
    pub fn active_session(&self) -> Option<&GameSession> {
        self.sessions.get(self.active)
    }

    /// Get the active session mutably.
    #[allow(dead_code)]
    pub fn active_session_mut(&mut self) -> Option<&mut GameSession> {
        self.sessions.get_mut(self.active)
    }

    /// Add a message to the chat log.
    pub fn push_chat(&mut self, entry: ChatEntry) {
        self.chat_log.push(entry);
        if self.chat_log.len() > self.max_chat {
            self.chat_log.remove(0);
        }
        // Reset scroll to bottom on new message.
        self.chat_scroll = 0;
    }

    /// Process an ObserverEvent and update app state.
    pub fn handle_observer_event(&mut self, event: ObserverEvent, session_idx: usize) {
        // Update stats on the target session.
        if let Some(session) = self.sessions.get_mut(session_idx) {
            let my_serial = session.serial();
            match &event {
                ObserverEvent::HpUpdated { serial, hits, max_hits, .. } if *serial == my_serial => {
                    session.stats.hits = *hits;
                    session.stats.max_hits = *max_hits;
                }
                ObserverEvent::ManaUpdated { serial, mana, max_mana, .. } if *serial == my_serial => {
                    session.stats.mana = *mana;
                    session.stats.max_mana = *max_mana;
                }
                ObserverEvent::StaminaUpdated { serial, stamina, max_stamina, .. } if *serial == my_serial => {
                    session.stats.stamina = *stamina;
                    session.stats.max_stamina = *max_stamina;
                }
                _ => {}
            }
        }

        // Chat log entries.
        match event {
            ObserverEvent::Speech { name, message, .. } => {
                self.push_chat(ChatEntry::chat(&name, &message));
            }
            ObserverEvent::ClilocMessage { name, cliloc_id, args, .. } => {
                let msg = if args.is_empty() {
                    format!("cliloc #{}", cliloc_id)
                } else {
                    format!("cliloc #{}: {}", cliloc_id, args)
                };
                self.push_chat(ChatEntry {
                    tag: name,
                    tag_color: Color::Cyan,
                    message: msg,
                });
            }
            ObserverEvent::DamageDealt { serial, amount, .. } => {
                self.push_chat(ChatEntry::combat(
                    format!("{:#010X} takes {} damage", serial, amount),
                ));
            }
            ObserverEvent::MobileAppeared { serial, graphic, x, y, z, .. } => {
                self.push_chat(ChatEntry::world(
                    format!("Mobile {:#010X} (body {:#06X}) appeared at {},{},{}", serial, graphic, x, y, z),
                ));
            }
            ObserverEvent::MobileRemoved { serial } => {
                self.push_chat(ChatEntry::world(
                    format!("Mobile {:#010X} left view", serial),
                ));
            }
            ObserverEvent::PositionChanged { .. } => {
                // Don't spam — position is visible in the stats panel.
            }
            ObserverEvent::GumpOpened { gump_id, serial, .. } => {
                self.push_chat(ChatEntry::system(
                    format!("Gump opened: id={:#010X} serial={:#010X}", gump_id, serial),
                ));
            }
            ObserverEvent::TargetRequest { cursor_id, .. } => {
                self.push_chat(ChatEntry::system(
                    format!("Target cursor requested (id={:#010X})", cursor_id),
                ));
            }
            _ => {
                // Other events: silently ignored for now.
            }
        }
    }

    /// Translate a terminal (col, row) to world coordinates, given the map area.
    pub fn screen_to_world(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        let area = self.map_area;
        // Account for border (1 cell on each side).
        let inner_x = area.x + 1;
        let inner_y = area.y + 1;
        let inner_w = area.width.saturating_sub(2);
        let inner_h = area.height.saturating_sub(2);

        if col < inner_x || row < inner_y {
            return None;
        }
        let dx = col - inner_x;
        let dy = row - inner_y;
        if dx >= inner_w || dy >= inner_h {
            return None;
        }

        let session = self.active_session()?;
        let (px, py, _) = session.position();
        let center_x = inner_w as i32 / 2;
        let center_y = inner_h as i32 / 2;

        let wx = px as i32 + (dx as i32 - center_x);
        let wy = py as i32 + (dy as i32 - center_y);

        if wx < 0 || wy < 0 || wx > 0xFFFF || wy > 0xFFFF {
            return None;
        }
        Some((wx as u16, wy as u16))
    }

    /// Find the entity at a world tile (prefers mobiles over items).
    pub fn entity_at(&self, wx: u16, wy: u16) -> Option<u32> {
        let session = self.active_session()?;
        let mut best: Option<(u32, bool)> = None;

        for entity in session.observer.session.visible.iter() {
            if entity.x() == wx && entity.y() == wy {
                match best {
                    None => best = Some((entity.serial, entity.is_mobile())),
                    Some((_, false)) if entity.is_mobile() => {
                        best = Some((entity.serial, true));
                    }
                    _ => {}
                }
            }
        }
        best.map(|(s, _)| s)
    }
}
