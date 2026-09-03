pub mod admin;
pub mod agent_schema;
pub mod catalog;
pub mod compact;
pub mod config;
pub mod content;
pub mod continuation;
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
pub mod ssh_proxy;
pub mod system_prompt;
pub mod tui;
pub mod user_config;

use std::sync::Arc;

use arc_swap::ArcSwap;

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

pub const OUTBOUND_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Shared outbound transport. Redirects are disabled because a redirect could
/// replay provider credentials to a URL the router did not select.
pub fn outbound_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(OUTBOUND_CONNECT_TIMEOUT)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("outbound HTTP client")
}

/// Buffered conversation requests are memory-heavy, so the in-flight count is
/// bounded — but a permit lives for the whole streamed response, and one Claude
/// Code session legitimately runs a main conversation, parallel subagents, and
/// concurrent count_tokens calls at once. The limit exists to stop unbounded
/// runaway (sixteen 32 MiB bodies still fit under the unit's MemoryMax), not
/// to throttle that parallelism.
const MAX_IN_FLIGHT_REQUESTS: usize = 16;

/// Request bodies can fan out into several JSON representations while they
/// are translated. Keep the wire body low enough that those bounded copies do
/// not destabilise the local control plane.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    /// Lazy per-provider clients; SSH-proxied providers own a SOCKS tunnel,
    /// while ordinary providers continue using the direct shared client.
    pub provider_clients: Arc<ssh_proxy::ProviderClients>,
    /// The daemon's own config: listen address, access control, and the
    /// providers used when not resolving per user. SIGHUP replaces this only
    /// after a complete config parse succeeds; in-flight requests retain the
    /// snapshot they selected.
    pub config: Arc<ArcSwap<config::Config>>,
    /// Set when the daemon serves several users, each with their own config
    /// and credentials in their home directory.
    pub user_configs: Option<Arc<user_config::UserConfigs>>,
    /// The daemon's actual bound address, reported to the TUI client.
    pub listen: std::net::SocketAddr,
    /// One-shot bypass armed by the override phrase.
    pub compact_override: compact::Override,
    /// Paces the force-continuation reminder across a conversation's turns.
    pub reminders: Arc<continuation::ReminderTracker>,
}

impl AppState {
    /// The config that governs this request. Serving several users means the
    /// providers, models and picker choice are the caller's own, read from
    /// their home; the uid comes from the kernel, not from the client.
    pub fn config_for(&self, uid: Option<u32>) -> Arc<config::Config> {
        match (&self.user_configs, uid) {
            (Some(configs), Some(uid)) => configs.get(uid),
            _ => self.config.load_full(),
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
                .unwrap_or_else(|| self.config.load().credentials_dir.join(format!("unresolved/{uid}"))),
            (Some(_), None) => self.config.load().credentials_dir.join("unresolved/unknown"),
            (None, _) => self.config.load().credentials_dir.clone(),
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
    app_with_activity(state, idle::Activity::new(), idle::Drain::new())
}

/// `activity` counts requests, including the whole life of a streamed response.
/// The daemon uses it for both idle exit and handover drain barriers.
pub fn app_with_activity(state: AppState, activity: idle::Activity, drain: idle::Drain) -> Router {
    let admission = idle::Admission::new(MAX_IN_FLIGHT_REQUESTS);
    let router = Router::new()
        .route("/v1/models", get(catalog::models))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .merge(admin::routes())
        .fallback(fallback)
        .with_state(state);
    router.layer(axum::middleware::from_fn(move |req, next| {
        idle::track(activity.clone(), drain.clone(), admission.clone(), req, next)
    }))
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

    // Two control cases, both decided from the last user message. Neither is
    // forwarded anywhere: the override arms a bypass and answers in-band, and
    // knowing a request is a compaction changes how a provider is asked.
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
    let mut bytes = bytes;
    let mut parsed = parsed;
    // Strip broken persisted thinking blocks before compaction detection,
    // request shaping, and backend selection. This protects all ordinary
    // messages too, including direct Anthropic passthrough.
    if let Some(request) = parsed.as_mut() {
        if content::sanitize_thinking(request) {
            bytes = serde_json::to_vec(request).expect("request JSON was already parsed").into();
        }
    }
    // The host's Agent model enum only names Anthropic models. Extend it once
    // before backend selection, so every provider dialect — including a
    // passthrough Anthropic request — lets a selector choose a routed agent.
    if !counting {
        if let Some(request) = parsed.as_mut() {
            if agent_schema::extend_model_enum(&config, request) {
                bytes = serde_json::to_vec(request).expect("request JSON was already parsed").into();
            }
        }
    }
    if let Some(parsed) = &parsed {
        if compact::is_override(parsed) {
            state.compact_override.arm();
            tracing::info!("compaction override armed for the next request");
            return compact::armed_reply(parsed);
        }
    }
    let compaction = parsed.as_ref().is_some_and(|parsed| {
        compact::is_compaction(parsed, &config.compact_patterns)
            || state.compact_override.take()
    });
    if compaction {
        tracing::info!("compacting");
    }

    match route::route(&config, &bytes) {
        route::Backend::Anthropic => passthrough::send(&state, &config, parts, bytes).await,
        route::Backend::Provider { provider, real_model } => {
            let call = providers::Call { counting, uid, compaction };
            providers::dispatch(&state, &config, provider, bytes, real_model, call).await
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
