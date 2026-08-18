pub mod admin;
pub mod catalog;
pub mod config;
pub mod credentials;
pub mod effort;
pub mod headers;
pub mod passthrough;
pub mod providers;
pub mod route;
pub mod sse;
pub mod tui;

use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

/// Request bodies are buffered so the model can be peeked; this bounds that
/// buffer, not any streaming response.
const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub config: Arc<config::Config>,
    pub credentials: Arc<credentials::CredentialStore>,
    /// The daemon's actual bound address, reported to the TUI client.
    pub listen: std::net::SocketAddr,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/models", get(catalog::models))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .merge(admin::routes())
        .fallback(fallback)
        .with_state(state)
}

async fn messages(State(state): State<AppState>, req: Request) -> Response {
    dispatch(state, req, false).await
}

async fn count_tokens(State(state): State<AppState>, req: Request) -> Response {
    dispatch(state, req, true).await
}

/// Buffer the body, route on a shallow parse of `model`, forward original bytes.
async fn dispatch(state: AppState, req: Request, counting: bool) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read request body: {err}")),
    };

    match route::route(&state.config, &bytes) {
        route::Backend::Anthropic => passthrough::send(&state, parts, bytes).await,
        route::Backend::Provider { provider, real_model } => {
            providers::dispatch(&state, provider, bytes, real_model, counting).await
        }
        route::Backend::UnknownAlias { model } => passthrough::proxy_error(&format!(
            "no configured provider lists model '{model}'; check the providers section of the router config"
        )),
    }
}

/// Every path we don't explicitly own is proxied to Anthropic unchanged.
async fn fallback(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read request body: {err}")),
    };
    passthrough::send(&state, parts, bytes).await
}
