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

use crate::config::{CompactionConfig, ProviderConfig};
use crate::providers::{json_response, provider_error, ProviderAuth};

/// Applies the provider's body knobs, then the compaction protocol's own on
/// top: extras merged, removals applied, and the trigger item appended last so
/// it stays the final input item.
pub fn shape_body(
    provider: &ProviderConfig,
    compaction: Option<&CompactionConfig>,
    outgoing: &mut Value,
) {
    let extra = provider
        .request_extra
        .iter()
        .chain(compaction.iter().flat_map(|c| c.request_extra.iter()));
    for (key, value) in extra {
        if let Ok(value) = serde_json::to_value(value) {
            outgoing[key.as_str()] = value;
        }
    }
    if let Some(object) = outgoing.as_object_mut() {
        let remove = provider
            .request_remove
            .iter()
            .chain(compaction.iter().flat_map(|c| c.request_remove.iter()));
        for key in remove {
            object.remove(key);
        }
    }
    // Codex marks a compaction with a trailing control item rather than a
    // prompt: the instruction itself lives on the server.
    if let Some(item) = compaction.and_then(|c| c.trigger_item.as_deref()) {
        if let Some(input) = outgoing["input"].as_array_mut() {
            input.push(serde_json::json!({"type": item}));
        }
    }
}

pub async fn messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth: ProviderAuth,
    body: Bytes,
    real_model: String,
    compaction: bool,
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
    // A provider's own compaction protocol only applies to a request the
    // router recognised as one; otherwise a compaction is an ordinary turn
    // carrying Claude Code's summarisation instruction as its last message.
    let compaction = compaction.then_some(provider.compaction.as_ref()).flatten();
    if compaction.is_some() {
        tracing::info!(provider = provider.name, "compacting via the provider's own protocol");
    }

    let mut outgoing = request::to_responses(&anthropic_req, &real_model, streaming);
    shape_body(provider, compaction, &mut outgoing);
    if let Some(level) =
        crate::effort::apply(provider.effort.as_ref(), &anthropic_req, &mut outgoing)
    {
        tracing::debug!(provider = provider.name, effort = level, "effort mapped");
    }

    // A provider may insist on streaming whatever the client asked for, in
    // which case the reply is assembled below rather than passed through.
    let upstream_streams = outgoing["stream"].as_bool().unwrap_or(streaming);

    // Fresh header map from the auth material only: nothing inbound, so the
    // Anthropic credential cannot reach this provider.
    let path = compaction
        .and_then(|c| c.path.as_deref())
        .unwrap_or("responses");
    let sent = client
        .post(format!("{}/{path}", provider.base_url))
        .headers(auth.into_headers())
        .header("content-type", "application/json")
        .header("accept", if upstream_streams { "text/event-stream" } else { "application/json" })
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
        let body = String::from_utf8_lossy(&bytes);
        let message = match serde_json::from_str::<Value>(&body) {
            Ok(parsed) => Some(response::to_anthropic(&parsed, &alias)),
            Err(_) => response::from_event_stream(&body, &alias),
        };
        match message {
            Some(message) => json_response(StatusCode::OK, message),
            None => crate::passthrough::proxy_error(&format!(
                "provider '{}' returned neither JSON nor a readable event stream",
                provider.name
            )),
        }
    }
}
