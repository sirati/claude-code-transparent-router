//! Second-provider paths. Only reachable for `anthropic/<model>` aliases.
//! Inbound headers are never passed into this module: request signatures take
//! body bytes and the model only, so the Anthropic credential cannot leak
//! here by construction.

pub mod anthropic_compat;
pub mod openai_compat;

use axum::body::Bytes;
use axum::response::Response;

use crate::config::ApiFormat;
use crate::{passthrough, AppState};

pub async fn dispatch(
    state: &AppState,
    provider: usize,
    body: Bytes,
    real_model: String,
    counting: bool,
) -> Response {
    let provider = &state.config.providers[provider];
    if counting {
        return count_tokens(&body);
    }
    let Some(key) = state.credentials.get(&provider.name) else {
        return passthrough::proxy_error(&format!(
            "provider '{name}' is configured but has no credentials set; \
             run claude-router in a terminal to set one, or supply the systemd \
             credential '{name}' / the {env}_API_KEY environment variable",
            name = provider.name,
            env = provider.name.to_uppercase().replace('-', "_"),
        ));
    };
    tracing::info!(provider = provider.name, model = real_model, api = ?provider.api, "routing");
    match provider.api {
        ApiFormat::Openai => {
            openai_compat::messages(&state.client, provider, key, body, real_model).await
        }
        ApiFormat::Anthropic => {
            anthropic_compat::messages(&state.client, provider, key, body, real_model).await
        }
    }
}

/// Token counting for provider models: a coarse local estimate (these
/// providers have no count endpoint). Good enough for context budgeting.
fn count_tokens(body: &Bytes) -> Response {
    let estimate = (body.len() / 4).max(1);
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header(passthrough::PROXY_ORIGIN_HEADER, passthrough::PROXY_ORIGIN_VALUE)
        .body(axum::body::Body::from(format!("{{\"input_tokens\":{estimate}}}")))
        .expect("count_tokens response")
}
