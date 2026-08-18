use serde::Deserialize;

use crate::config::Config;

/// Model IDs must start with `claude` or `anthropic` to survive Claude Code's
/// discovery filter, so provider models are aliased as `anthropic/<real-id>`.
/// Genuine Anthropic IDs are `claude-*`; the prefix cannot collide.
pub const ALIAS_PREFIX: &str = "anthropic/";

pub enum Backend {
    Anthropic,
    Provider { real_model: String },
}

#[derive(Deserialize)]
struct Peek {
    model: Option<String>,
}

/// Shallow-parse only `model` from the buffered body. The bytes themselves are
/// never reserialized; malformed JSON routes to Anthropic so its own error
/// response comes back verbatim.
pub fn route(config: &Config, body: &[u8]) -> Backend {
    if config.provider.is_none() {
        return Backend::Anthropic;
    }
    let Ok(Peek { model: Some(model) }) = serde_json::from_slice::<Peek>(body) else {
        return Backend::Anthropic;
    };
    match model.strip_prefix(ALIAS_PREFIX) {
        Some(real) if !real.is_empty() => Backend::Provider { real_model: real.to_string() },
        _ => Backend::Anthropic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config(with_provider: bool) -> Config {
        let provider = with_provider.then(|| {
            std::env::set_var("GLM_API_KEY", "test-key");
            let cfg = Config::from_env();
            std::env::remove_var("GLM_API_KEY");
            cfg.provider.expect("provider enabled")
        });
        Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            anthropic_base: "https://api.anthropic.com".into(),
            provider,
        }
    }

    #[test]
    fn aliased_model_routes_to_provider() {
        let body = br#"{"model":"anthropic/glm-4.7","messages":[]}"#;
        match route(&config(true), body) {
            Backend::Provider { real_model } => assert_eq!(real_model, "glm-4.7"),
            Backend::Anthropic => panic!("expected provider"),
        }
    }

    #[test]
    fn claude_model_passes_through() {
        let body = br#"{"model":"claude-sonnet-5","messages":[]}"#;
        assert!(matches!(route(&config(true), body), Backend::Anthropic));
    }

    #[test]
    fn alias_without_provider_passes_through() {
        let body = br#"{"model":"anthropic/glm-4.7"}"#;
        assert!(matches!(route(&config(false), body), Backend::Anthropic));
    }

    #[test]
    fn malformed_json_passes_through() {
        assert!(matches!(route(&config(true), b"not json"), Backend::Anthropic));
    }
}
