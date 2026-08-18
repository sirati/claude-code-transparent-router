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
    headless: bool,
}

fn parse_args() -> Args {
    let mut args = Args { config: None, headless: false };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--config" => args.config = argv.next().map(PathBuf::from),
            "--headless" => args.headless = true,
            "--help" | "-h" => {
                println!("claude-router [--config <path>] [--headless]");
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

#[tokio::main]
async fn main() {
    let args = parse_args();
    // The TUI owns the terminal; interactive runs log to a file instead.
    let interactive = !args.headless && std::io::stdout().is_terminal();

    let config = match config::Config::load(args.config) {
        Ok(config) => Arc::new(config),
        Err(err) => {
            eprintln!("config error: {err}");
            std::process::exit(1);
        }
    };
    init_tracing(interactive, &config);

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

    let credentials = Arc::new(CredentialStore::new(config.credentials_dir.clone()));
    let state = AppState { client, config: config.clone(), credentials: credentials.clone() };

    // Socket-activated FD from systemd when present, plain loopback bind otherwise.
    let listener = match ListenFd::from_env().take_tcp_listener(0) {
        Ok(Some(std_listener)) => {
            std_listener.set_nonblocking(true).expect("nonblocking");
            TcpListener::from_std(std_listener).expect("socket-activated listener")
        }
        _ => match TcpListener::bind(config.listen).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("cannot listen on {}: {err} (is another claude-router running?)", config.listen);
                std::process::exit(1);
            }
        },
    };
    let local_addr = listener.local_addr().expect("local addr");
    tracing::info!(addr = %local_addr, "claude-router listening");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(state))
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });

    if interactive {
        let tui_config = config.clone();
        let result = tokio::task::spawn_blocking(move || {
            tui::run(tui_config, credentials, local_addr)
        })
        .await;
        if let Err(err) = result.expect("tui thread") {
            eprintln!("tui error: {err}");
        }
        let _ = shutdown_tx.send(());
    } else {
        shutdown_signal().await;
        let _ = shutdown_tx.send(());
    }
    let _ = server.await.expect("server task");
}

fn init_tracing(interactive: bool, config: &config::Config) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "claude_code_transparent_router=info,claude_router=info".into());
    if interactive {
        let log_dir = config
            .credentials_dir
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&log_dir);
        if let Ok(file) = std::fs::File::options()
            .create(true)
            .append(true)
            .open(log_dir.join("router.log"))
        {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(file)
                .with_ansi(false)
                .init();
        }
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
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
