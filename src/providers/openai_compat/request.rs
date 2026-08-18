//! Anthropic Messages request -> OpenAI chat-completions request. Works on
//! `serde_json::Value` so unknown fields and block types degrade gracefully
//! instead of failing deserialization.

use serde_json::{json, Value};

pub fn to_openai(req: &Value, real_model: &str, streaming: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = system_text(&req["system"]) {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in req["messages"].as_array().into_iter().flatten() {
        convert_message(message, &mut messages);
    }

    let mut out = json!({"model": real_model, "messages": messages});
    for key in ["max_tokens", "temperature", "top_p"] {
        if let Some(v) = req.get(key).filter(|v| !v.is_null()) {
            out[key] = v.clone();
        }
    }
    if let Some(stops) = req.get("stop_sequences").filter(|v| !v.is_null()) {
        out["stop"] = stops.clone();
    }
    if let Some(tools) = req["tools"].as_array() {
        out["tools"] = tools.iter().map(convert_tool).collect();
    }
    if let Some(choice) = req.get("tool_choice").filter(|v| !v.is_null()) {
        out["tool_choice"] = convert_tool_choice(choice);
    }
    if streaming {
        out["stream"] = json!(true);
        out["stream_options"] = json!({"include_usage": true});
    }
    out
}

fn system_text(system: &Value) -> Option<String> {
    match system {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => Some(join_text(blocks, "\n\n")).filter(|s| !s.is_empty()),
        _ => None,
    }
}

fn convert_message(message: &Value, out: &mut Vec<Value>) {
    let role = message["role"].as_str().unwrap_or("user");
    match &message["content"] {
        Value::String(text) => out.push(json!({"role": role, "content": text})),
        Value::Array(blocks) if role == "assistant" => convert_assistant(blocks, out),
        Value::Array(blocks) => convert_user(blocks, out),
        _ => {}
    }
}

fn convert_assistant(blocks: &[Value], out: &mut Vec<Value>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("tool_use") => tool_calls.push(json!({
                "id": block["id"],
                "type": "function",
                "function": {"name": block["name"], "arguments": block["input"].to_string()},
            })),
            // thinking/redacted_thinking: signatures don't survive
            // translation, and OpenAI-style APIs have no slot for them.
            _ => {}
        }
    }
    let mut message = json!({"role": "assistant"});
    message["content"] = if text.is_empty() { Value::Null } else { json!(text) };
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    out.push(message);
}

fn convert_user(blocks: &[Value], out: &mut Vec<Value>) {
    // Tool results become `tool` role messages, emitted before any user
    // content: OpenAI requires them directly after the assistant tool_calls.
    let mut parts = Vec::new();
    let mut has_image = false;
    for block in blocks {
        match block["type"].as_str() {
            Some("tool_result") => out.push(json!({
                "role": "tool",
                "tool_call_id": block["tool_use_id"],
                "content": tool_result_text(block),
            })),
            Some("text") => parts.push(json!({"type": "text", "text": block["text"]})),
            Some("image") => {
                if let Some(part) = image_part(&block["source"]) {
                    has_image = true;
                    parts.push(part);
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return;
    }
    let content = if has_image {
        json!(parts)
    } else {
        json!(parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""))
    };
    out.push(json!({"role": "user", "content": content}));
}

fn tool_result_text(block: &Value) -> String {
    let text = match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => join_text(blocks, "\n"),
        _ => String::new(),
    };
    if block["is_error"].as_bool().unwrap_or(false) {
        format!("Error: {text}")
    } else {
        text
    }
}

fn image_part(source: &Value) -> Option<Value> {
    let url = match source["type"].as_str()? {
        "base64" => format!(
            "data:{};base64,{}",
            source["media_type"].as_str()?,
            source["data"].as_str()?
        ),
        "url" => source["url"].as_str()?.to_string(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

fn join_text(blocks: &[Value], sep: &str) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == json!("text"))
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join(sep)
}

fn convert_tool(tool: &Value) -> Value {
    let mut function = json!({"name": tool["name"], "parameters": tool["input_schema"]});
    if let Some(desc) = tool["description"].as_str() {
        function["description"] = json!(desc);
    }
    json!({"type": "function", "function": function})
}

fn convert_tool_choice(choice: &Value) -> Value {
    match choice["type"].as_str() {
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") => json!({"type": "function", "function": {"name": choice["name"]}}),
        _ => json!("auto"),
    }
}
