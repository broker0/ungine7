//! Terminal input handling — held-key tracking and mouse.
//!
//! Supports two modes:
//!
//! 1. **With keyboard enhancement** (Windows Terminal, modern terminals):
//!    `KeyEventKind::Release` events arrive, so keys are cleared instantly.
//!
//! 2. **Fallback** (legacy console, most Linux terminals):
//!    No `Release` events — instead, each `Press`/`Repeat` refreshes a
//!    timestamp.  Keys that haven't been refreshed within `KEY_EXPIRE`
//!    are considered released.  This still allows diagonal movement if
//!    two keys are pressed within the expiry window.

use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use u_core::position::Heading;

/// How long a key stays "held" without a new Press/Repeat event
/// (fallback mode only).  Tuned so that OS key-repeat (~30ms) keeps
/// the key alive, but a genuine release (no more repeats) expires it
/// quickly without feeling sluggish.
const KEY_EXPIRE: Duration = Duration::from_millis(150);

// ── HeldKeys ──────────────────────────────────────────────────────────────

/// Tracks which movement keys are currently held down.
///
/// Each key stores `Some(last_press_instant)` when active, `None` when
/// released.  In fallback mode (no `Release` events), [`HeldKeys::expire`] must
/// be called periodically to clear stale keys.
#[derive(Debug, Default)]
pub struct HeldKeys {
    up: Option<Instant>,
    down: Option<Instant>,
    left: Option<Instant>,
    right: Option<Instant>,
    pub shift: bool,
    /// Whether the terminal supports `KeyEventKind::Release`.
    /// Set once at startup.  When `true`, keys are cleared via Release
    /// events and `expire()` is a no-op.
    pub has_release_events: bool,
}

impl HeldKeys {
    /// Compute the composite heading from held keys.
    ///
    /// Two keys → diagonal (e.g. up + right → NorthEast).
    /// Single key → cardinal.
    /// None → `None`.
    pub fn heading(&self) -> Option<Heading> {
        let dx = (self.right.is_some() as i32) - (self.left.is_some() as i32);
        let dy = (self.down.is_some() as i32) - (self.up.is_some() as i32);
        Heading::from_delta(dx, dy)
    }

    /// Whether the Shift key is held (run mode).
    pub fn running(&self) -> bool {
        self.shift
    }

    /// Reset all keys to released.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.up = None;
        self.down = None;
        self.left = None;
        self.right = None;
        self.shift = false;
    }

    /// Expire keys that haven't received a Press/Repeat within
    /// `KEY_EXPIRE`.  No-op when the terminal provides Release events.
    pub fn expire(&mut self) {
        if self.has_release_events {
            return;
        }
        let now = Instant::now();
        let cutoff = now - KEY_EXPIRE;
        if self.up.is_some_and(|t| t < cutoff) { self.up = None; }
        if self.down.is_some_and(|t| t < cutoff) { self.down = None; }
        if self.left.is_some_and(|t| t < cutoff) { self.left = None; }
        if self.right.is_some_and(|t| t < cutoff) { self.right = None; }
    }

    /// Handle a key press/release event.  Returns `true` if this was a
    /// movement key (consumed).
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let pressed = key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat;
        let released = key.kind == KeyEventKind::Release;

        if !pressed && !released {
            return false;
        }

        // On press/repeat: stamp with current time.
        // On release: clear.
        let value = if pressed { Some(Instant::now()) } else { None };

        match key.code {
            KeyCode::Up | KeyCode::Char('w') | KeyCode::Char('W') => {
                self.up = value;
                true
            }
            KeyCode::Down | KeyCode::Char('s') | KeyCode::Char('S') => {
                self.down = value;
                true
            }
            KeyCode::Left | KeyCode::Char('a') | KeyCode::Char('A') => {
                self.left = value;
                true
            }
            KeyCode::Right | KeyCode::Char('d') | KeyCode::Char('D') => {
                self.right = value;
                true
            }
            _ => false,
        }
    }

    /// Track shift key state (called for every key event).
    pub fn update_shift(&mut self, modifiers: KeyModifiers) {
        self.shift = modifiers.contains(KeyModifiers::SHIFT);
    }
}

// ── Mouse helpers ─────────────────────────────────────────────────────────

/// Mouse action parsed from a terminal mouse event.
#[derive(Debug, Clone)]
pub enum MouseAction {
    /// Left click on a terminal cell.
    LeftClick { col: u16, row: u16 },
    /// Double left click (detected via timing in App).
    #[allow(dead_code)]
    DoubleLeftClick { col: u16, row: u16 },
    /// Right click (for walk-to-mouse).
    RightClick { col: u16, row: u16 },
    /// Right button held (continuous walk).
    RightHeld { col: u16, row: u16 },
}

/// Parse a crossterm mouse event.
pub fn parse_mouse_event(event: &MouseEvent) -> Option<MouseAction> {
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            Some(MouseAction::LeftClick {
                col: event.column,
                row: event.row,
            })
        }
        MouseEventKind::Down(MouseButton::Right) => {
            Some(MouseAction::RightClick {
                col: event.column,
                row: event.row,
            })
        }
        MouseEventKind::Drag(MouseButton::Right) => {
            Some(MouseAction::RightHeld {
                col: event.column,
                row: event.row,
            })
        }
        _ => None,
    }
}

/// Compute 8-directional heading from screen delta (mouse relative to player).
///
/// Uses the doryen-rs slope heuristic: if the slope ratio exceeds a threshold
/// the movement is purely cardinal; otherwise diagonal.
pub fn heading_from_screen_delta(dx: i32, dy: i32) -> Option<Heading> {
    if dx == 0 && dy == 0 {
        return None;
    }
    let dx_sign = dx.signum();
    let dy_sign = dy.signum();

    if dx == 0 || dy == 0 {
        return Heading::from_delta(dx_sign, dy_sign);
    }

    let slope_dx = (dx.abs() * 3) / dy.abs();
    let slope_dy = (dy.abs() * 3) / dx.abs();

    if slope_dx > 7 {
        Heading::from_delta(dx_sign, 0)
    } else if slope_dy > 7 {
        Heading::from_delta(0, dy_sign)
    } else {
        Heading::from_delta(dx_sign, dy_sign)
    }
}
