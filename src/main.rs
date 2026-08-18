use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use claude_code_transparent_router::{config, tui, AppState};
use listenfd::ListenFd;
use tokio::net::TcpListener;

struct Args {
    config: Option<PathBuf>,
    daemon: bool,
    /// Serve every user from their own config and credentials.
    user_config: bool,
    listen: Option<std::net::SocketAddr>,
    idle_timeout: Option<u64>,
    /// `login <provider>` / `logout <provider>`.
    command: Option<(String, String)>,
}

fn parse_args() -> Args {
    let mut args = Args {
        config: None,
        daemon: false,
        user_config: false,
        listen: None,
        idle_timeout: None,
        command: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--config" => args.config = argv.next().map(PathBuf::from),
            "--daemon" => args.daemon = true,
            "--user-config" => args.user_config = true,
            "--listen" => {
                args.listen = argv.next().and_then(|addr| addr.parse().ok());
                if args.listen.is_none() {
                    eprintln!("--listen needs an address like 127.0.0.1:8787");
                    std::process::exit(2);
                }
            }
            "--idle-timeout" => {
                args.idle_timeout = argv.next().and_then(|secs| secs.parse().ok());
                if args.idle_timeout.is_none() {
                    eprintln!("--idle-timeout needs a number of seconds (0 disables)");
                    std::process::exit(2);
                }
            }
            "login" | "logout" => match argv.next() {
                Some(provider) => args.command = Some((arg, provider)),
                None => {
                    eprintln!("{arg} needs a provider name");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                println!(
                    "claude-router [--config <path>] [--daemon] [--listen <addr>]\n\
                     \x20            [--user-config] [--idle-timeout <seconds>]\n\
                     claude-router login <provider>\n\
                     claude-router logout <provider>\n\n\
                     From a terminal: opens the TUI, which configures the running daemon.\n\
                     --daemon (or no TTY): runs the daemon itself.\n\
                     --user-config: serve each connecting user from their own config and\n\
                     \x20             credentials in their home, for a machine-wide daemon.\n\
                     --idle-timeout: exit after this many seconds without a request; pairs\n\
                     \x20              with systemd socket activation. 0 disables."
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
    let config = match config::Config::load(args.config.clone()) {
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
        if let Err(err) = tui::run(Arc::new(config)) {
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
        .block_on(serve(Arc::new(config), args));
}

async fn serve(config: Arc<config::Config>, args: Args) {
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
        _ => match TcpListener::bind(args.listen.unwrap_or(config.listen)).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!(
                    "cannot listen on {}: {err} (is another claude-router daemon running?)",
                    args.listen.unwrap_or(config.listen)
                );
                std::process::exit(1);
            }
        },
    };
    let bound = listener.local_addr().expect("local addr");
    let allowed = config.allowed_uids.clone();
    if allowed.is_empty() {
        tracing::info!(addr = %bound, "claude-router daemon listening (any local user)");
    } else {
        tracing::info!(addr = %bound, uids = ?allowed, "claude-router daemon listening");
    }
    let listener = claude_code_transparent_router::peer::UidFiltered::new(listener, allowed);

    // Serving several users means their providers and credentials come from
    // their own homes, so nothing about them is configured machine-wide.
    let user_configs = (args.user_config || config.user_config).then(|| {
        tracing::info!("serving each user from their own config");
        Arc::new(claude_code_transparent_router::user_config::UserConfigs::new(config.clone()))
    });
    let state = AppState { client, config: config.clone(), user_configs, listen: bound };

    let idle_timeout = args
        .idle_timeout
        .or(config.idle_timeout_secs)
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs);
    let activity = idle_timeout.map(|_| claude_code_transparent_router::idle::Activity::new());
    let app = claude_code_transparent_router::app_with_activity(state, activity.clone());

    let shutdown = async move {
        match (activity, idle_timeout) {
            // Whichever comes first: an idle stretch, or a signal.
            (Some(activity), Some(timeout)) => tokio::select! {
                _ = activity.wait_until_idle(timeout) => {}
                _ = shutdown_signal() => {}
            },
            _ => shutdown_signal().await,
        }
    };

    // ConnectInfo carries the verified peer uid, which is how per-user
    // separation stays the kernel's answer rather than a claim.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<claude_code_transparent_router::peer::PeerInfo>(),
    )
    .with_graceful_shutdown(shutdown)
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
    println!("Sign in to '{provider_name}' at:\n");
    println!("{}\n", started.authorize_url);
    println!(
        "Signing in on another machine? The browser will land on a\n\
         localhost address that does not exist there — paste it here:"
    );
    claude_code_transparent_router::oauth::login::open_browser(&started.authorize_url);

    // Whichever arrives first: this machine's own callback, or a redirect URL
    // pasted from the machine that actually has the browser.
    let session = tokio::select! {
        result = started.complete(&client, oauth_config) => result,
        pasted = read_line() => match pasted {
            Some(url) => started.complete_from_url(&client, oauth_config, &url).await,
            None => Err("no redirect URL given".to_string()),
        },
    };
    let session = match session {
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
    let daemon_base = format!("http://{}", config.listen);
    match claude_code_transparent_router::oauth::hand_to_daemon(
        &client,
        &daemon_base,
        provider_name,
        &session,
    )
    .await
    {
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

/// One line from the terminal, off the runtime thread so the callback server
/// keeps accepting while the user pastes.
async fn read_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok()?;
        Some(line.trim().to_string()).filter(|line| !line.is_empty())
    })
    .await
    .ok()
    .flatten()
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
