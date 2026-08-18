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
}

fn parse_args() -> Args {
    let mut args = Args { config: None, daemon: false };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--config" => args.config = argv.next().map(PathBuf::from),
            "--daemon" => args.daemon = true,
            "--help" | "-h" => {
                println!(
                    "claude-router [--config <path>] [--daemon]\n\n\
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
    tracing::info!(addr = %listen, "claude-router daemon listening");

    let credentials = Arc::new(CredentialStore::new(config.credentials_dir.clone()));
    let state = AppState { client, config, credentials, listen };

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve");
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
