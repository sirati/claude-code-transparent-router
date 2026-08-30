//! OpenAI Responses reply -> Anthropic message (non-streaming).

use serde_json::{json, Value};

/// Assemble one message from an event stream, for backends that stream no
/// matter what `stream` asked for — the ChatGPT one refuses anything else.
///
/// Built from the deltas rather than the terminal event: that event is
/// supposed to carry the finished response, but the ChatGPT backend sends it
/// with an empty `output`, so trusting it yields an empty message.
pub fn from_event_stream(body: &str, alias_model: &str) -> Option<Value> {
    let mut id = None;
    let mut text = String::new();
    let mut thinking = String::new();
    // Keyed by the provider's output_index, so fragments land on the right call.
    let mut calls: std::collections::BTreeMap<u64, (String, String, String)> =
        std::collections::BTreeMap::new();
    let mut usage = json!({});
    let mut status = None;
    let mut saw_event = false;

    for line in body.lines() {
        let Some(payload) = line.trim_end().strip_prefix("data:") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Value>(payload.trim()) else {
            continue;
        };
        let Some(kind) = event["type"].as_str() else {
            continue;
        };
        saw_event = true;
        let index = event["output_index"].as_u64().unwrap_or(0);
        let delta = event["delta"].as_str().unwrap_or_default();

        match kind {
            "response.created" => id = event["response"]["id"].as_str().map(str::to_string),
            "response.output_item.added" if event["item"]["type"] == json!("function_call") => {
                let item = &event["item"];
                calls.insert(
                    index,
                    (
                        item["call_id"].as_str().unwrap_or_default().to_string(),
                        item["name"].as_str().unwrap_or_default().to_string(),
                        String::new(),
                    ),
                );
            }
            "response.output_text.delta" => text.push_str(delta),
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                thinking.push_str(delta)
            }
            "response.function_call_arguments.delta" => {
                if let Some(call) = calls.get_mut(&index) {
                    call.2.push_str(delta);
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                usage = event["response"]["usage"].clone();
                status = event["response"]["status"].as_str().map(str::to_string);
            }
            _ => {}
        }
    }
    if !saw_event {
        return None;
    }

    let mut content = Vec::new();
    if !thinking.is_empty() {
        content.push(json!({"type": "text", "text": thinking}));
    }
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    let has_calls = !calls.is_empty();
    for (index, (call_id, name, arguments)) in calls {
        let fallback = format!("toolu_{index}");
        content.push(json!({
            "type": "tool_use",
            "id": if call_id.is_empty() { fallback } else { call_id },
            "name": name,
            "input": parse_arguments(&name, &Value::String(arguments)),
        }));
    }

    Some(json!({
        "id": id.unwrap_or_else(|| "msg_routed".into()),
        "type": "message",
        "role": "assistant",
        "model": alias_model,
        "content": content,
        "stop_reason": match (has_calls, status.as_deref()) {
            (true, _) => "tool_use",
            (_, Some("incomplete")) => "max_tokens",
            _ => "end_turn",
        },
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage["input_tokens"].as_u64().unwrap_or(0),
            "output_tokens": usage["output_tokens"].as_u64().unwrap_or(0),
        },
    }))
}

pub fn to_anthropic(responses: &Value, alias_model: &str) -> Value {
    let mut content = Vec::new();
    let mut stop_reason = "end_turn";

    for (i, item) in responses["output"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        match item["type"].as_str() {
            Some("reasoning") => {
                let text = reasoning_text(item);
                if !text.is_empty() {
                    // Responses reasoning has no Anthropic signature. Preserve
                    // readable summaries as ordinary assistant text instead.
                    content.push(json!({"type": "text", "text": text}));
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
                let name = item["name"].as_str().unwrap_or_default();
                content.push(json!({
                    "type": "tool_use",
                    "id": item["call_id"].as_str().filter(|s| !s.is_empty()).unwrap_or(&fallback),
                    "name": name,
                    "input": parse_arguments(name, &item["arguments"]),
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
pub fn parse_arguments(name: &str, arguments: &Value) -> Value {
    let mut parsed = match arguments.as_str() {
        Some("") | None => json!({}),
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({"raw": s})),
    };
    crate::agent_schema::without_no_isolation(name, &mut parsed);
    parsed
}
