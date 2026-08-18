//! Console mode: launching `claude-router` from a terminal opens this TUI
//! next to the running server. It shows configured providers and their
//! credential status, and can set (masked input) or clear stored credentials.
//! Credential files are read per-request, so changes apply immediately.

mod ui;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::{execute, terminal};

use crate::config::Config;
use crate::credentials::{CredentialStore, Source};

pub enum Mode {
    List,
    Entering { input: String },
    ConfirmClear,
}

pub struct App {
    pub config: Arc<Config>,
    pub credentials: Arc<CredentialStore>,
    pub listen: SocketAddr,
    pub selected: usize,
    pub mode: Mode,
    pub status: Option<String>,
}

impl App {
    fn provider_name(&self) -> Option<&str> {
        self.config.providers.get(self.selected).map(|p| p.name.as_str())
    }
}

/// Restores the terminal even on early return or panic-unwind.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            terminal::LeaveAlternateScreen,
            event::DisableBracketedPaste
        );
    }
}

pub fn run(
    config: Arc<Config>,
    credentials: Arc<CredentialStore>,
    listen: SocketAddr,
) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(
        io::stdout(),
        terminal::EnterAlternateScreen,
        event::EnableBracketedPaste
    )?;
    let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;

    let mut app = App { config, credentials, listen, selected: 0, mode: Mode::List, status: None };

    loop {
        term.draw(|frame| ui::draw(frame, &app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && on_key(&mut app, key.code, key.modifiers) =>
            {
                return Ok(());
            }
            Event::Paste(text) => {
                if let Mode::Entering { input } = &mut app.mode {
                    input.push_str(&text);
                }
            }
            _ => {}
        }
    }
}

/// Returns true to quit.
fn on_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    match &mut app.mode {
        Mode::List => on_list_key(app, code),
        Mode::Entering { input } => {
            match code {
                KeyCode::Esc => app.mode = Mode::List,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    let key = input.trim().to_string();
                    let name = app.provider_name().unwrap_or_default().to_string();
                    app.mode = Mode::List;
                    if key.is_empty() {
                        app.status = Some("empty input; credential unchanged".into());
                    } else {
                        app.status = Some(match app.credentials.set(&name, &key) {
                            Ok(()) => format!("credential for '{name}' saved"),
                            Err(err) => format!("failed to save credential: {err}"),
                        });
                    }
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            }
            false
        }
        Mode::ConfirmClear => {
            if let KeyCode::Char('y') | KeyCode::Char('Y') = code {
                let name = app.provider_name().unwrap_or_default().to_string();
                app.status = Some(match app.credentials.clear(&name) {
                    Ok(()) => format!("credential for '{name}' cleared"),
                    Err(err) => format!("failed to clear credential: {err}"),
                });
            }
            app.mode = Mode::List;
            false
        }
    }
}

fn on_list_key(app: &mut App, code: KeyCode) -> bool {
    let count = app.config.providers.len();
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Up | KeyCode::Char('k') if count > 0 => {
            app.selected = app.selected.checked_sub(1).unwrap_or(count - 1);
        }
        KeyCode::Down | KeyCode::Char('j') if count > 0 => {
            app.selected = (app.selected + 1) % count;
        }
        KeyCode::Char('s') | KeyCode::Enter if count > 0 => {
            app.mode = Mode::Entering { input: String::new() };
            app.status = None;
        }
        KeyCode::Char('c') if count > 0 => {
            let name = app.provider_name().unwrap_or_default();
            match app.credentials.source(name) {
                Source::File => {
                    app.mode = Mode::ConfirmClear;
                    app.status = None;
                }
                Source::Unset => app.status = Some("no stored credential to clear".into()),
                source => {
                    app.status = Some(format!(
                        "credential comes from the {}; it cannot be cleared here",
                        source.label()
                    ));
                }
            }
        }
        _ => {}
    }
    false
}
