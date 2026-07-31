//! Input bar widget — command entry line at the bottom.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let title = if app.input_mode {
        " Input (Esc=cancel) "
    } else {
        " Enter=type, WASD=move, Esc=quit "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if app.input_mode {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    let display = if app.input_mode {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Yellow)),
            Span::raw(&app.input_buffer),
            Span::styled("_", Style::default().fg(Color::Yellow).add_modifier(Modifier::SLOW_BLINK)),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
        ])
    };

    let paragraph = Paragraph::new(display).block(block);
    frame.render_widget(paragraph, area);
}
