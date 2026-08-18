use axum::body::{Body, Bytes};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;

use crate::headers;
use crate::AppState;

/// Marks responses that originate in this proxy rather than upstream, so the
/// two are distinguishable in a transcript.
pub const PROXY_ORIGIN_HEADER: &str = "x-proxy-origin";
pub const PROXY_ORIGIN_VALUE: &str = "claude-code-transparent-router";

/// Forward the request to Anthropic verbatim: original method, path, query,
/// end-to-end headers, and the exact body bytes received. The response comes
/// back the same way, with the body streamed chunk-by-chunk untouched.
pub async fn send(state: &AppState, parts: Parts, body: Bytes) -> Response {
    let path_and_query =
        parts.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let url = format!("{}{}", state.config.anthropic_base, path_and_query);

    let upstream = state
        .client
        .request(parts.method, url)
        .headers(headers::end_to_end(&parts.headers).into_inner())
        .body(body)
        .send()
        .await;

    let upstream = match upstream {
        Ok(upstream) => upstream,
        Err(err) => return proxy_error(&format!("upstream request failed: {err}")),
    };

    let status = upstream.status();
    let response_headers = headers::response_headers(upstream.headers());

    let mut response = Response::builder()
        .status(status)
        // No buffering, no re-framing: SSE chunks flow through as they arrive.
        .body(Body::from_stream(upstream.bytes_stream()))
        .expect("passthrough response");
    *response.headers_mut() = response_headers;
    response
}

/// A failure of *this proxy* (never an upstream error, which is forwarded
/// verbatim): 502 with an Anthropic-shaped error body clearly marked as ours.
pub fn proxy_error(message: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": format!("[{PROXY_ORIGIN_VALUE}] {message}"),
        },
    });
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header("content-type", "application/json")
        .header(PROXY_ORIGIN_HEADER, HeaderValue::from_static(PROXY_ORIGIN_VALUE))
        .body(Body::from(body.to_string()))
        .expect("proxy error response")
}
