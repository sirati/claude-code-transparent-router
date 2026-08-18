//! OpenAI Responses reply -> Anthropic message (non-streaming).

use serde_json::{json, Value};

pub fn to_anthropic(responses: &Value, alias_model: &str) -> Value {
    let mut content = Vec::new();
    let mut stop_reason = "end_turn";

    for (i, item) in responses["output"].as_array().into_iter().flatten().enumerate() {
        match item["type"].as_str() {
            Some("reasoning") => {
                let text = reasoning_text(item);
                if !text.is_empty() {
                    content.push(json!({"type": "thinking", "thinking": text, "signature": ""}));
                }
            }
            Some("message") => {
                let text = output_text(item);
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            Some("function_call") => {
                stop_reason = "tool_use";
                let fallback = format!("toolu_{i}");
                content.push(json!({
                    "type": "tool_use",
                    "id": item["call_id"].as_str().filter(|s| !s.is_empty()).unwrap_or(&fallback),
                    "name": item["name"],
                    "input": parse_arguments(&item["arguments"]),
                }));
            }
            _ => {}
        }
    }

    if responses["status"] == json!("incomplete") {
        stop_reason = "max_tokens";
    }

    json!({
        "id": responses["id"].as_str().unwrap_or("msg_routed"),
        "type": "message",
        "role": "assistant",
        "model": alias_model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": responses["usage"]["input_tokens"].as_u64().unwrap_or(0),
            "output_tokens": responses["usage"]["output_tokens"].as_u64().unwrap_or(0),
        },
    })
}

fn output_text(item: &Value) -> String {
    item["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

/// Reasoning items carry a summary array, and some deployments also return
/// full reasoning text; take whichever is present.
fn reasoning_text(item: &Value) -> String {
    let from = |key: &str| {
        item[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let summary = from("summary");
    if summary.is_empty() {
        from("content")
    } else {
        summary
    }
}

/// Responses ships tool arguments as a JSON-encoded string; Anthropic wants
/// the object. An unparseable fragment is preserved rather than lost.
pub fn parse_arguments(arguments: &Value) -> Value {
    match arguments.as_str() {
        Some("") | None => json!({}),
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({"raw": s})),
    }
}
