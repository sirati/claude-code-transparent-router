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
    /// Left as raw TOML so `preset` can be resolved before deserializing.
    #[serde(default)]
    providers: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct FileProvider {
    base_url: String,
    #[serde(default)]
    api: Option<ApiFormat>,
    #[serde(default)]
    models: Vec<FileModel>,
    #[serde(default)]
    effort: Option<EffortConfig>,
}

/// How a provider expresses reasoning effort. Claude Code sends Anthropic's
/// `output_config.effort`; providers spell it differently and accept a
/// different set of levels, so both the destination field and the level
/// mapping are configuration, never built into the router.
#[derive(Deserialize, Debug)]
pub struct EffortConfig {
    /// Dotted JSON path in the outgoing body, e.g. `reasoning.effort` or
    /// `reasoning_effort`.
    pub field: String,
    /// Inbound level -> provider level. Levels absent here fall back to
    /// `default`; with no default the field is left unset.
    #[serde(default)]
    pub map: BTreeMap<String, String>,
    /// Used when the request carries no effort, or an unmapped one.
    #[serde(default)]
    pub default: Option<String>,
    /// Top-level request keys to drop, for providers that would otherwise
    /// see two conflicting effort spellings.
    #[serde(default)]
    pub remove: Vec<String>,
}

/// Which API dialect the provider speaks. `anthropic` providers get
/// near-passthrough (model rewrite + credential swap, response verbatim);
/// `openai` providers go through the Messages <-> chat-completions translator.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ApiFormat {
    Openai,
    Anthropic,
}

/// A model entry: either a bare upstream ID, or an ID plus the display name
/// shown in Claude Code's model switcher.
#[derive(Deserialize)]
#[serde(untagged)]
enum FileModel {
    Id(String),
    Named { id: String, name: String },
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
    /// Provider API base URL, no trailing slash.
    pub base_url: String,
    pub api: ApiFormat,
    /// Real upstream models, served in /v1/models as `anthropic/<id>`.
    pub models: Vec<Model>,
    pub effort: Option<EffortConfig>,
}

pub struct Model {
    pub id: String,
    pub display_name: Option<String>,
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
            .map(|(name, raw)| {
                let resolved = crate::presets::resolve(raw)
                    .map_err(|err| format!("provider '{name}': {err}"))?;
                let provider: FileProvider = resolved
                    .try_into()
                    .map_err(|err| format!("provider '{name}': {err}"))?;
                Ok((name, provider))
            })
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .map(|(name, p)| ProviderConfig {
                name,
                base_url: p.base_url.trim_end_matches('/').to_string(),
                api: p.api.unwrap_or(ApiFormat::Openai),
                effort: p.effort,
                models: p
                    .models
                    .into_iter()
                    .map(|m| match m {
                        FileModel::Id(id) => Model { id, display_name: None },
                        FileModel::Named { id, name } => Model { id, display_name: Some(name) },
                    })
                    .collect(),
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
            credentials_dir: file.credentials_dir.unwrap_or_else(default_credentials_dir),
            providers,
            config_path: std::fs::metadata(&path).is_ok().then_some(path),
        })
    }

    pub fn provider_for_model(&self, real_model: &str) -> Option<usize> {
        self.providers.iter().position(|p| p.models.iter().any(|m| m.id == real_model))
    }
}

/// Model aliases carry no provider name, so a model ID listed by two
/// providers would be ambiguous to route.
fn check_unique_models(providers: &[ProviderConfig]) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for provider in providers {
        for model in &provider.models {
            if let Some(other) = seen.insert(&model.id, &provider.name) {
                return Err(format!(
                    "model '{}' is listed by both '{other}' and '{}'; model IDs must be unique across providers",
                    model.id, provider.name
                ));
            }
        }
    }
    Ok(())
}

/// Under systemd, StateDirectory= is the only writable home for runtime-set
/// credentials (DynamicUser + ProtectHome); interactively it's XDG state.
fn default_credentials_dir() -> PathBuf {
    match std::env::var_os("STATE_DIRECTORY") {
        Some(dir) => PathBuf::from(dir).join("credentials"),
        None => xdg_dir("XDG_STATE_HOME", ".local/state").join("claude-router/credentials"),
    }
}

fn xdg_dir(var: &str, home_fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(home_fallback)
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn loads_multi_provider_config_file() {
        let config = Config::load(Some(fixture("providers.toml"))).unwrap();
        assert_eq!(config.providers.len(), 2);
        let alpha = &config.providers[0];
        assert_eq!(alpha.name, "alpha");
        assert_eq!(alpha.base_url, "http://alpha.example/v1");
        assert_eq!(alpha.api, ApiFormat::Openai);
        assert_eq!(alpha.models.len(), 2);
        let beta = &config.providers[1];
        assert_eq!(beta.base_url, "http://beta.example/v1");
        assert_eq!(beta.api, ApiFormat::Anthropic);
        assert_eq!(beta.models[0].id, "beta-model");
        assert_eq!(beta.models[0].display_name.as_deref(), Some("Beta Model Pro"));
        assert_eq!(config.provider_for_model("beta-model"), Some(1));
        assert_eq!(config.provider_for_model("unlisted"), None);
    }

    #[test]
    fn duplicate_model_ids_across_providers_rejected() {
        let err = match Config::load(Some(fixture("duplicate-models.toml"))) {
            Err(err) => err,
            Ok(_) => panic!("duplicate model ids must be rejected"),
        };
        assert!(err.contains("shared-model"), "{err}");
    }

    #[test]
    fn preset_fills_provider_defaults_and_yields_to_overrides() {
        let config = Config::load(Some(fixture("preset.toml"))).unwrap();
        let custom = config.providers.iter().find(|p| p.name == "custom").unwrap();
        let deepseek = config.providers.iter().find(|p| p.name == "deepseek").unwrap();

        // Straight from the preset: endpoint, dialect, both models.
        assert_eq!(deepseek.base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(deepseek.api, ApiFormat::Anthropic);
        assert_eq!(deepseek.models.len(), 2);
        assert!(deepseek.models.iter().any(|m| m.id.ends_with("-pro")));
        assert!(deepseek.models.iter().any(|m| m.id.ends_with("-flash")));
        assert_eq!(deepseek.effort.as_ref().unwrap().field, "output_config.effort");

        // User keys win; unmentioned preset keys survive.
        assert_eq!(custom.base_url, "http://127.0.0.1:9000");
        assert_eq!(custom.models.len(), 1);
        assert_eq!(custom.api, ApiFormat::Anthropic);
        let effort = custom.effort.as_ref().unwrap();
        assert_eq!(effort.default.as_deref(), Some("max"));
        assert_eq!(effort.field, "output_config.effort");
    }

    #[test]
    fn explicitly_named_missing_config_is_an_error() {
        assert!(Config::load(Some(fixture("does-not-exist.toml"))).is_err());
    }
}
