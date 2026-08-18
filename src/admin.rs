//! Loopback admin API the TUI client uses to configure the running daemon.
//! Lives under `/__router/` so it can never collide with an Anthropic API
//! path (which the fallback would forward upstream). Keys transit only over
//! loopback and are stored via the daemon's own credential store; previews
//! are always masked.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::credentials::Source;
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/__router/providers", get(providers))
        .route("/__router/credentials/{provider}", put(set_credential).delete(clear_credential))
}

async fn providers(State(state): State<AppState>) -> Json<Value> {
    let providers: Vec<Value> = state
        .config
        .providers
        .iter()
        .map(|provider| {
            let source = state.credentials.source(&provider.name);
            json!({
                "name": provider.name,
                "base_url": provider.base_url,
                "models": provider.models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                "credential": {
                    "set": !matches!(source, Source::Unset),
                    "source": source.label(),
                    "preview": state.credentials.preview(&provider.name),
                    "can_clear": matches!(source, Source::File),
                },
            })
        })
        .collect();
    Json(json!({
        "listen": state.listen.to_string(),
        "config_path": state.config.config_path.as_ref().map(|p| p.display().to_string()),
        "providers": providers,
    }))
}

#[derive(Deserialize)]
struct SetKey {
    key: String,
}

async fn set_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<SetKey>,
) -> Response {
    if !known(&state, &provider) {
        return (StatusCode::NOT_FOUND, format!("unknown provider '{provider}'")).into_response();
    }
    let key = body.key.trim();
    if key.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty key").into_response();
    }
    match state.credentials.set(&provider, key) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to store credential: {err}"))
                .into_response()
        }
    }
}

async fn clear_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Response {
    if !known(&state, &provider) {
        return (StatusCode::NOT_FOUND, format!("unknown provider '{provider}'")).into_response();
    }
    match state.credentials.source(&provider) {
        // Idempotent: clearing an unset credential succeeds.
        Source::File | Source::Unset => match state.credentials.clear(&provider) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to clear credential: {err}"))
                    .into_response()
            }
        },
        source => (
            StatusCode::CONFLICT,
            format!("credential comes from the {}; it cannot be cleared here", source.label()),
        )
            .into_response(),
    }
}

fn known(state: &AppState, provider: &str) -> bool {
    state.config.providers.iter().any(|p| p.name == provider)
}
