use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use claude_code_transparent_router::credentials::CredentialStore;
use claude_code_transparent_router::{app, config, tui, AppState};
use listenfd::ListenFd;
use tokio::net::TcpListener;

struct Args {
    config: Option<PathBuf>,
    daemon: bool,
    /// `login <provider>` / `logout <provider>`.
    command: Option<(String, String)>,
}

fn parse_args() -> Args {
    let mut args = Args { config: None, daemon: false, command: None };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--config" => args.config = argv.next().map(PathBuf::from),
            "--daemon" => args.daemon = true,
            "login" | "logout" => match argv.next() {
                Some(provider) => args.command = Some((arg, provider)),
                None => {
                    eprintln!("{arg} needs a provider name");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                println!(
                    "claude-router [--config <path>] [--daemon]\n\
                     claude-router login <provider>\n\
                     claude-router logout <provider>\n\n\
                     From a terminal: opens the TUI that configures the running daemon.\n\
                     --daemon (or no TTY): runs the daemon itself."
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

fn main() {
    let args = parse_args();
    let config = match config::Config::load(args.config) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {err}");
            std::process::exit(1);
        }
    };

    if let Some((command, provider)) = args.command {
        let code = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run_auth_command(&config, &command, &provider));
        std::process::exit(code);
    }

    // Terminal launch = TUI client for the running daemon; it never listens.
    if !args.daemon && std::io::stdout().is_terminal() {
        if let Err(err) = tui::run(&config) {
            eprintln!("tui error: {err}");
            std::process::exit(1);
        }
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claude_code_transparent_router=info,claude_router=info".into()),
        )
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(serve(Arc::new(config)));
}

async fn serve(config: Arc<config::Config>) {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // No total request timeout: SSE turns run for minutes.
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        // Forward content-encoding truthfully: never decompress on our side.
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client");

    // Socket-activated FD from systemd when present, plain loopback bind otherwise.
    let listener = match ListenFd::from_env().take_tcp_listener(0) {
        Ok(Some(std_listener)) => {
            std_listener.set_nonblocking(true).expect("nonblocking");
            TcpListener::from_std(std_listener).expect("socket-activated listener")
        }
        _ => match TcpListener::bind(config.listen).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!(
                    "cannot listen on {}: {err} (is another claude-router daemon running?)",
                    config.listen
                );
                std::process::exit(1);
            }
        },
    };
    let listen = listener.local_addr().expect("local addr");
    let allowed = config.allowed_uids.clone();
    if allowed.is_empty() {
        tracing::info!(addr = %listen, "claude-router daemon listening (any local user)");
    } else {
        tracing::info!(addr = %listen, uids = ?allowed, "claude-router daemon listening");
    }
    let listener = claude_code_transparent_router::peer::UidFiltered::new(listener, allowed);

    let credentials = Arc::new(CredentialStore::new(config.credentials_dir.clone()));
    let tokens = Arc::new(claude_code_transparent_router::oauth::TokenStore::new(&config.credentials_dir));
    let state = AppState { client, config, credentials, tokens, listen };

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve");
}

/// `login` / `logout`: run against the token store directly, so they work
/// whether or not the daemon is up. Returns a process exit code.
async fn run_auth_command(config: &config::Config, command: &str, provider_name: &str) -> i32 {
    let Some(provider) = config.providers.iter().find(|p| p.name == provider_name) else {
        eprintln!("no provider named '{provider_name}' in the config");
        return 1;
    };
    let Some(oauth_config) = provider.oauth.as_ref() else {
        eprintln!(
            "provider '{provider_name}' uses an API key, not a login; \
             set its credential in the TUI instead"
        );
        return 1;
    };
    let tokens = claude_code_transparent_router::oauth::TokenStore::new(&config.credentials_dir);

    if command == "logout" {
        return match tokens.clear(provider_name) {
            Ok(()) => {
                println!("signed out of '{provider_name}'");
                0
            }
            Err(err) => {
                eprintln!("could not clear the '{provider_name}' login: {err}");
                1
            }
        };
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("http client");
    let started = match claude_code_transparent_router::oauth::login::start(oauth_config).await {
        Ok(started) => started,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    println!("Opening your browser to sign in to '{provider_name}'.");
    println!("If it does not open, visit:\n\n{}\n", started.authorize_url);
    claude_code_transparent_router::oauth::login::open_browser(&started.authorize_url);

    let session = match started.complete(&client, oauth_config).await {
        Ok(session) => session,
        Err(err) => {
            eprintln!("login failed: {err}");
            return 1;
        }
    };

    // Hand the session to the running daemon: under a system service it runs
    // as a different user with its own state directory, and would never see a
    // file written here. Falling back to the local store covers the case
    // where the daemon is not up yet.
    let daemon = claude_code_transparent_router::tui::client::Client::new(config.listen);
    match daemon.set_tokens(provider_name, &session) {
        Ok(()) => {
            println!("Signed in to '{provider_name}' ({}).", session.preview());
            0
        }
        Err(daemon_err) => match tokens.save(provider_name, &session) {
            Ok(()) => {
                println!(
                    "Signed in to '{provider_name}' ({}), stored locally.\n\
                     The running daemon did not accept it ({daemon_err}); \
                     it will pick this up if it reads {}.",
                    session.preview(),
                    config.credentials_dir.display(),
                );
                0
            }
            Err(err) => {
                eprintln!("signed in, but could not save the tokens: {err}");
                1
            }
        },
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}
