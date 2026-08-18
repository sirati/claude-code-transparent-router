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
    /// Browser sign-in running. The URL stays on screen for a browser
    /// elsewhere, and `pasted` collects the redirect URL coming back.
    LoggingIn { provider: String, url: String, pasted: String },
}

pub struct App {
    pub daemon: Client,
    pub snapshot: Result<Status, String>,
    pub selected: usize,
    pub mode: Mode,
    pub status: Option<String>,
    /// The router's own config, for the OAuth details a login needs.
    config: std::sync::Arc<Config>,
    /// Result of a login running on another thread.
    login: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// Sends a hand-pasted redirect URL to that thread.
    redirect: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl App {
    fn providers(&self) -> &[client::Provider] {
        self.snapshot.as_ref().map(|s| s.providers.as_slice()).unwrap_or(&[])
    }

    fn selected_provider(&self) -> Option<&client::Provider> {
        self.providers().get(self.selected)
    }

    /// Run the browser flow on a worker thread so the interface keeps
    /// drawing, and hand the finished session to the daemon.
    fn start_login(&mut self) {
        let Some(provider) = self.selected_provider() else { return };
        if !provider.oauth {
            self.status = Some(format!(
                "provider '{}' uses an API key; press [s] to set it",
                provider.name
            ));
            return;
        }
        let name = provider.name.clone();
        let Some(oauth) = self
            .config
            .providers
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.oauth.clone())
        else {
            self.status =
                Some(format!("no OAuth settings for '{name}' in this config; cannot sign in"));
            return;
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let (url_tx, url_rx) = std::sync::mpsc::channel();
        let (redirect_tx, redirect_rx) = tokio::sync::mpsc::unbounded_channel();
        let daemon = self.daemon.base_url();
        let provider_name = name.clone();
        std::thread::spawn(move || {
            let result = run_login(&oauth, &provider_name, &daemon, url_tx, redirect_rx);
            let _ = tx.send(result);
        });

        // The URL arrives as soon as the callback port is bound; a failure to
        // bind shows up as the login result instead.
        match url_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(url) => {
                crate::oauth::login::open_browser(&url);
                self.mode = Mode::LoggingIn { provider: name, url, pasted: String::new() };
                self.status = None;
            }
            Err(_) => self.mode = Mode::List,
        }
        self.login = Some(rx);
        self.redirect = Some(redirect_tx);
    }

    /// Pick up a finished login without blocking the interface.
    fn poll_login(&mut self) {
        let Some(rx) = &self.login else { return };
        match rx.try_recv() {
            Ok(result) => {
                self.login = None;
                self.redirect = None;
                self.mode = Mode::List;
                self.status = Some(match result {
                    Ok(message) => message,
                    Err(err) => format!("login failed: {err}"),
                });
                self.refresh();
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.login = None;
                self.mode = Mode::List;
                self.status = Some("login ended unexpectedly".into());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
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

pub fn run(config: std::sync::Arc<Config>) -> io::Result<()> {
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

    let mut app = App {
        daemon,
        snapshot,
        selected: 0,
        mode: Mode::List,
        status: None,
        config,
        login: None,
        redirect: None,
    };
    let mut ticks: u32 = 0;

    loop {
        app.poll_login();
        let mut hyperlink = None;
        term.draw(|frame| hyperlink = ui::draw(frame, &app))?;
        if let Some(link) = hyperlink {
            write_hyperlink(&link)?;
        }
        if !event::poll(Duration::from_millis(200))? {
            // Pick up daemon-side changes (and reconnects) in the background,
            // but never mid-entry: a refresh must not eat pasted input.
            ticks += 1;
            if ticks.is_multiple_of(10) && matches!(app.mode, Mode::List) {
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
            Event::Paste(text) => match &mut app.mode {
                Mode::Entering { input } => input.push_str(&text),
                Mode::LoggingIn { pasted, .. } => pasted.push_str(text.trim()),
                _ => {}
            },
            _ => {}
        }
    }
}

/// OSC 8 hyperlink, written straight to the terminal because ratatui's cell
/// buffer has no way to carry one. Terminals that understand it make the
/// anchor clickable; the rest simply show the text.
fn write_hyperlink(link: &ui::Hyperlink) -> io::Result<()> {
    use std::io::Write;
    let mut out = io::stdout();
    ratatui::crossterm::queue!(out, ratatui::crossterm::cursor::MoveTo(link.x, link.y))?;
    write!(out, "\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", link.url, link.text)?;
    out.flush()
}

fn run_login(
    oauth: &crate::config::OauthConfig,
    provider: &str,
    daemon: &str,
    url_tx: std::sync::mpsc::Sender<String>,
    mut redirect_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not start the login runtime: {err}"))?;
    runtime.block_on(async {
        let started = crate::oauth::login::start(oauth).await?;
        let _ = url_tx.send(started.authorize_url.clone());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|err| format!("could not build an HTTP client: {err}"))?;
        // Whichever arrives first: this machine's callback, or a redirect URL
        // pasted from wherever the browser actually is.
        let session = tokio::select! {
            result = started.complete(&client, oauth) => result?,
            pasted = redirect_rx.recv() => match pasted {
                Some(url) => started.complete_from_url(&client, oauth, &url).await?,
                None => return Err("login cancelled".to_string()),
            },
        };
        // The daemon owns the store its requests read from, so it takes the
        // finished session rather than this process writing a file.
        crate::oauth::hand_to_daemon(&client, daemon, provider, &session).await?;
        Ok(format!("signed in to '{provider}' ({})", session.preview()))
    })
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
        Mode::LoggingIn { pasted, .. } => {
            match code {
                KeyCode::Esc => {
                    app.login = None;
                    app.redirect = None;
                    app.mode = Mode::List;
                    app.status = Some("login cancelled".into());
                }
                KeyCode::Backspace => {
                    pasted.pop();
                }
                // Hand the pasted redirect to the waiting login thread.
                KeyCode::Enter => {
                    let url = pasted.trim().to_string();
                    if url.is_empty() {
                        app.status = Some("paste the URL the browser was sent to".into());
                    } else if let Some(tx) = &app.redirect {
                        let _ = tx.send(url);
                        app.status = Some("completing sign-in...".into());
                    }
                }
                KeyCode::Char(c) => pasted.push(c),
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
                // OAuth providers have nothing to paste: start the browser
                // flow instead of asking for a key.
                Some(provider) if provider.oauth => app.start_login(),
                Some(_) => {
                    app.mode = Mode::Entering { input: String::new() };
                    app.status = None;
                }
                None => {}
            }
        }
        KeyCode::Char('l') if count > 0 => app.start_login(),
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
