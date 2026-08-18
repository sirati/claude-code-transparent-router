use claude_code_transparent_router::providers::responses;
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

/// The ChatGPT backend refuses `stream: false`, then sends a terminal event
/// whose `output` is empty — so a non-streaming caller can only be served by
/// rebuilding the message from the deltas.
#[test]
fn non_streaming_reply_is_assembled_from_the_event_stream() {
    let body = [
        r#"data: {"type":"response.created","response":{"id":"resp_9"}}"#,
        r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"message"}}"#,
        r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"Hel"}"#,
        r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"lo"}"#,
        r#"data: {"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call_1","name":"get_weather"}}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"city\":"}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":1,"delta":"\"Berlin\"}"}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":11,"output_tokens":5}}}"#,
    ]
    .join("\n");

    let message = response::from_event_stream(&body, "codex/sol").unwrap();
    assert_eq!(message["id"], "resp_9");
    assert_eq!(message["model"], "codex/sol");
    assert_eq!(message["content"][0]["text"], "Hello");
    assert_eq!(message["content"][1]["name"], "get_weather");
    assert_eq!(message["content"][1]["input"], json!({"city": "Berlin"}));
    assert_eq!(message["stop_reason"], "tool_use");
    assert_eq!(message["usage"]["input_tokens"], 11);

    // Nothing resembling an event stream: say so rather than invent a reply.
    assert!(response::from_event_stream("not a stream", "codex/sol").is_none());
}

/// Claude Code puts `system` messages inside the conversation from the second
/// turn onwards; the ChatGPT backend answers "System messages are not
/// allowed", so they have to become `developer` messages.
#[test]
fn system_messages_become_developer_messages() {
    let anthropic = json!({
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "system", "content": "ambient context, not from the user"},
            {"role": "user", "content": "who are you"},
        ],
    });
    let out = request::to_responses(&anthropic, "gpt-5.6-sol", true);
    let input = out["input"].as_array().unwrap();

    let roles: Vec<&str> = input.iter().filter_map(|i| i["role"].as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "developer", "user"]);
    assert!(!input.iter().any(|i| i["role"] == "system"), "{input:#?}");
    assert_eq!(input[2]["content"][0]["text"], "ambient context, not from the user");
}

/// A user message may carry guidance either side of a tool result, and Claude
/// Code relies on that to keep roles alternating. Emitting the items in source
/// order keeps every part, in place.
#[test]
fn text_around_a_tool_result_survives_in_order() {
    let anthropic = json!({
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "before"},
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": "the output"},
            {"type": "text", "text": "actually, stop and do something else"},
        ]}],
    });
    let out = request::to_responses(&anthropic, "gpt-5.6-sol", true);
    let input = out["input"].as_array().unwrap();

    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["content"][0]["text"], "before");
    assert_eq!(input[1]["type"], "function_call_output");
    assert_eq!(input[1]["call_id"], "toolu_1");
    assert_eq!(input[2]["content"][0]["text"], "actually, stop and do something else");
}

/// A provider may want a compaction sent its own way. The knobs are config,
/// not code: extras and removals stack on the provider's own, and the trigger
/// item is appended last so it stays the final input item.
#[test]
fn compaction_protocol_reshapes_the_body() {
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compaction.toml");
    let config = claude_code_transparent_router::config::Config::load(Some(fixture)).unwrap();
    let provider = config.providers.iter().find(|p| p.compaction.is_some()).unwrap();

    let anthropic = json!({"messages": [{"role": "user", "content": "summarise"}], "stream": true});
    let mut ordinary = request::to_responses(&anthropic, "gamma-model", true);
    responses::shape_body(provider, None, &mut ordinary);
    assert_eq!(ordinary["store"], json!(false));
    assert_eq!(ordinary["stream"], json!(true));
    assert_eq!(ordinary["input"].as_array().unwrap().len(), 1);

    let mut compacting = request::to_responses(&anthropic, "gamma-model", true);
    responses::shape_body(provider, provider.compaction.as_ref(), &mut compacting);
    let object = compacting.as_object().unwrap();
    for dropped in ["tool_choice", "store", "stream", "include"] {
        assert!(!object.contains_key(dropped), "{dropped} survived: {compacting:#?}");
    }
    assert_eq!(compacting["parallel_tool_calls"], json!(true));
    let input = compacting["input"].as_array().unwrap();
    assert_eq!(input.last().unwrap(), &json!({"type": "compaction_trigger"}));
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
