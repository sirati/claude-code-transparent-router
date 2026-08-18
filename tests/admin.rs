//! The admin API the TUI client drives, tested against the real router with
//! the same TOML fixture a deployment would use.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use claude_code_transparent_router::config::Config;
use claude_code_transparent_router::{app, AppState};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

fn test_app(credentials_dir: &std::path::Path) -> axum::Router {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/providers.toml");
    let mut config = Config::load(Some(fixture)).unwrap();
    config.credentials_dir = credentials_dir.to_path_buf();
    app(AppState {
        client: reqwest::Client::new(),
        config: Arc::new(config),
        user_configs: None,
        listen: "127.0.0.1:9999".parse().unwrap(),
    })
}

async fn providers_json(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(Request::get("/__router/providers").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn credential_lifecycle_via_admin_api() {
    let dir = std::env::temp_dir().join(format!("admin-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let app = test_app(&dir);

    let status = providers_json(&app).await;
    assert_eq!(status["providers"].as_array().unwrap().len(), 2);
    assert_eq!(status["providers"][0]["name"], "alpha");
    assert_eq!(status["providers"][0]["credential"]["set"], false);

    // set: stored masked, readable back through status
    let response = app
        .clone()
        .oneshot(
            Request::put("/__router/credentials/alpha")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"key":"sk-test-abcdef123456"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let status = providers_json(&app).await;
    let credential = &status["providers"][0]["credential"];
    assert_eq!(credential["set"], true);
    assert_eq!(credential["can_clear"], true);
    let preview = credential["preview"].as_str().unwrap();
    assert!(preview.starts_with("sk-test-") && preview.ends_with("****"), "{preview}");
    assert!(!preview.contains("abcdef123456"));

    // clear
    let response = app
        .clone()
        .oneshot(Request::delete("/__router/credentials/alpha").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let status = providers_json(&app).await;
    assert_eq!(status["providers"][0]["credential"]["set"], false);

    // unknown provider
    let response = app
        .clone()
        .oneshot(Request::delete("/__router/credentials/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn picker_model_leads_the_row_list() {
    let dir = std::env::temp_dir().join(format!("picker-test-{}", std::process::id()));
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/providers.toml");
    let mut config = Config::load(Some(fixture)).unwrap();
    config.credentials_dir = dir.clone();
    // Second provider's model, so leading it proves the choice is honoured.
    config.picker_model = Some("beta-model".into());
    let app = app(AppState {
        client: reqwest::Client::new(),
        config: Arc::new(config),
        user_configs: None,
        listen: "127.0.0.1:9999".parse().unwrap(),
    });

    let response = app
        .oneshot(Request::get("/__router/picker").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    // Rows carry the readable provider/shorthand form, with the context
    // marker this model's 1M window earns it.
    let first = body.lines().next().unwrap();
    assert_eq!(first, "beta/beta-pro[1m]\tBeta Model Pro");
    // Every model still appears; only the order changes.
    assert_eq!(body.lines().count(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn per_user_mode_resolves_each_uid_to_its_own_home() {
    let base = std::env::temp_dir().join(format!("multiuser-{}", std::process::id()));
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/providers.toml");
    let mut config = Config::load(Some(fixture)).unwrap();
    config.credentials_dir = base.clone();
    let config = Arc::new(config);
    let state = AppState {
        client: reqwest::Client::new(),
        user_configs: Some(Arc::new(
            claude_code_transparent_router::user_config::UserConfigs::new(config.clone()),
        )),
        config,
        listen: "127.0.0.1:9999".parse().unwrap(),
    };

    // A real uid resolves to that user's own state directory.
    let own = claude_code_transparent_router::peer::own_uid().unwrap();
    let home = claude_code_transparent_router::user_config::home_dir(own).unwrap();
    assert_eq!(state.state_dir(Some(own)), home.join(".local/state/claude-router/credentials"));

    // A uid with no passwd entry, and an unidentified caller, each get an
    // isolated directory rather than somebody else's keys.
    assert!(state.state_dir(Some(999_999)).starts_with(&base));
    assert_ne!(state.state_dir(Some(999_999)), state.state_dir(Some(own)));
    assert_ne!(state.state_dir(None), state.state_dir(Some(own)));
}

#[test]
fn single_user_mode_uses_one_store_for_everyone() {
    let base = std::env::temp_dir().join(format!("singleuser-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/providers.toml");
    let mut config = Config::load(Some(fixture)).unwrap();
    config.credentials_dir = base.clone();
    let state = AppState {
        client: reqwest::Client::new(),
        config: Arc::new(config),
        user_configs: None,
        listen: "127.0.0.1:9999".parse().unwrap(),
    };

    state.credentials(Some(1000)).set("alpha", "sk-shared").unwrap();
    assert_eq!(state.credentials(None).get("alpha").unwrap().expose(), "sk-shared");
    assert_eq!(state.credentials(Some(1001)).get("alpha").unwrap().expose(), "sk-shared");

    let _ = std::fs::remove_dir_all(&base);
}
