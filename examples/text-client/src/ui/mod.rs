//! UI rendering — ratatui layout and widget dispatch.

pub mod map;
pub mod stats;
pub mod chat;
pub mod input_bar;
pub mod nearby;
pub mod login;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::app::{App, AppScreen};

/// Main render entry point.
pub fn render(frame: &mut Frame, app: &App) {
    match app.screen {
        AppScreen::Login => render_login(frame, app),
        AppScreen::Game  => render_game(frame, app),
    }
}

/// Render the login form (centered on screen).
fn render_login(frame: &mut Frame, app: &App) {
    login::render(frame, frame.area(), app);
}

/// Render the in-game UI (map, stats, chat, etc.).
fn render_game(frame: &mut Frame, app: &App) {
    // Top-level: vertical split into [main area | chat | input].
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),        // map + stats
            Constraint::Length(8),       // chat log
            Constraint::Length(3),       // input bar
        ])
        .split(frame.area());

    // Top area: horizontal split [map | right panel].
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(65),  // map
            Constraint::Percentage(35),  // stats + nearby
        ])
        .split(outer[0]);

    // Right panel: vertical split [stats | nearby].
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),   // stats
            Constraint::Min(4),      // nearby list
        ])
        .split(top[1]);

    // Draw each widget.
    map::render(frame, top[0], app);
    stats::render(frame, right[0], app);
    nearby::render(frame, right[1], app);
    chat::render(frame, outer[1], app);
    input_bar::render(frame, outer[2], app);
}

/// Map area rectangle (used by App to translate mouse clicks to map coords).
pub fn map_rect(frame_area: Rect) -> Rect {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(8),
            Constraint::Length(3),
        ])
        .split(frame_area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(65),
            Constraint::Percentage(35),
        ])
        .split(outer[0]);

    top[0]
}
