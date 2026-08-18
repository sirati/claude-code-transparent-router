use serde::Deserialize;

use crate::config::Config;

pub enum Backend {
    Anthropic,
    Provider { provider: usize, real_model: String },
    /// A name that looks like it meant a routed model but matched nothing.
    /// Forwarding it would produce a confusing upstream 404.
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
    resolve(config, &model)
}

/// Accepted spellings:
///
/// - `<provider>/<model>` — e.g. `deepseek/v4-pro`
/// - `<model>` — a bare ID or shorthand, e.g. `sol`
///
/// In both forms `<model>` may be the upstream ID or one of its shorthands.
/// Anything else is Anthropic's to answer.
pub fn resolve(config: &Config, model: &str) -> Backend {
    if let Some((provider, rest)) = model.split_once('/') {
        return match find(config, Some(provider), rest) {
            Some(backend) => backend,
            // A provider-qualified name that matched nothing is a mistake
            // worth reporting, but only when the prefix really is one of
            // ours — other slashed IDs belong upstream.
            None if config.providers.iter().any(|p| p.name == provider) => {
                Backend::UnknownAlias { model: model.to_string() }
            }
            None => Backend::Anthropic,
        };
    }

    // A bare name only routes when it actually names a configured model, so
    // Anthropic's own IDs are never captured.
    find(config, None, model).unwrap_or(Backend::Anthropic)
}

fn find(config: &Config, provider_name: Option<&str>, model: &str) -> Option<Backend> {
    config.providers.iter().enumerate().find_map(|(index, provider)| {
        if provider_name.is_some_and(|name| name != provider.name) {
            return None;
        }
        provider
            .models
            .iter()
            .find(|candidate| candidate.matches(model))
            .map(|candidate| Backend::Provider {
                provider: index,
                real_model: candidate.id.clone(),
            })
    })
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

    fn routed(model: &str) -> Option<(usize, String)> {
        match resolve(&config(), model) {
            Backend::Provider { provider, real_model } => Some((provider, real_model)),
            _ => None,
        }
    }

    #[test]
    fn accepts_provider_qualified_names() {
        assert_eq!(routed("beta/beta-model"), Some((1, "beta-model".into())));
        assert_eq!(routed("alpha/alpha-model"), Some((0, "alpha-model".into())));
    }

    #[test]
    fn accepts_shorthands_in_every_form() {
        // The fixture gives beta-model the shorthand "beta-pro".
        assert_eq!(routed("beta-pro"), Some((1, "beta-model".into())));
        assert_eq!(routed("beta/beta-pro"), Some((1, "beta-model".into())));
    }

    #[test]
    fn accepts_a_bare_model_id() {
        assert_eq!(routed("alpha-model"), Some((0, "alpha-model".into())));
    }

    #[test]
    fn anthropic_models_are_never_captured() {
        let config = config();
        assert!(matches!(resolve(&config, "claude-sonnet-5"), Backend::Anthropic));
        assert!(matches!(resolve(&config, "claude-opus-4-5"), Backend::Anthropic));
        // A slashed ID whose prefix is not one of ours belongs upstream —
        // including the `anthropic/` spelling this router used to accept.
        assert!(matches!(resolve(&config, "bedrock/anthropic.claude-v2"), Backend::Anthropic));
        assert!(matches!(resolve(&config, "anthropic/beta-model"), Backend::Anthropic));
    }

    #[test]
    fn mistakes_in_our_own_namespaces_are_reported() {
        assert!(matches!(resolve(&config(), "beta/nope"), Backend::UnknownAlias { .. }));
    }

    #[test]
    fn malformed_json_passes_through() {
        assert!(matches!(route(&config(), b"not json"), Backend::Anthropic));
    }
}
