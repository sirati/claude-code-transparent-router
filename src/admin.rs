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
        .route("/__router/picker", get(picker))
        .route("/__router/credentials/{provider}", put(set_credential).delete(clear_credential))
        .route("/__router/oauth/{provider}", put(set_tokens))
}

/// Hand a completed login to the daemon. The browser flow has to run in the
/// user's session, but the daemon may be a different user with its own state
/// directory (systemd `DynamicUser`), so the CLI posts the result here rather
/// than writing to a store the daemon never reads.
async fn set_tokens(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(tokens): Json<crate::oauth::Tokens>,
) -> Response {
    if !is_oauth(&state, &provider) {
        return (StatusCode::NOT_FOUND, format!("no OAuth provider named '{provider}'"))
            .into_response();
    }
    match state.tokens.save(&provider, &tokens) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to store login: {err}"))
            .into_response(),
    }
}

/// Plain-text picker rows (`<alias>\t<display name>` per line) for shell
/// consumers like the claude-routed wrapper, which turns the first line into
/// ANTHROPIC_CUSTOM_MODEL_OPTION. Claude Code's gateway model discovery only
/// runs with API-key auth, so OAuth sessions need this route instead.
async fn picker(State(state): State<AppState>) -> String {
    let mut out = String::new();
    for provider in &state.config.providers {
        for model in &provider.models {
            let display = model
                .display_name
                .clone()
                .unwrap_or_else(|| format!("{} (via {})", model.id, provider.name));
            out.push_str(&format!("{}{}\t{display}\n", crate::route::ALIAS_PREFIX, model.id));
        }
    }
    out
}

async fn providers(State(state): State<AppState>) -> Json<Value> {
    let providers: Vec<Value> = state
        .config
        .providers
        .iter()
        .map(|provider| {
            // OAuth providers are described by their stored session; API-key
            // providers by the credential store.
            let credential = match provider.oauth.is_some() {
                true => match state.tokens.get(&provider.name) {
                    Some(tokens) => json!({
                        "set": true,
                        "source": "login",
                        "preview": tokens.preview(),
                        "can_clear": true,
                    }),
                    None => json!({
                        "set": false,
                        "source": format!("not signed in - run: claude-router login {}", provider.name),
                        "preview": Value::Null,
                        "can_clear": false,
                    }),
                },
                false => {
                    let source = state.credentials.source(&provider.name);
                    json!({
                        "set": !matches!(source, Source::Unset),
                        "source": source.label(),
                        "preview": state.credentials.preview(&provider.name),
                        "can_clear": matches!(source, Source::File),
                    })
                }
            };
            json!({
                "name": provider.name,
                "base_url": provider.base_url,
                "models": provider.models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                "oauth": provider.oauth.is_some(),
                "credential": credential,
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
    if is_oauth(&state, &provider) {
        return (
            StatusCode::CONFLICT,
            format!("provider '{provider}' signs in with a browser; run: claude-router login {provider}"),
        )
            .into_response();
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
    if is_oauth(&state, &provider) {
        return match state.tokens.clear(&provider) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to sign out: {err}"))
                .into_response(),
        };
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

fn is_oauth(state: &AppState, provider: &str) -> bool {
    state.config.providers.iter().any(|p| p.name == provider && p.oauth.is_some())
}
