//! The admin API the TUI client drives, tested against the real router with
//! the same TOML fixture a deployment would use.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use claude_code_transparent_router::config::Config;
use claude_code_transparent_router::credentials::CredentialStore;
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
        credentials: Arc::new(CredentialStore::new(config.credentials_dir.clone())),
        config: Arc::new(config),
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
