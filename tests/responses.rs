use claude_code_transparent_router::providers::responses::{request, response, stream};
use serde_json::{json, Value};

fn run_translator(lines: &[&str]) -> (String, Vec<String>) {
    let mut translator = stream::Translator::new("anthropic/test-model".into());
    let mut out = String::new();
    for line in lines {
        translator.on_line(line, &mut out);
    }
    translator.finish(&mut out);
    let events =
        out.lines().filter_map(|l| l.strip_prefix("event: ")).map(str::to_string).collect();
    (out, events)
}

#[test]
fn text_stream_produces_wellformed_sequence() {
    let (out, events) = run_translator(&[
        r#"data: {"type":"response.created","response":{"id":"resp_1"}}"#,
        r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"message"}}"#,
        r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"Hel"}"#,
        r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"lo"}"#,
        r#"data: {"type":"response.output_item.done","output_index":0}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":11,"output_tokens":2}}}"#,
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
    assert!(out.contains(r#""input_tokens":11"#));
}

#[test]
fn reasoning_then_tool_call_keeps_indices_sequential() {
    let (out, events) = run_translator(&[
        r#"data: {"type":"response.created","response":{"id":"resp_2"}}"#,
        r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning"}}"#,
        r#"data: {"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"pondering"}"#,
        r#"data: {"type":"response.output_item.done","output_index":0}"#,
        r#"data: {"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call_a","name":"get_weather"}}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"city\":"}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":1,"delta":"\"Berlin\"}"}"#,
        r#"data: {"type":"response.output_item.done","output_index":1}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
    ]);
    assert_eq!(events.iter().filter(|e| *e == "content_block_start").count(), 2);
    assert!(out.contains(r#""thinking":"pondering""#));
    assert!(out.contains(r#""name":"get_weather""#));
    assert!(out.contains(r#""index":1"#));
    assert!(out.contains(r#""stop_reason":"tool_use""#));

    let fragments: String = out
        .lines()
        .filter(|l| l.contains("input_json_delta"))
        .filter_map(|l| serde_json::from_str::<Value>(l.strip_prefix("data: ")?).ok())
        .filter_map(|v| v["delta"]["partial_json"].as_str().map(str::to_string))
        .collect();
    assert_eq!(fragments, r#"{"city":"Berlin"}"#);
}

#[test]
fn incomplete_response_maps_to_max_tokens() {
    let (out, _) = run_translator(&[
        r#"data: {"type":"response.created","response":{"id":"r"}}"#,
        r#"data: {"type":"response.incomplete","response":{"status":"incomplete"}}"#,
    ]);
    assert!(out.contains(r#""stop_reason":"max_tokens""#));
}

#[test]
fn stream_error_surfaces_in_band() {
    let (out, events) = run_translator(&[
        r#"data: {"type":"response.created","response":{"id":"r"}}"#,
        r#"data: {"type":"response.failed","response":{"error":{"message":"upstream exploded"}}}"#,
    ]);
    assert!(events.contains(&"error".to_string()));
    assert!(out.contains("upstream exploded"));
}

#[test]
fn unannounced_deltas_still_open_a_block() {
    let (out, events) =
        run_translator(&[r#"data: {"type":"response.output_text.delta","output_index":3,"delta":"hi"}"#]);
    assert_eq!(events[0], "message_start");
    assert!(events.contains(&"content_block_start".to_string()));
    assert!(out.contains(r#""text":"hi""#));
}

#[test]
fn request_translation_maps_tools_and_history() {
    let anthropic = json!({
        "model": "anthropic/test-model",
        "max_tokens": 1000,
        "system": "be brief",
        "messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Berlin"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "12C rain"}
            ]}
        ],
        "tools": [{"name": "get_weather", "description": "d", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "any"}
    });
    let out = request::to_responses(&anthropic, "real-model", true);

    assert_eq!(out["model"], "real-model");
    assert_eq!(out["instructions"], "be brief");
    assert_eq!(out["max_output_tokens"], 1000);
    assert_eq!(out["store"], json!(false));
    assert_eq!(out["stream"], json!(true));
    // Tools are flat in Responses, not nested under "function".
    assert_eq!(out["tools"][0]["name"], "get_weather");
    assert_eq!(out["tool_choice"], "required");

    let input = out["input"].as_array().unwrap();
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(input[1]["content"][0]["type"], "output_text");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "toolu_1");
    assert_eq!(input[2]["arguments"], json!(r#"{"city":"Berlin"}"#));
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "toolu_1");
}

#[test]
fn response_translation_maps_items_and_usage() {
    let responses = json!({
        "id": "resp_9",
        "status": "completed",
        "output": [
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "thought"}]},
            {"type": "message", "content": [{"type": "output_text", "text": "answer"}]},
            {"type": "function_call", "call_id": "call_9", "name": "f", "arguments": "{\"a\":1}"}
        ],
        "usage": {"input_tokens": 7, "output_tokens": 3}
    });
    let msg = response::to_anthropic(&responses, "anthropic/test-model");
    assert_eq!(msg["model"], "anthropic/test-model");
    assert_eq!(msg["content"][0]["thinking"], "thought");
    assert_eq!(msg["content"][1]["text"], "answer");
    assert_eq!(msg["content"][2]["input"], json!({"a": 1}));
    assert_eq!(msg["stop_reason"], "tool_use");
    assert_eq!(msg["usage"]["input_tokens"], 7);
}
