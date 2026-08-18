use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::response::Response;
use serde_json::{json, Value};

use crate::{passthrough, AppState};

/// GET /v1/models: Anthropic's catalog with the configured providers' models
/// spliced in, named `<provider>/<model>`.
///
/// Claude Code's gateway discovery drops IDs that do not mention `claude` or
/// `anthropic`, so these rows reach its picker through
/// `ANTHROPIC_CUSTOM_MODEL_OPTION` — which does no such filtering — rather
/// than through discovery.
pub async fn models(
    State(state): State<AppState>,
    crate::peer::Caller(uid): crate::peer::Caller,
    req: Request,
) -> Response {
    let config = state.config_for(uid);
    let (mut parts, body) = req.into_parts();
    let bytes = match to_bytes(body, 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(err) => return passthrough::proxy_error(&format!("failed to read request body: {err}")),
    };
    if !config.providers.is_empty() {
        // Owned route: request an identity body so the catalog can be parsed
        // and merged. Pure passthrough (no providers) stays verbatim.
        parts.headers.remove("accept-encoding");
    }
    let upstream = passthrough::send(&state, &config, parts, bytes).await;

    if config.providers.is_empty() || !upstream.status().is_success() {
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
        for provider in &config.providers {
            for model in &provider.models {
                let display_name = model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| format!("{} (via {})", model.id, provider.name));
                let mut entry = json!({
                    "type": "model",
                    "id": format!("{}/{}", provider.name, model.id),
                    "display_name": display_name,
                    "created_at": "2026-01-01T00:00:00Z",
                });
                // Advertised for anything that reads the catalog. Claude Code
                // is not one of them: its discovery reads only `id` and
                // `display_name`, so the window reaches it through
                // CLAUDE_CODE_MAX_CONTEXT_TOKENS, which the wrapper sets.
                if let Some(window) = model.context_window {
                    entry["context_window"] = json!(window);
                }
                if let Some(output) = model.max_output_tokens {
                    entry["max_output_tokens"] = json!(output);
                }
                data.push(entry);
            }
        }
    }

    let merged = catalog.to_string();
    parts.headers.remove("content-length");
    Response::from_parts(parts, Body::from(merged))
}
