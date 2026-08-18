//! Anthropic Messages SSE event framing. The stream contract Claude Code
//! expects is `message_start` → (`content_block_start` → `content_block_delta`*
//! → `content_block_stop`)* → `message_delta` → `message_stop`, with block
//! indices assigned in order.

use serde_json::{json, Value};

/// One SSE frame: `event: <type>\ndata: <json>\n\n`.
fn frame(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

pub fn message_start(id: &str, model: &str) -> String {
    frame(
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        }),
    )
}

pub fn content_block_start(index: usize, content_block: Value) -> String {
    frame(
        "content_block_start",
        &json!({"type": "content_block_start", "index": index, "content_block": content_block}),
    )
}

pub fn text_block() -> Value {
    json!({"type": "text", "text": ""})
}

pub fn thinking_block() -> Value {
    json!({"type": "thinking", "thinking": ""})
}

pub fn tool_use_block(id: &str, name: &str) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": {}})
}

pub fn text_delta(index: usize, text: &str) -> String {
    delta(index, json!({"type": "text_delta", "text": text}))
}

pub fn thinking_delta(index: usize, thinking: &str) -> String {
    delta(index, json!({"type": "thinking_delta", "thinking": thinking}))
}

pub fn input_json_delta(index: usize, partial_json: &str) -> String {
    delta(index, json!({"type": "input_json_delta", "partial_json": partial_json}))
}

fn delta(index: usize, delta: Value) -> String {
    frame(
        "content_block_delta",
        &json!({"type": "content_block_delta", "index": index, "delta": delta}),
    )
}

pub fn content_block_stop(index: usize) -> String {
    frame("content_block_stop", &json!({"type": "content_block_stop", "index": index}))
}

pub fn message_delta(stop_reason: &str, input_tokens: u64, output_tokens: u64) -> String {
    frame(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
        }),
    )
}

pub fn message_stop() -> String {
    frame("message_stop", &json!({"type": "message_stop"}))
}

/// A whole streamed message in one response, for replies the router itself
/// produces: the frame sequence is the same one a provider would send.
pub fn single_message_response(model: &str, text: &str) -> axum::response::Response {
    let body = format!(
        "{}{}{}{}{}{}",
        message_start("msg_router", model),
        content_block_start(0, text_block()),
        text_delta(0, text),
        content_block_stop(0),
        message_delta("end_turn", 0, 0),
        message_stop(),
    );
    axum::response::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header(crate::passthrough::PROXY_ORIGIN_HEADER, crate::passthrough::PROXY_ORIGIN_VALUE)
        .body(axum::body::Body::from(body))
        .expect("router message response")
}

pub fn error(message: &str) -> String {
    frame(
        "error",
        &json!({"type": "error", "error": {"type": "api_error", "message": message}}),
    )
}
