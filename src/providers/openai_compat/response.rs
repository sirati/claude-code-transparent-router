//! OpenAI chat-completions response -> Anthropic message (non-streaming).

use serde_json::{json, Value};

pub fn to_anthropic(openai: &Value, alias_model: &str) -> Value {
    let choice = &openai["choices"][0];
    let message = &choice["message"];

    let mut content = Vec::new();
    if let Some(thinking) = message["reasoning_content"].as_str().filter(|s| !s.is_empty()) {
        content.push(json!({"type": "thinking", "thinking": thinking, "signature": ""}));
    }
    if let Some(text) = message["content"].as_str().filter(|s| !s.is_empty()) {
        content.push(json!({"type": "text", "text": text}));
    }
    for (i, call) in message["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let fallback_id = format!("toolu_{i}");
        content.push(json!({
            "type": "tool_use",
            "id": call["id"].as_str().filter(|s| !s.is_empty()).unwrap_or(&fallback_id),
            "name": call["function"]["name"],
            "input": parse_arguments(&call["function"]["arguments"]),
        }));
    }

    json!({
        "id": openai["id"].as_str().unwrap_or("msg_routed"),
        "type": "message",
        "role": "assistant",
        "model": alias_model,
        "content": content,
        "stop_reason": stop_reason(choice["finish_reason"].as_str()),
        "stop_sequence": null,
        "usage": {
            "input_tokens": openai["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            "output_tokens": openai["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        },
    })
}

pub fn stop_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        _ => "end_turn",
    }
}

/// OpenAI ships tool arguments as a JSON-encoded string; Anthropic wants the
/// object. An unparseable fragment is preserved under "raw" rather than lost.
pub fn parse_arguments(arguments: &Value) -> Value {
    match arguments.as_str() {
        Some("") | None => json!({}),
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({"raw": s})),
    }
}
