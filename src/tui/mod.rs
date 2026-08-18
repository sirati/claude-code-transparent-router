//! Console mode: `claude-router` from a terminal opens this TUI, a pure
//! client of the already-running daemon's `/__router` admin API. It never
//! listens itself. Credentials set here are stored by the daemon and apply
//! to the next request immediately; pasted keys are masked (prefix + `****`).

pub mod client;
mod ui;

use std::io;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::{execute, terminal};

use crate::config::Config;
use client::{Client, Status};

pub enum Mode {
    List,
    Entering { input: String },
    ConfirmClear,
}

pub struct App {
    pub daemon: Client,
    pub snapshot: Result<Status, String>,
    pub selected: usize,
    pub mode: Mode,
    pub status: Option<String>,
}

impl App {
    fn providers(&self) -> &[client::Provider] {
        self.snapshot.as_ref().map(|s| s.providers.as_slice()).unwrap_or(&[])
    }

    fn selected_provider(&self) -> Option<&client::Provider> {
        self.providers().get(self.selected)
    }

    fn refresh(&mut self) {
        self.snapshot = self.daemon.status();
        let count = self.providers().len();
        self.selected = self.selected.min(count.saturating_sub(1));
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

pub fn run(config: &Config) -> io::Result<()> {
    let daemon = Client::new(config.listen);
    let snapshot = daemon.status();

    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(
        io::stdout(),
        terminal::EnterAlternateScreen,
        event::EnableBracketedPaste
    )?;
    let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;

    let mut app = App { daemon, snapshot, selected: 0, mode: Mode::List, status: None };
    let mut ticks: u32 = 0;

    loop {
        term.draw(|frame| ui::draw(frame, &app))?;
        if !event::poll(Duration::from_millis(500))? {
            // Pick up daemon-side changes (and reconnects) in the background,
            // but never mid-entry: a refresh must not eat pasted input.
            ticks += 1;
            if ticks.is_multiple_of(4) && matches!(app.mode, Mode::List) {
                app.refresh();
            }
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

/// Returns true to quit (the daemon keeps running).
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
                    let name =
                        app.selected_provider().map(|p| p.name.clone()).unwrap_or_default();
                    app.mode = Mode::List;
                    if key.is_empty() {
                        app.status = Some("empty input; credential unchanged".into());
                    } else {
                        app.status = Some(match app.daemon.set_credential(&name, &key) {
                            Ok(()) => format!("credential for '{name}' saved"),
                            Err(err) => format!("failed to save credential: {err}"),
                        });
                        app.refresh();
                    }
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            }
            false
        }
        Mode::ConfirmClear => {
            if let KeyCode::Char('y') | KeyCode::Char('Y') = code {
                let name = app.selected_provider().map(|p| p.name.clone()).unwrap_or_default();
                app.status = Some(match app.daemon.clear_credential(&name) {
                    Ok(()) => format!("credential for '{name}' cleared"),
                    Err(err) => format!("failed to clear credential: {err}"),
                });
                app.refresh();
            }
            app.mode = Mode::List;
            false
        }
    }
}

fn on_list_key(app: &mut App, code: KeyCode) -> bool {
    let count = app.providers().len();
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('r') => {
            app.refresh();
            app.status = None;
        }
        KeyCode::Up | KeyCode::Char('k') if count > 0 => {
            app.selected = app.selected.checked_sub(1).unwrap_or(count - 1);
        }
        KeyCode::Down | KeyCode::Char('j') if count > 0 => {
            app.selected = (app.selected + 1) % count;
        }
        KeyCode::Char('s') | KeyCode::Enter if count > 0 => {
            match app.selected_provider() {
                // The browser flow needs a terminal of its own, so the TUI
                // points at the command rather than trying to host it.
                Some(provider) if provider.oauth => {
                    let name = provider.name.clone();
                    app.status = Some(format!("run `claude-router login {name}` to sign in"));
                }
                Some(_) => {
                    app.mode = Mode::Entering { input: String::new() };
                    app.status = None;
                }
                None => {}
            }
        }
        KeyCode::Char('c') if count > 0 => {
            let credential = app.selected_provider().map(|p| p.credential.clone());
            match credential {
                Some(c) if c.can_clear => {
                    app.mode = Mode::ConfirmClear;
                    app.status = None;
                }
                Some(c) if !c.set => app.status = Some("no stored credential to clear".into()),
                Some(c) => {
                    app.status = Some(format!(
                        "credential comes from the {}; it cannot be cleared here",
                        c.source
                    ));
                }
                None => {}
            }
        }
        _ => {}
    }
    false
}
