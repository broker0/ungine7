//! Nearby entities list widget.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use framework::vessel::Entity;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Nearby ");

    let Some(session) = app.active_session() else {
        frame.render_widget(Paragraph::new("").block(block), area);
        return;
    };

    let inner_height = block.inner(area).height as usize;
    let (px, py, _) = session.position();

    // Collect entities, sorted by distance.
    let mut entities: Vec<(u32, u16, u16, i8, bool, u8)> = session
        .observer
        .session
        .visible
        .iter()
        .filter(|e| !(e.x() == px && e.y() == py && e.serial == session.serial()))
        .filter(|e| e.is_mobile()) // TODO: temporarily hide non-mobiles
        .map(|e| {
            let noto = e.notoriety().unwrap_or(0);
            (e.serial, e.x(), e.y(), e.z(), e.is_mobile(), noto)
        })
        .collect();

    // Sort by Chebyshev distance.
    entities.sort_by_key(|(_s, x, y, _z, _m, _n)| {
        let dx = (*x as i32 - px as i32).unsigned_abs();
        let dy = (*y as i32 - py as i32).unsigned_abs();
        dx.max(dy)
    });

    let lines: Vec<Line<'_>> = entities
        .iter()
        .take(inner_height)
        .map(|(serial, x, y, _z, is_mobile, noto)| {
            let dx = *x as i32 - px as i32;
            let dy = *y as i32 - py as i32;
            let dist = dx.unsigned_abs().max(dy.unsigned_abs());
            let dir = direction_label(dx, dy);

            let (tag, color) = if *is_mobile {
                ("M", notoriety_color(*noto))
            } else {
                ("I", Color::Green)
            };

            Line::from(vec![
                Span::styled(
                    format!("[{}] ", tag),
                    Style::default().fg(color),
                ),
                Span::raw(format!("{:#010X}", serial)),
                Span::styled(
                    format!(" {}{}",dist, dir),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn direction_label(dx: i32, dy: i32) -> &'static str {
    match (dx.signum(), dy.signum()) {
        (0, -1) => "N",
        (1, -1) => "NE",
        (1, 0)  => "E",
        (1, 1)  => "SE",
        (0, 1)  => "S",
        (-1, 1) => "SW",
        (-1, 0) => "W",
        (-1, -1) => "NW",
        _ => "",
    }
}

fn notoriety_color(noto: u8) -> Color {
    match noto {
        1 => Color::Blue,
        2 => Color::Green,
        3 | 4 => Color::Gray,
        5 => Color::Yellow,
        6 => Color::Red,
        7 => Color::Yellow,
        _ => Color::White,
    }
}
