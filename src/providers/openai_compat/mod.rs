//! Anthropic Messages in, Anthropic Messages out, an OpenAI-compatible
//! chat-completions API in the middle.

pub mod request;
pub mod response;
pub mod stream;

use axum::body::{Body, Bytes};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::credentials::SecretKey;
use crate::passthrough::{PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE};

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

/// Provider-side errors are re-shaped into Anthropic's error envelope (the
/// CLI knows how to display those) with the upstream status preserved and the
/// provider's own message quoted.
fn provider_error(status: StatusCode, provider: &str, detail: &str) -> Response {
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

fn json_response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header(PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE)
        .body(Body::from(body.to_string()))
        .expect("provider json response")
}
