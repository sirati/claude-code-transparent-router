use serde::Deserialize;

use crate::config::Config;

/// Model IDs must start with `claude` or `anthropic` to survive Claude Code's
/// discovery filter, so provider models are aliased as `anthropic/<real-id>`.
/// Genuine Anthropic IDs are `claude-*`; the prefix cannot collide.
pub const ALIAS_PREFIX: &str = "anthropic/";

pub enum Backend {
    Anthropic,
    Provider { provider: usize, real_model: String },
    /// `anthropic/<x>` where no configured provider lists `<x>`: forwarding it
    /// would produce a confusing upstream 404, so it gets a clear local error.
    UnknownAlias { model: String },
}

#[derive(Deserialize)]
struct Peek {
    model: Option<String>,
}

/// Shallow-parse only `model` from the buffered body. The bytes themselves are
/// never reserialized; malformed JSON routes to Anthropic so its own error
/// response comes back verbatim.
pub fn route(config: &Config, body: &[u8]) -> Backend {
    let Ok(Peek { model: Some(model) }) = serde_json::from_slice::<Peek>(body) else {
        return Backend::Anthropic;
    };
    let Some(real) = model.strip_prefix(ALIAS_PREFIX).filter(|m| !m.is_empty()) else {
        return Backend::Anthropic;
    };
    match config.provider_for_model(real) {
        Some(provider) => Backend::Provider { provider, real_model: real.to_string() },
        None => Backend::UnknownAlias { model: model.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{tests::fixture, Config};

    /// Tests exercise the real config path: providers come from the same TOML
    /// loading a deployment uses, never from structs built in code.
    fn config() -> Config {
        Config::load(Some(fixture("providers.toml"))).unwrap()
    }

    #[test]
    fn aliased_models_route_to_their_provider() {
        match route(&config(), br#"{"model":"anthropic/beta-model"}"#) {
            Backend::Provider { provider, real_model } => {
                assert_eq!(provider, 1);
                assert_eq!(real_model, "beta-model");
            }
            _ => panic!("expected provider"),
        }
    }

    #[test]
    fn claude_model_passes_through() {
        assert!(matches!(
            route(&config(), br#"{"model":"claude-sonnet-5"}"#),
            Backend::Anthropic
        ));
    }

    #[test]
    fn unknown_alias_is_flagged() {
        assert!(matches!(
            route(&config(), br#"{"model":"anthropic/nope"}"#),
            Backend::UnknownAlias { .. }
        ));
    }

    #[test]
    fn malformed_json_passes_through() {
        assert!(matches!(route(&config(), b"not json"), Backend::Anthropic));
    }
}
