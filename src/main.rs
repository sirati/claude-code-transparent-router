use std::sync::Arc;
use std::time::Duration;

use claude_code_transparent_router::{app, config, AppState};
use listenfd::ListenFd;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claude_code_transparent_router=info,claude_router=info".into()),
        )
        .init();

    let config = Arc::new(config::Config::from_env());

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
        _ => TcpListener::bind(config.listen).await.expect("bind loopback"),
    };
    tracing::info!(addr = ?listener.local_addr().ok(), "claude-router listening");

    axum::serve(listener, app(AppState { client, config }))
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
