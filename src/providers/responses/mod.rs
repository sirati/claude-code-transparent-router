//! Anthropic Messages in, Anthropic Messages out, an OpenAI Responses API in
//! the middle. Used by providers whose tool calling requires `/v1/responses`
//! rather than chat-completions.

pub mod request;
pub mod response;
pub mod stream;

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::providers::{json_response, provider_error, ProviderAuth};

pub async fn messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth: ProviderAuth,
    body: Bytes,
    real_model: String,
) -> Response {
    let anthropic_req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(err) => {
            return crate::passthrough::proxy_error(&format!(
                "request body is not valid JSON: {err}"
            ))
        }
    };
    // The CLI gets back the alias it asked for, never the provider's own ID.
    let alias = anthropic_req["model"].as_str().unwrap_or(&real_model).to_string();
    let streaming = anthropic_req["stream"].as_bool().unwrap_or(false);
    let mut outgoing = request::to_responses(&anthropic_req, &real_model, streaming);
    // Provider-specific body knobs come from config, not from this module.
    for (key, value) in &provider.request_extra {
        if let Ok(value) = serde_json::to_value(value) {
            outgoing[key.as_str()] = value;
        }
    }
    if let Some(level) =
        crate::effort::apply(provider.effort.as_ref(), &anthropic_req, &mut outgoing)
    {
        tracing::debug!(provider = provider.name, effort = level, "effort mapped");
    }

    // Fresh header map from the auth material only: nothing inbound, so the
    // Anthropic credential cannot reach this provider.
    let sent = client
        .post(format!("{}/responses", provider.base_url))
        .headers(auth.into_headers())
        .header("content-type", "application/json")
        .header("accept", if streaming { "text/event-stream" } else { "application/json" })
        .body(outgoing.to_string())
        .send()
        .await;

    let upstream = match sent {
        Ok(upstream) => upstream,
        Err(err) => {
            return crate::passthrough::proxy_error(&format!(
                "provider '{}' request failed: {err}",
                provider.name
            ))
        }
    };

    let status = upstream.status();
    if !status.is_success() {
        let detail = upstream.text().await.unwrap_or_default();
        return provider_error(status, &provider.name, &detail);
    }

    if streaming {
        stream::response(upstream, alias)
    } else {
        let bytes = match upstream.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                return crate::passthrough::proxy_error(&format!(
                    "provider '{}' response read failed: {err}",
                    provider.name
                ))
            }
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(parsed) => json_response(StatusCode::OK, response::to_anthropic(&parsed, &alias)),
            Err(err) => crate::passthrough::proxy_error(&format!(
                "provider '{}' returned invalid JSON: {err}",
                provider.name
            )),
        }
    }
}
