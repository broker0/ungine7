//! Stats panel — HP, Mana, Stamina, position, direction.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Stats ");

    let Some(session) = app.active_session() else {
        frame.render_widget(
            Paragraph::new("Not connected").block(block),
            area,
        );
        return;
    };

    let s = &session.stats;
    let (x, y, z) = session.position();
    let heading = session.observer.pos.facing.heading();

    let lines = vec![
        hp_bar_line("HP  ", s.hits, s.max_hits, Color::Red),
        hp_bar_line("MP  ", s.mana, s.max_mana, Color::Blue),
        hp_bar_line("ST  ", s.stamina, s.max_stamina, Color::Yellow),
        Line::from(""),
        Line::from(vec![
            Span::styled("Pos: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{},{},{}", x, y, z)),
        ]),
        Line::from(vec![
            Span::styled("Dir: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", heading)),
        ]),
        Line::from(vec![
            Span::styled("Map: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", session.world())),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn hp_bar_line(label: &str, current: u16, max: u16, color: Color) -> Line<'static> {
    let bar_width = 12;
    let filled = if max > 0 {
        ((current as u32 * bar_width as u32) / max as u32).min(bar_width as u32) as usize
    } else {
        0
    };
    let empty = bar_width - filled;

    let bar_filled: String = "\u{2588}".repeat(filled);
    let bar_empty: String = "\u{2591}".repeat(empty);

    Line::from(vec![
        Span::styled(label.to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(bar_filled, Style::default().fg(color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {}/{}", current, max)),
    ])
}
