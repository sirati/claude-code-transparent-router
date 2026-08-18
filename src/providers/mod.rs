//! Second-provider paths. Only reachable for `anthropic/<model>` aliases.
//! Inbound headers are never passed into these modules: every provider
//! request is built from a [`ProviderAuth`] constructed here, so the
//! Anthropic credential cannot leak to a provider by construction.

pub mod anthropic_compat;
pub mod openai_compat;
pub mod responses;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::Value;

use crate::config::ApiFormat;
use crate::credentials::SecretKey;
use crate::passthrough::{proxy_error, PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE};
use crate::AppState;

/// Authentication material for one provider request. Built fresh per request
/// from the credential store; never derived from the inbound header map.
pub struct ProviderAuth {
    headers: Vec<(String, String)>,
}

impl ProviderAuth {
    /// Bearer-token auth, the common case for API keys.
    pub fn bearer(key: &SecretKey) -> Self {
        Self::bearer_token(key.expose())
    }

    /// API-key auth for the Anthropic dialect: both `x-api-key` and the
    /// bearer form, because Anthropic-compatible endpoints differ in which
    /// one they read.
    pub fn api_key(key: &SecretKey) -> Self {
        Self::bearer(key).with("x-api-key", key.expose())
    }

    pub fn bearer_token(token: &str) -> Self {
        Self { headers: vec![("authorization".into(), format!("Bearer {token}"))] }
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn into_headers(self) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in self.headers {
            if let (Ok(name), Ok(value)) =
                (HeaderName::try_from(name), HeaderValue::try_from(value))
            {
                map.append(name, value);
            }
        }
        map
    }
}

/// What this particular call needs to know beyond the request body.
#[derive(Clone, Copy, Default)]
pub struct Call {
    /// A token-count request rather than a turn.
    pub counting: bool,
    /// Whose credentials to use; the kernel's answer for the connection.
    pub uid: Option<u32>,
    /// Claude Code is compacting the conversation.
    pub compaction: bool,
}

pub async fn dispatch(
    state: &AppState,
    config: &crate::config::Config,
    provider: usize,
    body: Bytes,
    real_model: String,
    call: Call,
) -> Response {
    let (counting, uid, compaction) = (call.counting, call.uid, call.compaction);
    let provider = &config.providers[provider];
    if counting {
        return count_tokens(&body);
    }
    tracing::info!(provider = provider.name, model = real_model, api = ?provider.api, "routing");

    // OAuth providers authenticate with a stored token; everything else with
    // an API key. Either way the header map is built here, from credential
    // material only.
    if provider.oauth.is_some() {
        return match oauth_auth(state, provider, uid).await {
            Ok(auth) => forward(state, provider, auth, body, real_model, compaction).await,
            Err(err) => proxy_error(&err),
        };
    }

    let Some(key) = state.credentials(uid).get(&provider.name) else {
        return proxy_error(&format!(
            "provider '{name}' is configured but has no credentials set; \
             run claude-router in a terminal to set one, or supply the systemd \
             credential '{name}' / the {env}_API_KEY environment variable",
            name = provider.name,
            env = provider.name.to_uppercase().replace('-', "_"),
        ));
    };
    match provider.api {
        ApiFormat::Openai => {
            openai_compat::messages(&state.client, provider, key, body, real_model).await
        }
        ApiFormat::Anthropic => {
            anthropic_compat::messages(
                &state.client,
                provider,
                ProviderAuth::api_key(&key),
                body,
                real_model,
            )
            .await
        }
        ApiFormat::Responses => {
            forward(state, provider, ProviderAuth::bearer(&key), body, real_model, compaction)
                .await
        }
    }
}

async fn forward(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
    auth: ProviderAuth,
    body: Bytes,
    real_model: String,
    compaction: bool,
) -> Response {
    let auth = provider
        .headers
        .iter()
        .fold(auth, |auth, (name, value)| auth.with(name, value.clone()));
    match provider.api {
        ApiFormat::Responses => {
            responses::messages(&state.client, provider, auth, body, real_model, compaction).await
        }
        ApiFormat::Anthropic => {
            anthropic_compat::messages(&state.client, provider, auth, body, real_model).await
        }
        // OAuth against the openai dialect is not wired up; no configured
        // provider needs it yet, and guessing the header shape would be worse
        // than saying so.
        other => proxy_error(&format!(
            "provider '{}' uses OAuth with api = {other:?}, which this router does not support yet",
            provider.name
        )),
    }
}

/// Load the provider's tokens, refreshing when they are close to expiry, and
/// build the auth headers the provider expects.
async fn oauth_auth(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
    uid: Option<u32>,
) -> Result<ProviderAuth, String> {
    let config = provider.oauth.as_ref().expect("oauth provider");
    let store = state.tokens(uid);
    let mut tokens = store.get(&provider.name).ok_or_else(|| {
        format!(
            "provider '{}' is not signed in; run `claude-router login {}`",
            provider.name, provider.name
        )
    })?;

    if tokens.needs_refresh(crate::oauth::REFRESH_WINDOW) {
        tracing::info!(provider = provider.name, "refreshing access token");
        let refreshed = crate::oauth::refresh(&state.client, config, &tokens.refresh_token)
            .await
            .map_err(|err| {
                format!(
                    "could not refresh the '{}' login: {err}; run `claude-router login {}`",
                    provider.name, provider.name
                )
            })?;
        // Keep the account id from login when a refresh response omits it.
        tokens = crate::oauth::Tokens {
            account_id: refreshed.account_id.or(tokens.account_id),
            ..refreshed
        };
        if let Err(err) = store.save(&provider.name, &tokens) {
            tracing::warn!(provider = provider.name, %err, "could not persist refreshed tokens");
        }
    }

    let mut auth = ProviderAuth::bearer_token(&tokens.access_token);
    if let (Some(header), Some(account_id)) = (&config.account_header, &tokens.account_id) {
        auth = auth.with(header, account_id.clone());
    }
    Ok(auth)
}

/// Provider-side errors are re-shaped into Anthropic's error envelope (the
/// CLI knows how to display those) with the upstream status preserved and the
/// provider's own message quoted.
pub fn provider_error(status: StatusCode, provider: &str, detail: &str) -> Response {
    let message = format!("provider '{provider}' returned {status}: {}", detail.trim());
    json_response(
        status,
        serde_json::json!({
            "type": "error",
            "error": {"type": error_type(status), "message": message},
        }),
    )
}

fn error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        529 => "overloaded_error",
        _ => "api_error",
    }
}

pub fn json_response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header(PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE)
        .body(Body::from(body.to_string()))
        .expect("provider json response")
}

/// Token counting for provider models: a coarse local estimate (these
/// providers have no count endpoint). Good enough for context budgeting.
fn count_tokens(body: &Bytes) -> Response {
    let estimate = (body.len() / 4).max(1);
    json_response(StatusCode::OK, serde_json::json!({"input_tokens": estimate}))
}
