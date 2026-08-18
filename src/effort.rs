//! Reasoning-effort translation. Claude Code sends Anthropic's
//! `output_config.effort`; each provider names the field differently and
//! accepts its own set of levels, so the destination path and the level
//! mapping both come from that provider's config.

use serde_json::{Map, Value};

use crate::config::EffortConfig;

/// The field Claude Code sends effort in (Anthropic Messages API).
const SOURCE: (&str, &str) = ("output_config", "effort");

/// Read the requested effort from `source`, map it, and write it into
/// `target` at the configured path. `source` and `target` are the same body
/// on near-passthrough providers and different ones when translating.
/// Returns the provider-side level that was set, for logging.
pub fn apply(config: Option<&EffortConfig>, source: &Value, target: &mut Value) -> Option<String> {
    let config = config?;
    let requested = source[SOURCE.0][SOURCE.1].as_str();

    if let Some(object) = target.as_object_mut() {
        for key in &config.remove {
            object.remove(key);
        }
    }

    // An empty map means the provider uses Anthropic's own level names, so
    // only the field moves. With a map, an unlisted level falls back to the
    // default; with no default the field is left unset and the provider
    // applies its own canonical default for that model.
    let mapped = match (requested, config.map.is_empty()) {
        (Some(level), true) => Some(level.to_string()),
        (Some(level), false) => config.map.get(level).cloned(),
        (None, _) => None,
    }
    .or_else(|| config.default.clone())?;
    set_path(target, &config.field, Value::String(mapped.clone()));
    Some(mapped)
}

/// Set a dotted path, creating intermediate objects. A segment whose current
/// value is not an object is replaced, so a provider's spelling always wins
/// over whatever the client happened to send there.
fn set_path(target: &mut Value, path: &str, value: Value) {
    let mut current = target;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let object = current.as_object_mut().expect("object");
        if segments.peek().is_none() {
            object.insert(segment.to_string(), value);
            return;
        }
        current = object.entry(segment).or_insert_with(|| Value::Object(Map::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{tests::fixture, Config};
    use serde_json::json;

    fn effort_config() -> EffortConfig {
        let config = Config::load(Some(fixture("providers.toml"))).unwrap();
        config.providers.into_iter().find(|p| p.name == "beta").unwrap().effort.unwrap()
    }

    #[test]
    fn maps_level_into_configured_path_and_drops_removed_keys() {
        let config = effort_config();
        let mut body = json!({
            "model": "beta-model",
            "output_config": {"effort": "medium"},
            "messages": [],
        });
        let source = body.clone();
        // The fixture maps medium -> high, writes reasoning.effort, drops
        // output_config.
        assert_eq!(apply(Some(&config), &source, &mut body).as_deref(), Some("high"));
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("output_config").is_none());
    }

    /// A provider that shares Anthropic's level names only needs the field
    /// to move, and then imposes no default of its own.
    #[test]
    fn an_empty_map_moves_the_level_unchanged() {
        let config = EffortConfig {
            field: "reasoning.effort".into(),
            map: Default::default(),
            default: None,
            remove: vec!["output_config".into()],
        };
        let source = json!({"output_config": {"effort": "xhigh"}});
        let mut body = source.clone();
        assert_eq!(apply(Some(&config), &source, &mut body).as_deref(), Some("xhigh"));
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert!(body.get("output_config").is_none());

        // Nothing requested and no default: upstream decides.
        let source = json!({});
        let mut body = source.clone();
        assert_eq!(apply(Some(&config), &source, &mut body), None);
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn unmapped_level_falls_back_to_default() {
        let config = effort_config();
        let source = json!({"output_config": {"effort": "unheard-of"}});
        let mut body = source.clone();
        assert_eq!(apply(Some(&config), &source, &mut body).as_deref(), config.default.as_deref());
    }

    #[test]
    fn absent_config_leaves_body_untouched() {
        let source = json!({"output_config": {"effort": "high"}});
        let mut body = source.clone();
        assert_eq!(apply(None, &source, &mut body), None);
        assert_eq!(body, source);
    }

    #[test]
    fn translation_targets_a_separate_body() {
        let config = effort_config();
        let source = json!({"output_config": {"effort": "low"}});
        let mut target = json!({"model": "beta-model"});
        assert_eq!(apply(Some(&config), &source, &mut target).as_deref(), Some("low"));
        assert_eq!(target["reasoning"]["effort"], "low");
        assert_eq!(target["model"], "beta-model");
    }
}
