use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::response::Response;
use serde_json::{json, Value};

use crate::route::ALIAS_PREFIX;
use crate::{passthrough, AppState};

/// GET /v1/models: Anthropic's catalog with the second provider's models
/// spliced in. Claude Code's model picker drops IDs that don't start with
/// `claude` or `anthropic`, hence the `anthropic/<id>` aliases.
pub async fn models(State(state): State<AppState>, req: Request) -> Response {
    let (mut parts, body) = req.into_parts();
    let bytes = match to_bytes(body, 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read request body: {err}")),
    };
    if !state.config.providers.is_empty() {
        // Owned route: request an identity body so the catalog can be parsed
        // and merged. Pure passthrough (no providers) stays verbatim.
        parts.headers.remove("accept-encoding");
    }
    let upstream = passthrough::send(&state, parts, bytes).await;

    if state.config.providers.is_empty() || !upstream.status().is_success() {
        return upstream;
    }

    // The owned route is the one place a body is reserialized.
    let (mut parts, body) = upstream.into_parts();
    let bytes = match to_bytes(body, 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read catalog: {err}")),
    };
    let Ok(mut catalog) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    // Append to the final page only, so paginated fetches see each entry once.
    let last_page = catalog["has_more"] == json!(false);
    if let (true, Some(data)) = (last_page, catalog["data"].as_array_mut()) {
        for provider in &state.config.providers {
            for model in &provider.models {
                data.push(json!({
                    "type": "model",
                    "id": format!("{ALIAS_PREFIX}{model}"),
                    "display_name": format!("{model} (via {})", provider.name),
                    "created_at": "2026-01-01T00:00:00Z",
                }));
            }
        }
    }

    let merged = catalog.to_string();
    parts.headers.remove("content-length");
    Response::from_parts(parts, Body::from(merged))
}
