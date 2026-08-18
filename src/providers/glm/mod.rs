//! Second-provider path. Only reachable for `anthropic/<model>` aliases, and
//! only when a GLM credential is configured. Inbound headers are never passed
//! into this module: request signatures take body bytes and the model only, so
//! the Anthropic credential cannot leak here by construction.

use axum::body::Bytes;
use axum::response::Response;

use crate::{passthrough, AppState};

/// Translation path stub: full Messages -> chat/completions translation lands
/// next; until then aliased models get a clearly proxy-marked error.
pub async fn messages(_state: &AppState, _body: Bytes, real_model: String) -> Response {
    passthrough::proxy_error(&format!(
        "second-provider translation for '{real_model}' is not implemented yet"
    ))
}

/// Token counting for provider models: a coarse local estimate (the provider
/// has no count endpoint). Marked coarse in the README; good enough for the
/// CLI's context budgeting.
pub fn count_tokens(body: &Bytes) -> Response {
    let estimate = (body.len() / 4).max(1);
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header(passthrough::PROXY_ORIGIN_HEADER, passthrough::PROXY_ORIGIN_VALUE)
        .body(axum::body::Body::from(format!("{{\"input_tokens\":{estimate}}}")))
        .expect("count_tokens response")
}
