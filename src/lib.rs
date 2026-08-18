pub mod catalog;
pub mod config;
pub mod headers;
pub mod passthrough;
pub mod providers;
pub mod route;
pub mod sse;

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
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/models", get(catalog::models))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
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
        route::Backend::Provider { real_model } => {
            if counting {
                providers::glm::count_tokens(&bytes)
            } else {
                providers::glm::messages(&state, bytes, real_model).await
            }
        }
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
