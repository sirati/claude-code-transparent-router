//! Provider that speaks the Anthropic Messages API natively. Near-passthrough:
//! the request's `model` is rewritten to the provider's real ID and the
//! credential swapped for the provider's own; the response — including SSE —
//! streams back verbatim, so nothing is lost in translation.

use axum::body::{Body, Bytes};
use axum::response::Response;
use serde_json::{json, Value};

use crate::config::ProviderConfig;
use crate::credentials::SecretKey;
use crate::headers;
use crate::passthrough::proxy_error;

pub async fn messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    key: SecretKey,
    body: Bytes,
    real_model: String,
) -> Response {
    let mut request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(err) => return proxy_error(&format!("request body is not valid JSON: {err}")),
    };
    request["model"] = json!(real_model);

    // Fresh header map, never the inbound one: the Anthropic credential
    // cannot reach this provider. Both auth conventions are sent because
    // Anthropic-compatible endpoints differ in which one they read.
    let sent = client
        .post(format!("{}/v1/messages", provider.base_url))
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", key.expose())
        .header("authorization", format!("Bearer {}", key.expose()))
        .body(request.to_string())
        .send()
        .await;

    let upstream = match sent {
        Ok(upstream) => upstream,
        Err(err) => {
            return proxy_error(&format!("provider '{}' request failed: {err}", provider.name))
        }
    };

    let status = upstream.status();
    let response_headers = headers::response_headers(upstream.headers());
    let mut response = Response::builder()
        .status(status)
        // Already Anthropic-shaped: stream through chunk-by-chunk untouched.
        .body(Body::from_stream(upstream.bytes_stream()))
        .expect("provider response");
    *response.headers_mut() = response_headers;
    response
}
