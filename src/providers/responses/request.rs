//! Anthropic Messages request -> OpenAI Responses request. Works on
//! `serde_json::Value` so unknown fields and block types degrade gracefully
//! instead of failing deserialization.

use serde_json::{json, Value};

pub fn to_responses(req: &Value, real_model: &str, streaming: bool) -> Value {
    let mut input = Vec::new();
    for message in req["messages"].as_array().into_iter().flatten() {
        convert_message(message, &mut input);
    }

    let mut out = json!({
        "model": real_model,
        "input": input,
        // The router never asks the provider to retain conversation state;
        // Claude Code replays the full history every turn.
        "store": false,
    });
    if let Some(instructions) = system_text(&req["system"]) {
        out["instructions"] = json!(instructions);
    }
    if let Some(max) = req.get("max_tokens").filter(|v| !v.is_null()) {
        out["max_output_tokens"] = max.clone();
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = req.get(key).filter(|v| !v.is_null()) {
            out[key] = v.clone();
        }
    }
    if let Some(tools) = req["tools"].as_array() {
        out["tools"] = tools.iter().map(convert_tool).collect();
    }
    if let Some(choice) = req.get("tool_choice").filter(|v| !v.is_null()) {
        out["tool_choice"] = convert_tool_choice(choice);
    }
    if streaming {
        out["stream"] = json!(true);
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
        Value::String(text) => out.push(text_message(role, text)),
        Value::Array(blocks) if role == "assistant" => convert_assistant(blocks, out),
        Value::Array(blocks) => convert_user(blocks, out),
        _ => {}
    }
}

fn text_message(role: &str, text: &str) -> Value {
    let part_type = if role == "assistant" { "output_text" } else { "input_text" };
    json!({
        "type": "message",
        "role": role,
        "content": [{"type": part_type, "text": text}],
    })
}

fn convert_assistant(blocks: &[Value], out: &mut Vec<Value>) {
    let text = join_text(blocks, "");
    if !text.is_empty() {
        out.push(text_message("assistant", &text));
    }
    for block in blocks {
        if block["type"] == json!("tool_use") {
            // Responses keys tool results by call_id, so the id Claude Code
            // generated for the tool_use travels through unchanged.
            out.push(json!({
                "type": "function_call",
                "call_id": block["id"],
                "name": block["name"],
                "arguments": block["input"].to_string(),
            }));
        }
    }
}

fn convert_user(blocks: &[Value], out: &mut Vec<Value>) {
    let mut parts = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("tool_result") => out.push(json!({
                "type": "function_call_output",
                "call_id": block["tool_use_id"],
                "output": tool_result_text(block),
            })),
            Some("text") => parts.push(json!({"type": "input_text", "text": block["text"]})),
            Some("image") => {
                if let Some(part) = image_part(&block["source"]) {
                    parts.push(part);
                }
            }
            _ => {}
        }
    }
    if !parts.is_empty() {
        out.push(json!({"type": "message", "role": "user", "content": parts}));
    }
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
    Some(json!({"type": "input_image", "image_url": url}))
}

fn join_text(blocks: &[Value], sep: &str) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == json!("text"))
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join(sep)
}

/// Responses puts the function's fields at the top level of the tool, unlike
/// chat-completions which nests them under `function`.
fn convert_tool(tool: &Value) -> Value {
    let mut out = json!({
        "type": "function",
        "name": tool["name"],
        "parameters": tool["input_schema"],
    });
    if let Some(desc) = tool["description"].as_str() {
        out["description"] = json!(desc);
    }
    out
}

fn convert_tool_choice(choice: &Value) -> Value {
    match choice["type"].as_str() {
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") => json!({"type": "function", "name": choice["name"]}),
        _ => json!("auto"),
    }
}
