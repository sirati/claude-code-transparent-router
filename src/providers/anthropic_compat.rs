//! Provider that speaks the Anthropic Messages API natively. Near-passthrough:
//! the request's `model` is rewritten to the provider's real ID and the
//! credential swapped for the provider's own; the response — including SSE —
//! streams back verbatim, so nothing is lost in translation.

use axum::body::{Body, Bytes};
use axum::http::HeaderValue;
use axum::response::Response;
use serde_json::{json, Value};

use super::ProviderAuth;
use crate::config::ProviderConfig;
use crate::headers;
use crate::passthrough::proxy_error;

pub async fn messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth: ProviderAuth,
    body: Bytes,
    real_model: String,
) -> Response {
    let mut request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(err) => return proxy_error(&format!("request body is not valid JSON: {err}")),
    };
    request["model"] = json!(real_model);
    let source = request.clone();
    if let Some(level) = crate::effort::apply(provider.effort.as_ref(), &source, &mut request) {
        tracing::debug!(provider = provider.name, effort = level, "effort mapped");
    }

    // Fresh header map, never the inbound one: the Anthropic credential
    // cannot reach this provider. Auth is the caller's (an API key sends both
    // conventions; OAuth sends only the bearer form plus its beta header).
    let mut auth_headers = auth.into_headers();
    auth_headers.insert("content-type", HeaderValue::from_static("application/json"));
    auth_headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    let sent = client
        .post(format!("{}/v1/messages", provider.base_url))
        .headers(auth_headers)
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
