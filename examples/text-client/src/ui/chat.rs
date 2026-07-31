//! Scrollable chat / event log.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Chat ");
    let inner = block.inner(area);
    let visible_lines = inner.height as usize;

    // Build display lines.
    let total = app.chat_log.len();
    let scroll_offset = app.chat_scroll;
    let start = if total > visible_lines + scroll_offset {
        total - visible_lines - scroll_offset
    } else {
        0
    };
    let end = if total > scroll_offset {
        total - scroll_offset
    } else {
        0
    };

    let lines: Vec<Line<'_>> = app.chat_log[start..end]
        .iter()
        .map(|entry| entry.to_line())
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}
