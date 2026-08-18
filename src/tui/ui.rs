use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table};
use ratatui::Frame;

use crate::credentials::{mask, Source};

use super::{App, Mode};

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, table, footer] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(2)])
            .areas(frame.area());

    draw_header(frame, app, header);
    draw_providers(frame, app, table);
    draw_footer(frame, app, footer);

    if let Mode::Entering { input } = &app.mode {
        draw_input_popup(frame, app, input);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let config_line = match &app.config.config_path {
        Some(path) => format!("config: {}", path.display()),
        None => "config: (defaults; no config file found)".to_string(),
    };
    let text = vec![
        Line::from(vec![
            Span::styled(
                "claude-router",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  listening on http://{}", app.listen)),
        ]),
        Line::from(config_line),
    ];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn draw_providers(frame: &mut Frame, app: &App, area: Rect) {
    if app.config.providers.is_empty() {
        frame.render_widget(
            Paragraph::new("no providers configured — the router is pure passthrough")
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let rows = app.config.providers.iter().enumerate().map(|(i, provider)| {
        let source = app.credentials.source(&provider.name);
        let credential = match source {
            Source::Unset => Span::styled("not set", Style::default().fg(Color::Red)),
            _ => Span::styled(
                format!(
                    "{} ({})",
                    app.credentials.preview(&provider.name).unwrap_or_default(),
                    source.label(),
                ),
                Style::default().fg(Color::Green),
            ),
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
    )
    .block(Block::default().borders(Borders::NONE));
    frame.render_widget(table, area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::ConfirmClear => Line::from(Span::styled(
            format!(
                "clear credential for '{}'? [y]es / [n]o",
                app.config.providers.get(app.selected).map(|p| p.name.as_str()).unwrap_or("?")
            ),
            Style::default().fg(Color::Yellow),
        )),
        _ => match &app.status {
            Some(status) => Line::from(Span::styled(status.clone(), Style::default().fg(Color::Yellow))),
            None => Line::from(Span::styled(
                "↑/↓ select   [s]et credential   [c]lear credential   [q]uit",
                Style::default().fg(Color::DarkGray),
            )),
        },
    };
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

/// Masked entry, Claude Code style: a short visible prefix, the rest `*`s.
fn draw_input_popup(frame: &mut Frame, app: &App, input: &str) {
    let area = centered(frame.area(), 60, 5);
    let name = app.config.providers.get(app.selected).map(|p| p.name.as_str()).unwrap_or("?");
    let shown = if input.is_empty() { String::from("(paste or type, Enter to save, Esc to cancel)") } else { mask(input) };
    let style = if input.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(shown, style))).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" set credential for '{name}' ")),
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
