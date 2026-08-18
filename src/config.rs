use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

/// On-disk shape (TOML). On NixOS this file is generated from the module's
/// nix expression; it never contains credentials.
#[derive(Deserialize, Default)]
struct FileConfig {
    listen: Option<SocketAddr>,
    anthropic_upstream: Option<String>,
    credentials_dir: Option<PathBuf>,
    #[serde(default)]
    providers: BTreeMap<String, FileProvider>,
}

#[derive(Deserialize)]
struct FileProvider {
    base_url: String,
    #[serde(default)]
    models: Vec<String>,
}

pub struct Config {
    pub listen: SocketAddr,
    /// Base URL requests are forwarded to verbatim, no trailing slash.
    pub anthropic_base: String,
    /// Where the TUI-managed credential files live (one file per provider).
    pub credentials_dir: PathBuf,
    pub providers: Vec<ProviderConfig>,
    /// The file this config was loaded from, for display; None means defaults.
    pub config_path: Option<PathBuf>,
}

pub struct ProviderConfig {
    pub name: String,
    /// OpenAI-compatible chat-completions base URL, no trailing slash.
    pub base_url: String,
    /// Real upstream model IDs, served in /v1/models as `anthropic/<id>`.
    pub models: Vec<String>,
}

impl Config {
    /// Precedence: explicit CLI path > $CLAUDE_ROUTER_CONFIG > XDG default.
    /// A missing default file is fine (pure passthrough); an unreadable or
    /// invalid explicitly-named file is an error.
    pub fn load(cli_path: Option<PathBuf>) -> Result<Self, String> {
        let explicit = cli_path
            .or_else(|| std::env::var_os("CLAUDE_ROUTER_CONFIG").map(PathBuf::from));
        let (path, required) = match explicit {
            Some(path) => (path, true),
            None => (xdg_dir("XDG_CONFIG_HOME", ".config").join("claude-router/config.toml"), false),
        };

        let file = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<FileConfig>(&text)
                .map_err(|err| format!("{}: {err}", path.display()))?,
            Err(err) if required => return Err(format!("{}: {err}", path.display())),
            Err(_) => FileConfig::default(),
        };
        let listen = match std::env::var("CLAUDE_ROUTER_LISTEN") {
            Ok(addr) => addr.parse().map_err(|_| "CLAUDE_ROUTER_LISTEN must be a socket address")?,
            Err(_) => file.listen.unwrap_or_else(|| "127.0.0.1:8787".parse().unwrap()),
        };

        let providers: Vec<ProviderConfig> = file
            .providers
            .into_iter()
            .map(|(name, p)| ProviderConfig {
                name,
                base_url: p.base_url.trim_end_matches('/').to_string(),
                models: p.models,
            })
            .collect();
        check_unique_models(&providers)?;

        Ok(Self {
            listen,
            anthropic_base: file
                .anthropic_upstream
                .unwrap_or_else(|| "https://api.anthropic.com".into())
                .trim_end_matches('/')
                .to_string(),
            credentials_dir: file.credentials_dir.unwrap_or_else(|| {
                xdg_dir("XDG_STATE_HOME", ".local/state").join("claude-router/credentials")
            }),
            providers,
            config_path: std::fs::metadata(&path).is_ok().then_some(path),
        })
    }

    pub fn provider_for_model(&self, real_model: &str) -> Option<usize> {
        self.providers.iter().position(|p| p.models.iter().any(|m| m == real_model))
    }
}

/// Model aliases carry no provider name, so a model ID listed by two
/// providers would be ambiguous to route.
fn check_unique_models(providers: &[ProviderConfig]) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for provider in providers {
        for model in &provider.models {
            if let Some(other) = seen.insert(model, &provider.name) {
                return Err(format!(
                    "model '{model}' is listed by both '{other}' and '{}'; model IDs must be unique across providers",
                    provider.name
                ));
            }
        }
    }
    Ok(())
}

fn xdg_dir(var: &str, home_fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(home_fallback)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> FileConfig {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn parses_multi_provider_config() {
        let file = parse(
            r#"
            listen = "127.0.0.1:9999"

            [providers.glm]
            base_url = "https://api.z.ai/api/paas/v4"
            models = ["glm-4.7", "glm-4.5-air"]

            [providers.deepseek]
            base_url = "https://api.deepseek.com/v1/"
            models = ["deepseek-chat"]
            "#,
        );
        assert_eq!(file.providers.len(), 2);
        assert_eq!(file.providers["glm"].models.len(), 2);
    }

    #[test]
    fn duplicate_models_across_providers_rejected() {
        let providers = vec![
            ProviderConfig { name: "a".into(), base_url: "x".into(), models: vec!["m".into()] },
            ProviderConfig { name: "b".into(), base_url: "y".into(), models: vec!["m".into()] },
        ];
        assert!(check_unique_models(&providers).is_err());
    }
}
