use claude_code_transparent_router::providers::openai_compat::{request, response, stream};
use serde_json::{json, Value};

/// Collect the anthropic event types emitted for a sequence of OpenAI SSE lines.
fn run_translator(lines: &[&str]) -> (String, Vec<String>) {
    let mut translator = stream::Translator::new("anthropic/test-model".into());
    let mut out = String::new();
    for line in lines {
        translator.on_line(line, &mut out);
    }
    translator.finish(&mut out);
    let events = out
        .lines()
        .filter_map(|l| l.strip_prefix("event: "))
        .map(str::to_string)
        .collect();
    (out, events)
}

#[test]
fn text_stream_produces_wellformed_sequence() {
    let (out, events) = run_translator(&[
        r#"data: {"id":"c1","choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        r#"data: {"id":"c1","choices":[{"delta":{"content":"Hel"}}]}"#,
        r#"data: {"id":"c1","choices":[{"delta":{"content":"lo"}}]}"#,
        r#"data: {"id":"c1","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"id":"c1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
        "data: [DONE]",
    ]);
    assert_eq!(
        events,
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );
    assert!(out.contains(r#""text":"Hel"#));
    assert!(out.contains(r#""output_tokens":2"#));
    assert!(out.contains(r#""stop_reason":"end_turn""#));
}

#[test]
fn tool_call_stream_accumulates_json_deltas() {
    let (out, events) = run_translator(&[
        r#"data: {"id":"c2","choices":[{"delta":{"content":"I'll check."}}]}"#,
        r#"data: {"id":"c2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":""}}]}}]}"#,
        r#"data: {"id":"c2","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
        r#"data: {"id":"c2","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Berlin\"}"}}]}}]}"#,
        r#"data: {"id":"c2","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]);
    assert_eq!(
        events,
        [
            "message_start",
            "content_block_start", // text
            "content_block_delta",
            "content_block_stop",
            "content_block_start", // tool_use, index 1
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );
    assert!(out.contains(r#""name":"get_weather""#));
    assert!(out.contains(r#""index":1"#));
    assert!(out.contains(r#""stop_reason":"tool_use""#));
    // The two argument fragments concatenate to valid JSON.
    let fragments: String = out
        .lines()
        .filter(|l| l.contains("input_json_delta"))
        .filter_map(|l| serde_json::from_str::<Value>(l.strip_prefix("data: ")?).ok())
        .filter_map(|v| v["delta"]["partial_json"].as_str().map(str::to_string))
        .collect();
    assert_eq!(fragments, r#"{"city":"Berlin"}"#);
}

#[test]
fn reasoning_content_becomes_thinking_block() {
    let (out, events) = run_translator(&[
        r#"data: {"id":"c3","choices":[{"delta":{"reasoning_content":"hmm"}}]}"#,
        r#"data: {"id":"c3","choices":[{"delta":{"content":"answer"}}]}"#,
        "data: [DONE]",
    ]);
    assert_eq!(events[0..4], ["message_start", "content_block_start", "content_block_delta", "content_block_stop"]);
    assert!(out.contains(r#""thinking":"hmm""#));
    assert!(out.contains(r#""text":"answer""#));
}

#[test]
fn stream_without_done_still_closes() {
    let (_, events) =
        run_translator(&[r#"data: {"id":"c4","choices":[{"delta":{"content":"hi"}}]}"#]);
    assert!(events.ends_with(&["message_delta".into(), "message_stop".into()]));
}

#[test]
fn request_translation_maps_tools_and_history() {
    let anthropic = json!({
        "model": "anthropic/test-model",
        "max_tokens": 1000,
        "system": [{"type": "text", "text": "be brief"}],
        "messages": [
            {"role": "user", "content": "weather in berlin?"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Berlin"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "12C rain"}
            ]}
        ],
        "tools": [{"name": "get_weather", "description": "d", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "auto"}
    });
    let openai = request::to_openai(&anthropic, "test-model", true);
    assert_eq!(openai["model"], "test-model");
    assert_eq!(openai["stream"], json!(true));
    let messages = openai["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        json!(r#"{"city":"Berlin"}"#)
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "toolu_1");
    assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(openai["tool_choice"], "auto");
}

#[test]
fn response_translation_maps_tool_calls_and_usage() {
    let openai = json!({
        "id": "chatcmpl-1",
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": "on it",
                "tool_calls": [{"id": "call_9", "function": {"name": "f", "arguments": "{\"a\":1}"}}]
            }
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3}
    });
    let msg = response::to_anthropic(&openai, "anthropic/test-model");
    assert_eq!(msg["model"], "anthropic/test-model");
    assert_eq!(msg["stop_reason"], "tool_use");
    assert_eq!(msg["content"][0]["text"], "on it");
    assert_eq!(msg["content"][1]["input"], json!({"a": 1}));
    assert_eq!(msg["usage"]["input_tokens"], 7);
}
