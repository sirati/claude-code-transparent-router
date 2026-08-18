use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table};
use ratatui::Frame;

use crate::credentials::mask;

use super::{App, Mode};

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, table, footer] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(2)])
            .areas(frame.area());

    draw_header(frame, app, header);
    draw_providers(frame, app, table);
    draw_footer(frame, app, footer);

    match &app.mode {
        Mode::Entering { input } => draw_input_popup(frame, app, input),
        Mode::LoggingIn { provider, url } => draw_login_popup(frame, provider, url),
        _ => {}
    }
}

/// The browser is already open; the URL is here for when it is not, and for
/// signing in from a machine without one.
fn draw_login_popup(frame: &mut Frame, provider: &str, url: &str) {
    let area = centered(frame.area(), frame.area().width.saturating_sub(8).min(100), 9);
    let text = vec![
        Line::from("Waiting for the browser to finish signing in..."),
        Line::from(""),
        Line::from(Span::styled("If it did not open, visit:", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(url.to_string(), Style::default().fg(Color::Cyan))),
        Line::from(""),
        Line::from(Span::styled("Esc to cancel", Style::default().fg(Color::DarkGray))),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(format!(" sign in to '{provider}' "))),
        area,
    );
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let daemon_line = match &app.snapshot {
        Ok(status) => Line::from(vec![
            Span::styled(
                "claude-router",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  daemon at http://{}", status.listen)),
            Span::styled("  connected", Style::default().fg(Color::Green)),
        ]),
        Err(_) => Line::from(vec![
            Span::styled(
                "claude-router",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  disconnected", Style::default().fg(Color::Red)),
        ]),
    };
    let config_line = match app.snapshot.as_ref().ok().and_then(|s| s.config_path.as_deref()) {
        Some(path) => format!("daemon config: {path}"),
        None => "daemon config: (defaults)".to_string(),
    };
    frame.render_widget(
        Paragraph::new(vec![daemon_line, Line::from(config_line)])
            .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn draw_providers(frame: &mut Frame, app: &App, area: Rect) {
    let providers = match &app.snapshot {
        Ok(status) => &status.providers,
        Err(err) => {
            frame.render_widget(
                Paragraph::new(format!("{err}\n\npress [r] to retry"))
                    .style(Style::default().fg(Color::Red)),
                area,
            );
            return;
        }
    };
    if providers.is_empty() {
        frame.render_widget(
            Paragraph::new("no providers configured — the daemon is pure passthrough")
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let rows = providers.iter().enumerate().map(|(i, provider)| {
        let credential = if provider.credential.set {
            Span::styled(
                format!(
                    "{} ({})",
                    provider.credential.preview.as_deref().unwrap_or_default(),
                    provider.credential.source,
                ),
                Style::default().fg(Color::Green),
            )
        } else {
            Span::styled("not set", Style::default().fg(Color::Red))
        };
        let style = if i == app.selected {
            Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Row::new(vec![
            Span::raw(provider.name.clone()),
            Span::raw(provider.base_url.clone()),
            Span::raw(provider.models.join(", ")),
            credential,
        ])
        .style(style)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Percentage(35),
            Constraint::Percentage(25),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["provider", "base url", "models", "credential"])
            .style(Style::default().add_modifier(Modifier::UNDERLINED)),
    );
    frame.render_widget(table, area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::ConfirmClear => Line::from(Span::styled(
            format!(
                "clear credential for '{}'? [y]es / [n]o",
                app.selected_provider().map(|p| p.name.as_str()).unwrap_or("?")
            ),
            Style::default().fg(Color::Yellow),
        )),
        _ => match &app.status {
            Some(status) => {
                Line::from(Span::styled(status.clone(), Style::default().fg(Color::Yellow)))
            }
            None => Line::from(Span::styled(
                "↑/↓ select   [s]et key   [l]og in   [c]lear   [r]efresh   [q]uit",
                Style::default().fg(Color::DarkGray),
            )),
        },
    };
    frame.render_widget(Paragraph::new(line).block(Block::default().borders(Borders::TOP)), area);
}

/// Masked entry, Claude Code style: a short visible prefix, the rest `*`s.
fn draw_input_popup(frame: &mut Frame, app: &App, input: &str) {
    let area = centered(frame.area(), 60, 5);
    let name = app.selected_provider().map(|p| p.name.as_str()).unwrap_or("?");
    let (shown, style) = if input.is_empty() {
        (
            String::from("(paste or type, Enter to save, Esc to cancel)"),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (mask(input), Style::default())
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(shown, style))).block(
            Block::default().borders(Borders::ALL).title(format!(" set credential for '{name}' ")),
        ),
        area,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}
