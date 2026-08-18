//! Named provider presets. Each one is a TOML file under `presets/`,
//! embedded at build time so a deployed binary needs nothing beside it. A
//! preset only supplies defaults: any field the user writes in their own
//! config wins, and lists are replaced wholesale rather than concatenated.

use toml::Value;

/// `preset = "<name>"` in a provider table.
pub const FIELD: &str = "preset";

const PRESETS: &[(&str, &str)] = &[("deepseek", include_str!("../presets/deepseek.toml"))];

pub fn names() -> Vec<&'static str> {
    PRESETS.iter().map(|(name, _)| *name).collect()
}

/// Resolve a provider table's `preset` reference, returning the table with
/// preset defaults filled in underneath the user's own keys.
pub fn resolve(provider: Value) -> Result<Value, String> {
    let Some(name) = provider.get(FIELD) else {
        return Ok(provider);
    };
    let name = name
        .as_str()
        .ok_or_else(|| format!("{FIELD} must be a string, one of: {}", names().join(", ")))?;
    let (_, text) = PRESETS
        .iter()
        .find(|(preset, _)| *preset == name)
        .ok_or_else(|| format!("unknown preset '{name}'; known presets: {}", names().join(", ")))?;
    let preset: Value =
        toml::from_str(text).map_err(|err| format!("preset '{name}' is invalid: {err}"))?;

    let mut merged = merge(preset, provider);
    if let Some(table) = merged.as_table_mut() {
        table.remove(FIELD);
    }
    Ok(merged)
}

/// Deep-merge `over` onto `base`: tables merge key by key, everything else
/// (including arrays) is replaced by `over`.
fn merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Table(base), Value::Table(mut over)) => {
            let mut out = base;
            for (key, value) in over.iter_mut() {
                let merged = match out.remove(key) {
                    Some(existing) => merge(existing, value.clone()),
                    None => value.clone(),
                };
                out.insert(key.clone(), merged);
            }
            Value::Table(out)
        }
        (_, over) => over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Value {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn every_shipped_preset_parses() {
        for (name, text) in PRESETS {
            let value: Value =
                toml::from_str(text).unwrap_or_else(|err| panic!("preset {name}: {err}"));
            assert!(value.get("base_url").is_some(), "preset {name} has no base_url");
        }
    }

    #[test]
    fn preset_supplies_defaults() {
        let resolved = resolve(table(r#"preset = "deepseek""#)).unwrap();
        assert_eq!(resolved["base_url"].as_str(), Some("https://api.deepseek.com/anthropic"));
        assert_eq!(resolved["api"].as_str(), Some("anthropic"));
        assert_eq!(resolved["models"].as_array().unwrap().len(), 2);
        assert!(resolved.get(FIELD).is_none(), "preset marker must not survive");
    }

    #[test]
    fn user_keys_override_preset() {
        let resolved = resolve(table(
            r#"
            preset = "deepseek"
            base_url = "http://localhost:9000"
            models = ["only-this-one"]
            "#,
        ))
        .unwrap();
        assert_eq!(resolved["base_url"].as_str(), Some("http://localhost:9000"));
        // Arrays replace rather than append.
        assert_eq!(resolved["models"].as_array().unwrap().len(), 1);
        // Untouched preset keys still come through.
        assert_eq!(resolved["api"].as_str(), Some("anthropic"));
    }

    #[test]
    fn nested_tables_merge_key_by_key() {
        let resolved = resolve(table(
            r#"
            preset = "deepseek"
            [effort]
            default = "max"
            "#,
        ))
        .unwrap();
        assert_eq!(resolved["effort"]["default"].as_str(), Some("max"));
        // field came from the preset, default from the user
        assert_eq!(resolved["effort"]["field"].as_str(), Some("output_config.effort"));
    }

    #[test]
    fn unknown_preset_is_an_error() {
        let err = resolve(table(r#"preset = "nope""#)).unwrap_err();
        assert!(err.contains("unknown preset"), "{err}");
    }
}
