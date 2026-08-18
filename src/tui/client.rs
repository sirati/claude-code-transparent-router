//! Blocking HTTP client for the daemon's `/__router` admin API.

use std::net::SocketAddr;
use std::time::Duration;

use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Credential {
    pub set: bool,
    pub source: String,
    pub preview: Option<String>,
    pub can_clear: bool,
}

#[derive(Deserialize, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub credential: Credential,
}

#[derive(Deserialize, Clone)]
pub struct Status {
    pub listen: String,
    pub config_path: Option<String>,
    pub providers: Vec<Provider>,
}

pub struct Client {
    http: reqwest::blocking::Client,
    base: String,
}

impl Client {
    pub fn new(daemon: SocketAddr) -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(5))
                .build()
                .expect("admin client"),
            base: format!("http://{daemon}"),
        }
    }

    pub fn status(&self) -> Result<Status, String> {
        self.http
            .get(format!("{}/__router/providers", self.base))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
            .map_err(friendly)
    }

    pub fn set_credential(&self, provider: &str, key: &str) -> Result<(), String> {
        check(
            self.http
                .put(format!("{}/__router/credentials/{provider}", self.base))
                .json(&serde_json::json!({"key": key}))
                .send(),
        )
    }

    pub fn clear_credential(&self, provider: &str) -> Result<(), String> {
        check(self.http.delete(format!("{}/__router/credentials/{provider}", self.base)).send())
    }
}

fn check(result: reqwest::Result<reqwest::blocking::Response>) -> Result<(), String> {
    match result {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => {
            let status = response.status();
            Err(response.text().unwrap_or_else(|_| status.to_string()))
        }
        Err(err) => Err(friendly(err)),
    }
}

fn friendly(err: reqwest::Error) -> String {
    if err.is_connect() || err.is_timeout() {
        "daemon not reachable — start it with `claude-router --daemon` or via systemd".into()
    } else {
        err.to_string()
    }
}
