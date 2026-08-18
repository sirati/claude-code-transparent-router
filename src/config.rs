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
    /// Accept connections only from the user the daemon runs as. Right for a
    /// user service; wrong for a system service whose clients are other
    /// users, which is why it is opt-in.
    #[serde(default)]
    restrict_to_owner: bool,
    /// Additional uids allowed to connect. Empty and `restrict_to_owner`
    /// unset means any local user may connect, as any loopback port allows.
    #[serde(default)]
    allowed_uids: Vec<u32>,
    /// Which model fills the extra custom /model row; gateway discovery
    /// already lists every configured model. Written as the alias or the bare
    /// provider model ID.
    #[serde(default)]
    picker_model: Option<String>,
    /// Serve each connecting user from their own config and credentials in
    /// their home directory, so one system-wide daemon carries nothing that
    /// belongs to a particular person.
    #[serde(default)]
    user_config: bool,
    /// Exit after this many seconds without a request; pairs with socket
    /// activation. Absent or zero keeps the daemon resident.
    #[serde(default)]
    idle_timeout_secs: Option<u64>,
    /// Extra wordings that mark a compaction request, added to the ones the
    /// router already knows. Claude Code's phrasing changes between
    /// releases, so this is how a new one is taught without a rebuild.
    #[serde(default)]
    compact_patterns: Vec<String>,
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
    #[serde(default)]
    oauth: Option<OauthConfig>,
    /// Static headers added to every request to this provider.
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// Fields merged into the outgoing request body, for provider-specific
    /// knobs the translators don't model.
    #[serde(default)]
    request_extra: BTreeMap<String, toml::Value>,
    /// Top-level fields to drop from the outgoing body, for providers that
    /// reject something the translation would otherwise send.
    #[serde(default)]
    request_remove: Vec<String>,
    /// How this provider wants a compaction request, when it wants one
    /// differently from an ordinary turn.
    #[serde(default)]
    compaction: Option<CompactionConfig>,
}

/// A provider's own compaction protocol, applied only to requests recognised
/// as Claude Code compacting the conversation.
///
/// Codex offers two on this backend: a `responses/compact` endpoint, and a
/// normal turn carrying a trailing `{"type": "compaction_trigger"}` item —
/// with the summarisation prompt held server-side. Both answer with an
/// `encrypted_content` compaction item rather than readable text, which
/// Claude Code cannot use as its summary, so neither is on by default.
#[derive(Deserialize, Debug, Default)]
pub struct CompactionConfig {
    /// Endpoint path for compaction, relative to the provider's base URL.
    #[serde(default)]
    pub path: Option<String>,
    /// Append `{"type": "<this>"}` as the final input item.
    #[serde(default)]
    pub trigger_item: Option<String>,
    /// Fields merged into the compaction request body.
    #[serde(default)]
    pub request_extra: BTreeMap<String, toml::Value>,
    /// Fields dropped from the compaction request body. Codex omits
    /// tool_choice, store, stream, include and client_metadata on its
    /// dedicated endpoint.
    #[serde(default)]
    pub request_remove: Vec<String>,
}

/// A provider whose credential is an OAuth token rather than an API key.
/// Every endpoint, identifier and parameter comes from configuration, so
/// supporting another OAuth provider is a preset file, not a code change.
#[derive(Deserialize, Debug, Clone)]
pub struct OauthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scope: String,
    /// Loopback port for the redirect. The provider's registered redirect
    /// URI has to match, so this is rarely free to choose.
    pub callback_port: u16,
    #[serde(default = "default_callback_path")]
    pub callback_path: String,
    /// Extra query parameters on the authorize URL.
    #[serde(default)]
    pub authorize_extra: BTreeMap<String, String>,
    /// `form` (RFC 6749) or `json`, as the provider's refresh endpoint wants.
    #[serde(default = "default_refresh_format")]
    pub refresh_format: RefreshFormat,
    /// Full refresh/token endpoint, when it is not `{issuer}/oauth/token`.
    /// Anthropic is the example: authorize lives at claude.ai, tokens at
    /// api.anthropic.com.
    #[serde(default)]
    pub token_url: Option<String>,
    /// Dotted path of the id_token claim holding the account identifier,
    /// e.g. `https://api.openai.com/auth.chatgpt_account_id`. Claim names
    /// containing dots are matched before the path is split.
    #[serde(default)]
    pub account_id_claim: Option<String>,
    /// Header the account identifier is sent in.
    #[serde(default)]
    pub account_header: Option<String>,
    /// Reuse the Claude Code CLI's own claude.ai login instead of starting a
    /// browser flow: on `login`, import `~/.claude/.credentials.json` and
    /// refresh it if stale. When the CLI has no session, fall back to the
    /// browser. Only meaningful for a provider whose OAuth is the same one
    /// the CLI uses.
    #[serde(default)]
    pub import_claude_code: bool,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RefreshFormat {
    Form,
    Json,
}

fn default_callback_path() -> String {
    "/auth/callback".into()
}

fn default_refresh_format() -> RefreshFormat {
    RefreshFormat::Form
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

/// Which API dialect the provider speaks. `anthropic` gets near-passthrough
/// (model rewrite + credential swap, response verbatim); `openai` goes
/// through the Messages <-> chat-completions translator; `responses` through
/// the Messages <-> Responses translator, which is what OpenAI's newer models
/// need for tool calling.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ApiFormat {
    Openai,
    Anthropic,
    Responses,
}

