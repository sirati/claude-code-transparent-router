pub mod admin;
pub mod catalog;
pub mod config;
pub mod credentials;
pub mod effort;
pub mod headers;
pub mod idle;
pub mod oauth;
pub mod passthrough;
pub mod peer;
pub mod presets;
pub mod providers;
pub mod route;
pub mod sse;
pub mod tui;
pub mod user_config;

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
    /// The daemon's own config: listen address, access control, and the
    /// providers used when not resolving per user.
    pub config: Arc<config::Config>,
    /// Set when the daemon serves several users, each with their own config
    /// and credentials in their home directory.
    pub user_configs: Option<Arc<user_config::UserConfigs>>,
    /// The daemon's actual bound address, reported to the TUI client.
    pub listen: std::net::SocketAddr,
}

impl AppState {
    /// The config that governs this request. Serving several users means the
    /// providers, models and picker choice are the caller's own, read from
    /// their home; the uid comes from the kernel, not from the client.
    pub fn config_for(&self, uid: Option<u32>) -> Arc<config::Config> {
        match (&self.user_configs, uid) {
            (Some(configs), Some(uid)) => configs.get(uid),
            _ => self.config.clone(),
        }
    }

    /// Where this caller's credentials live. In per-user mode that is their
    /// own home, so the daemon only ever reads what its owner wrote; the CLI
    /// running as them is what writes it.
    pub fn state_dir(&self, uid: Option<u32>) -> std::path::PathBuf {
        match (&self.user_configs, uid) {
            (Some(_), Some(uid)) => user_config::credentials_dir(uid)
                // A uid with no resolvable home gets a directory that is
                // deliberately empty rather than someone else's keys.
                .unwrap_or_else(|| self.config.credentials_dir.join(format!("unresolved/{uid}"))),
            (Some(_), None) => self.config.credentials_dir.join("unresolved/unknown"),
            (None, _) => self.config.credentials_dir.clone(),
        }
    }

    pub fn credentials(&self, uid: Option<u32>) -> credentials::CredentialStore {
        credentials::CredentialStore::new(self.state_dir(uid))
    }

    pub fn tokens(&self, uid: Option<u32>) -> oauth::TokenStore {
        oauth::TokenStore::new(self.state_dir(uid))
    }
}

pub fn app(state: AppState) -> Router {
    app_with_activity(state, None)
}

/// `activity` is present when the daemon should exit after an idle period;
/// it counts requests, including the whole life of a streamed response.
pub fn app_with_activity(state: AppState, activity: Option<idle::Activity>) -> Router {
    let router = Router::new()
        .route("/v1/models", get(catalog::models))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .merge(admin::routes())
        .fallback(fallback)
        .with_state(state);
    match activity {
        Some(activity) => router.layer(axum::middleware::from_fn(move |req, next| {
            idle::track(activity.clone(), req, next)
        })),
        None => router,
    }
}

async fn messages(
    State(state): State<AppState>,
    peer::Caller(uid): peer::Caller,
    req: Request,
) -> Response {
    dispatch(state, req, false, uid).await
}

async fn count_tokens(
    State(state): State<AppState>,
    peer::Caller(uid): peer::Caller,
    req: Request,
) -> Response {
    dispatch(state, req, true, uid).await
}

/// Buffer the body, route on a shallow parse of `model`, forward original bytes.
async fn dispatch(state: AppState, req: Request, counting: bool, uid: Option<u32>) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read request body: {err}")),
    };

    let config = state.config_for(uid);
    match route::route(&config, &bytes) {
        route::Backend::Anthropic => passthrough::send(&state, &config, parts, bytes).await,
        route::Backend::Provider { provider, real_model } => {
            providers::dispatch(&state, &config, provider, bytes, real_model, counting, uid)
                .await
        }
        route::Backend::UnknownAlias { model } => passthrough::proxy_error(&format!(
            "no configured provider lists model '{model}'; check the providers section of the router config"
        )),
    }
}

/// Every path we don't explicitly own is proxied to Anthropic unchanged.
async fn fallback(
    State(state): State<AppState>,
    peer::Caller(uid): peer::Caller,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read request body: {err}")),
    };
    let config = state.config_for(uid);
    passthrough::send(&state, &config, parts, bytes).await
}
