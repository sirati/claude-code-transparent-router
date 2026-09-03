//! Anthropic Messages in, Anthropic Messages out, an OpenAI-compatible
//! chat-completions API in the middle.

pub mod request;
pub mod response;
pub mod stream;

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::credentials::SecretKey;
use crate::providers::{json_response, provider_error};

pub async fn messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    key: SecretKey,
    body: Bytes,
    real_model: String,
) -> Response {
    let anthropic_req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(err) => return crate::passthrough::proxy_error(&format!("request body is not valid JSON: {err}")),
    };
    let mut anthropic_req = anthropic_req;
    // Harness instructions lead the system prompt; the client never sees them.
    crate::system_prompt::prepend(provider.system_prompt.as_deref(), &mut anthropic_req);
    // The CLI gets back the model name it asked for (the alias), never the
    // provider-internal ID.
    let alias = anthropic_req["model"].as_str().unwrap_or(&real_model).to_string();
    let streaming = anthropic_req["stream"].as_bool().unwrap_or(false);
    let mut openai_req = request::to_openai(&anthropic_req, &real_model, streaming);
    if let Some(level) =
        crate::effort::apply(provider.effort.as_ref(), &anthropic_req, &mut openai_req)
    {
        tracing::debug!(provider = provider.name, effort = level, "effort mapped");
    }

    // Fresh header map, never the inbound one: the Anthropic credential
    // cannot reach this provider.
    let sent = client
        .post(format!("{}/chat/completions", provider.base_url))
        .header("authorization", format!("Bearer {}", key.expose()))
        .header("content-type", "application/json")
        .header("accept", if streaming { "text/event-stream" } else { "application/json" })
        .body(openai_req.to_string())
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
            Ok(openai) => json_response(StatusCode::OK, response::to_anthropic(&openai, &alias)),
            Err(err) => crate::passthrough::proxy_error(&format!(
                "provider '{}' returned invalid JSON: {err}",
                provider.name
            )),
        }
    }
}