/// A model entry: a bare upstream ID, or an ID with a display name for the
/// model switcher and shorthands to select it by.
#[derive(Deserialize)]
#[serde(untagged)]
enum FileModel {
    Id(String),
    Full {
        id: String,
        #[serde(default)]
        name: Option<String>,
        /// Shorter names for the same model, e.g. `sol` for `gpt-5.6-sol`.
        #[serde(default)]
        aliases: Vec<String>,
        /// Tokens the model accepts. Claude Code assumes 200k for a model it
        /// does not know, and compacts against that, so a bigger window has
        /// to be declared somewhere.
        #[serde(default)]
        context_window: Option<u64>,
        #[serde(default)]
        max_output_tokens: Option<u64>,
    },
}

pub struct Config {
    pub listen: SocketAddr,
    /// Base URL requests are forwarded to verbatim, no trailing slash.
    pub anthropic_base: String,
    /// Where the TUI-managed credential files live (one file per provider).
    pub credentials_dir: PathBuf,
    /// Uids permitted to connect; empty means unrestricted.
    pub allowed_uids: Vec<u32>,
    /// Provider model ID (never the alias) to offer as the custom /model row.
    pub picker_model: Option<String>,
    /// Serve each user from their own config file and credential store.
    pub user_config: bool,
    /// Idle seconds before the daemon exits; None or zero means stay.
    pub idle_timeout_secs: Option<u64>,
    /// Extra wordings that mark a compaction request.
    pub compact_patterns: Vec<String>,
    pub providers: Vec<ProviderConfig>,
    /// The file this config was loaded from, for display; None means defaults.
    pub config_path: Option<PathBuf>,
}

pub struct ProviderConfig {
    pub name: String,
    /// Provider API base URL, no trailing slash.
    pub base_url: String,
    pub api: ApiFormat,
    /// Real upstream models, served in /v1/models as
    /// `claude-routed-<provider>/<id>`.
    pub models: Vec<Model>,
    pub effort: Option<EffortConfig>,
    pub oauth: Option<OauthConfig>,
    pub headers: BTreeMap<String, String>,
    pub request_extra: BTreeMap<String, toml::Value>,
    pub request_remove: Vec<String>,
    /// Set when this provider has its own compaction protocol.
    pub compaction: Option<CompactionConfig>,
}

pub struct Model {
    pub id: String,
    pub display_name: Option<String>,
    /// Shorthands that select this model, alongside its ID.
    pub aliases: Vec<String>,
    /// Tokens the model accepts, and will emit, when known.
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// A window this size or larger is declared with Claude Code's `[1m]` marker
/// rather than through the session-wide token setting.
pub const LARGE_CONTEXT: u64 = 1_000_000;

impl Model {
    /// Does `name` select this model? Its ID or any of its shorthands do.
    pub fn matches(&self, name: &str) -> bool {
        self.id == name || self.aliases.iter().any(|alias| alias == name)
    }

    pub fn has_large_context(&self) -> bool {
        self.context_window.is_some_and(|window| window >= LARGE_CONTEXT)
    }

    /// Every name this model answers to.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
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
                oauth: p.oauth,
                headers: p.headers,
                request_extra: p.request_extra,
                request_remove: p.request_remove,
                compaction: p.compaction,
                models: p
                    .models
                    .into_iter()
                    .map(|m| match m {
                        FileModel::Id(id) => Model {
                            id,
                            display_name: None,
                            aliases: Vec::new(),
                            context_window: None,
                            max_output_tokens: None,
                        },
                        FileModel::Full {
                            id,
                            name,
                            aliases,
                            context_window,
                            max_output_tokens,
                        } => Model {
                            id,
                            display_name: name,
                            aliases,
                            context_window,
                            max_output_tokens,
                        },
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
            allowed_uids: {
                let mut uids = file.allowed_uids;
                if file.restrict_to_owner {
                    // Without our own uid the daemon would lock itself out.
                    uids.extend(crate::peer::own_uid());
                }
                uids
            },
            user_config: file.user_config,
            idle_timeout_secs: file.idle_timeout_secs,
            compact_patterns: file.compact_patterns,
            picker_model: file.picker_model,
            providers,
            config_path: std::fs::metadata(&path).is_ok().then_some(path),
        })
    }

    pub fn provider_for_model(&self, real_model: &str) -> Option<usize> {
        self.providers.iter().position(|p| p.models.iter().any(|m| m.id == real_model))
    }
}

/// A bare model name selects one model, so IDs and shorthands have to be
/// unique across providers; otherwise `/model sol` would be a coin toss.
fn check_unique_models(providers: &[ProviderConfig]) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for provider in providers {
        for model in &provider.models {
            for name in model.names() {
                if let Some(other) = seen.insert(name, &provider.name) {
                    return Err(format!(
                        "'{name}' is claimed by both '{other}' and '{}'; \
                         model IDs and shorthands must be unique across providers",
                        provider.name
                    ));
                }
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
        // A compaction section is optional, and absent unless written.
        assert!(alpha.compaction.is_none());
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

    #[test]
    fn anthropic_preset_loads_as_an_oauth_provider() {
        let config = Config::load(Some(fixture("anthropic.toml"))).unwrap();
        let anthropic = &config.providers[0];
        assert_eq!(anthropic.api, ApiFormat::Anthropic);
        assert_eq!(anthropic.models.len(), 4);
        let oauth = anthropic.oauth.as_ref().unwrap();
        assert!(oauth.import_claude_code);
        assert_eq!(
            oauth.token_url.as_deref(),
            Some("https://api.anthropic.com/v1/oauth/token")
        );
    }
}
