use std::fmt;
use std::net::SocketAddr;

pub struct Config {
    pub listen: SocketAddr,
    /// Base URL requests are forwarded to verbatim, no trailing slash.
    pub anthropic_base: String,
    pub provider: Option<ProviderConfig>,
}

pub struct ProviderConfig {
    /// OpenAI-compatible chat-completions base URL, no trailing slash.
    pub base_url: String,
    pub key: SecretKey,
    /// Real upstream model IDs, served in /v1/models as `anthropic/<id>`.
    pub models: Vec<String>,
}

/// API key for the second provider. Never printed, never merged into
/// forwarded header maps — `providers::glm` builds its headers from scratch.
pub struct SecretKey(String);

impl SecretKey {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

impl Config {
    pub fn from_env() -> Self {
        let listen = std::env::var("CLAUDE_ROUTER_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8787".into())
            .parse()
            .expect("CLAUDE_ROUTER_LISTEN must be a socket address");

        let anthropic_base = trim_slash(
            std::env::var("ANTHROPIC_UPSTREAM_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
        );

        Self { listen, anthropic_base, provider: provider_from_env() }
    }
}

fn provider_from_env() -> Option<ProviderConfig> {
    let key = match load_key() {
        Some(key) => key,
        None => {
            tracing::info!("no GLM credential found; second-provider routing disabled");
            return None;
        }
    };

    let base_url = trim_slash(
        std::env::var("GLM_BASE_URL").unwrap_or_else(|_| "https://api.z.ai/api/paas/v4".into()),
    );
    let models = std::env::var("GLM_MODELS")
        .unwrap_or_else(|_| "glm-4.7".into())
        .split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();

    Some(ProviderConfig { base_url, key, models })
}

/// systemd `LoadCredential=glm:...` first, `GLM_API_KEY` as the dev fallback.
fn load_key() -> Option<SecretKey> {
    if let Ok(dir) = std::env::var("CREDENTIALS_DIRECTORY") {
        if let Ok(key) = std::fs::read_to_string(format!("{dir}/glm")) {
            return Some(SecretKey(key.trim().to_string()));
        }
    }
    std::env::var("GLM_API_KEY").ok().map(|k| SecretKey(k.trim().to_string()))
}

fn trim_slash(url: String) -> String {
    url.trim_end_matches('/').to_string()
}
