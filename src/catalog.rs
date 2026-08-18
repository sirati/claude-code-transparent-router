use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::response::Response;
use serde_json::{json, Value};

use crate::config::Config;
use crate::route::{GATEWAY_PREFIX, LARGE_CONTEXT_MARKER};
use crate::{passthrough, AppState};

/// GET /v1/models: Anthropic's catalog with the configured providers' models
/// spliced in, named `claude-routed-<provider>/<model>`.
///
/// Claude Code's gateway discovery keeps only ids that start with `claude`, so
/// each routed model carries the `claude-routed-` prefix; `route::resolve`
/// strips it back off on the way in. A model at or over 1M context also gets
/// the `[1m]` marker Claude Code reads as "assume a 1M window", since that is
/// the only per-model way to declare a window.
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
        data.extend(gateway_rows(&config));
    }

    let merged = catalog.to_string();
    parts.headers.remove("content-length");
    Response::from_parts(parts, Body::from(merged))
}

/// The configured models as gateway rows: `claude-routed-<provider>/<model>`
/// ids, with the `[1m]` marker on large windows. Shared by the `/v1/models`
/// merge above and the `/__router/gateway-models` cache the wrapper writes for
/// OAuth sessions, where live discovery never runs.
pub fn gateway_rows(config: &Config) -> Vec<Value> {
    let mut rows = Vec::new();
    for provider in &config.providers {
        for model in &provider.models {
            let display_name = model
                .display_name
                .clone()
                .unwrap_or_else(|| format!("{} (via {})", model.id, provider.name));
            let mut id = format!("{GATEWAY_PREFIX}{}/{}", provider.name, model.id);
            // The plain id would have Claude Code assume its 200k default, so
            // a model large enough to need the marker is advertised only with
            // it.
            if model.has_large_context() {
                id.push_str(LARGE_CONTEXT_MARKER);
            }
            let mut entry = json!({
                "type": "model",
                "id": id,
                "display_name": display_name,
                "created_at": "2026-01-01T00:00:00Z",
            });
            // Advertised for anything that reads the catalog. Claude Code is
            // not one of them: its discovery reads only `id` and `display_name`.
            if let Some(window) = model.context_window {
                entry["context_window"] = json!(window);
            }
            if let Some(output) = model.max_output_tokens {
                entry["max_output_tokens"] = json!(output);
            }
            rows.push(entry);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{tests::fixture, Config};
    use crate::route;

    #[test]
    fn gateway_rows_prefix_and_round_trip() {
        let config = Config::load(Some(fixture("providers.toml"))).unwrap();
        let rows = gateway_rows(&config);
        assert!(!rows.is_empty());
        for row in &rows {
            let id = row["id"].as_str().expect("gateway id is a string");
            // Discovery keeps only ids beginning with claude/anthropic.
            assert!(id.starts_with(GATEWAY_PREFIX), "id must satisfy discovery: {id}");
            // The id must route back to the provider it advertises.
            assert!(
                matches!(route::resolve(&config, id), route::Backend::Provider { .. }),
                "gateway id did not round-trip: {id}"
            );
        }
    }
}
