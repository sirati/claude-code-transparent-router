//! Relax only Claude Code's Agent tool model choices for routed providers.
//!
//! The host schema carries an Anthropic-only enum. Every provider sees this
//! same request, so expand it once before dispatch rather than teaching each
//! backend dialect its own exception.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::config::Config;
use crate::route::LARGE_CONTEXT_MARKER;

/// Add the router's provider-qualified model names and the empty selection to
/// the exact `Agent` tool schema in `request`. Returns whether it changed the
/// JSON; callers can preserve original request bytes when it did not.
pub fn extend_model_enum(config: &Config, request: &mut Value) -> bool {
    let choices = model_choices(config);
    let Some(tools) = request.get_mut("tools").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut changed = false;
    for tool in tools
        .iter_mut()
        .filter(|tool| tool.get("name").and_then(Value::as_str) == Some("Agent"))
    {
        let Some(schema) = tool.get_mut("input_schema").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(model) = properties.get_mut("model").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(enum_values) = model.get_mut("enum").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        for value in enum_values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .chain(choices.iter().cloned())
        {
            if seen.insert(value.clone()) {
                values.push(json!(value));
            }
        }
        if *enum_values != values {
            *enum_values = values;
            changed = true;
        }
    }
    changed
}

/// Clone an Anthropic tool schema for an OpenAI provider. The host Agent tool
/// accepts an absent `model` override and then uses the selected custom
/// agent's frontmatter model, but rejects a routed model string if it is
/// present. Do not let OpenAI choose that invalid field at all.
pub fn without_model_for_openai(tool: &Value) -> Value {
    let mut schema = tool["input_schema"].clone();
    if tool.get("name").and_then(Value::as_str) != Some("Agent") {
        return schema;
    }
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.remove("model");
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|field| field != "model");
    }
    schema
}

fn model_choices(config: &Config) -> Vec<String> {
    let mut choices = vec![String::new()];
    for provider in &config.providers {
        for model in &provider.models {
            for name in model.names() {
                let qualified = format!("{}/{}", provider.name, name);
                choices.push(qualified.clone());
                if model.has_large_context() {
                    choices.push(format!("{qualified}{LARGE_CONTEXT_MARKER}"));
                }
            }
        }
    }
    choices
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::{tests::fixture, Config};

    #[test]
    fn extends_only_the_agent_model_enum() {
        let config = Config::load(Some(fixture("providers.toml"))).unwrap();
        let mut request = json!({
            "tools": [
                {
                    "name": "Agent",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "model": {"type": "string", "enum": ["sonnet", "sonnet"]}
                        },
                        "required": ["model"]
                    }
                },
                {
                    "name": "get_weather",
                    "input_schema": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    }
                }
            ]
        });
        let weather = request["tools"][1].clone();

        assert!(extend_model_enum(&config, &mut request));
        let agent = &request["tools"][0]["input_schema"];
        let choices = agent["properties"]["model"]["enum"].as_array().unwrap();
        let values: Vec<&str> = choices.iter().filter_map(Value::as_str).collect();
        assert_eq!(values[0], "sonnet");
        assert_eq!(values.iter().filter(|value| **value == "sonnet").count(), 1);
        assert!(values.contains(&""));
        assert!(values.contains(&"alpha/alpha-model"));
        assert!(values.contains(&"beta/beta-model"));
        assert!(values.contains(&"beta/beta-pro"));
        assert!(values.contains(&"beta/beta-model[1m]"));
        assert!(values.contains(&"beta/beta-pro[1m]"));
        assert_eq!(agent["required"], json!(["model"]));
        assert_eq!(request["tools"][1], weather);

        assert!(!extend_model_enum(&config, &mut request));
    }

    #[test]
    fn omits_only_agent_model_for_openai() {
        let agent = json!({
            "name": "Agent",
            "input_schema": {
                "type": "object",
                "properties": {
                    "model": {"type": "string", "enum": ["sonnet"]},
                    "prompt": {"type": "string"}
                },
                "required": ["model", "prompt"],
                "additionalProperties": false
            }
        });
        let schema = without_model_for_openai(&agent);
        assert!(schema["properties"].get("model").is_none());
        assert_eq!(schema["required"], json!(["prompt"]));
        assert_eq!(schema["additionalProperties"], false);

        let other = json!({"name": "get_weather", "input_schema": {"required": ["city"]}});
        assert_eq!(without_model_for_openai(&other), other["input_schema"]);
    }

    #[test]
    fn ignores_missing_or_malformed_agent_schemas() {
        let config = Config::load(Some(fixture("providers.toml"))).unwrap();
        let mut request = json!({"tools": [{"name": "Agent", "input_schema": {}}]});
        let original = request.clone();
        assert!(!extend_model_enum(&config, &mut request));
        assert_eq!(request, original);
    }
}
