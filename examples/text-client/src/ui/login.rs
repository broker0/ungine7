//! Login screen widget — server address, account, password form.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, LoginField};

/// Width of the login form box (characters).
const FORM_WIDTH: u16 = 50;
/// Height of the login form box (lines).
const FORM_HEIGHT: u16 = 14;

pub fn render(frame: &mut Frame, _area: Rect, app: &App) {
    let area = frame.area();

    // Center the form on screen.
    let form_area = centered_rect(FORM_WIDTH, FORM_HEIGHT, area);

    // Clear background behind the form.
    frame.render_widget(Clear, form_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Login ")
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(form_area);
    frame.render_widget(block, form_area);

    // Split inner area into rows.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // blank
            Constraint::Length(1), // "Address" label + field
            Constraint::Length(1), // blank
            Constraint::Length(1), // "Account" label + field
            Constraint::Length(1), // blank
            Constraint::Length(1), // "Password" label + field
            Constraint::Length(1), // blank
            Constraint::Length(1), // status
            Constraint::Length(1), // blank
            Constraint::Length(1), // hint
            Constraint::Min(0),   // rest
        ])
        .split(inner);

    let login = &app.login;

    // Render each field.
    render_field(frame, rows[1], "Address:  ", &login.address, false, login.focused == LoginField::Address);
    render_field(frame, rows[3], "Account:  ", &login.account, false, login.focused == LoginField::Account);
    render_field(frame, rows[5], "Password: ", &login.password, true, login.focused == LoginField::Password);

    // Status message.
    let status_style = if login.connecting {
        Style::default().fg(Color::Yellow)
    } else if login.status.starts_with("Error") || login.status.starts_with("error") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    let status = Paragraph::new(Line::from(Span::styled(&login.status, status_style)))
        .alignment(Alignment::Center);
    frame.render_widget(status, rows[7]);

    // Hint line.
    let hint = Paragraph::new(Line::from(vec![
        Span::styled("Tab", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled(" switch field  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled(" connect  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled(" quit", Style::default().fg(Color::DarkGray)),
    ])).alignment(Alignment::Center);
    frame.render_widget(hint, rows[9]);
}

fn render_field(frame: &mut Frame, area: Rect, label: &str, value: &str, masked: bool, focused: bool) {
    let label_width = label.len() as u16;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(2),           // left padding
            Constraint::Length(label_width), // label
            Constraint::Min(4),             // field value
            Constraint::Length(2),           // right padding
        ])
        .split(area);

    let label_style = Style::default().fg(Color::Gray);
    let label_widget = Paragraph::new(Span::styled(label, label_style));
    frame.render_widget(label_widget, cols[1]);

    let display_value = if masked {
        "*".repeat(value.len())
    } else {
        value.to_string()
    };

    let field_style = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut spans = vec![Span::styled(&display_value, field_style)];
    if focused {
        spans.push(Span::styled("_", Style::default().fg(Color::Yellow).add_modifier(Modifier::SLOW_BLINK)));
    }

    let field_widget = Paragraph::new(Line::from(spans));
    frame.render_widget(field_widget, cols[2]);
}

/// Create a centered rectangle of the given size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
