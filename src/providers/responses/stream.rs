//! OpenAI Responses SSE -> Anthropic Messages SSE.
//!
//! Responses numbers its own output items; Anthropic numbers content blocks.
//! The translator keeps its own block counter and a map from the provider's
//! `output_index` to the block it opened, so indices stay sequential and each
//! block is closed exactly once whatever order events arrive in.

use std::collections::BTreeMap;
use std::convert::Infallible;

use axum::body::{Body, Bytes};
use axum::response::Response;
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use crate::passthrough::{PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE};
use crate::sse::anthropic as sse;

pub fn response(upstream: reqwest::Response, alias_model: String) -> Response {
    sse_response(Body::from_stream(translate(
        upstream.bytes_stream(),
        alias_model,
    )))
}

/// An SSE response carrying already-translated Anthropic frames.
pub fn sse_response(body: Body) -> Response {
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header(PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE)
        .body(body)
        .expect("stream response")
}

/// Feed one provider response into `translator`, yielding Anthropic frames as
/// they are produced. The message is not closed: the caller decides whether
/// another round follows.
pub fn drain_round(
    translator: &mut Translator,
    buf: &mut Vec<u8>,
    chunk: Result<Bytes, reqwest::Error>,
) -> Result<String, String> {
    let mut out = String::new();
    match chunk {
        Ok(bytes) => {
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                translator.on_line(&String::from_utf8_lossy(&line), &mut out);
            }
            Ok(out)
        }
        Err(err) => Err(format!("[{PROXY_ORIGIN_VALUE}] provider stream failed: {err}")),
    }
}

pub fn error_frame(message: &str) -> String {
    sse::error(message)
}

fn translate(
    upstream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    alias_model: String,
) -> impl Stream<Item = Result<String, Infallible>> + Send {
    async_stream::stream! {
        let mut translator = Translator::new(alias_model);
        // Byte-level buffer: chunk boundaries can split UTF-8 sequences, so
        // decoding happens per complete line.
        let mut buf: Vec<u8> = Vec::new();
        let mut upstream = std::pin::pin!(upstream);
        while let Some(chunk) = upstream.next().await {
            let mut out = String::new();
            match chunk {
                Ok(bytes) => {
                    buf.extend_from_slice(&bytes);
                    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        translator.on_line(&String::from_utf8_lossy(&line), &mut out);
                    }
                }
                Err(err) => {
                    out.push_str(&sse::error(&format!(
                        "[{PROXY_ORIGIN_VALUE}] provider stream failed: {err}"
                    )));
                    yield Ok::<_, Infallible>(out);
                    return;
                }
            }
            if !out.is_empty() { yield Ok(out); }
        }
        let mut out = String::new();
        translator.finish(&mut out);
        if !out.is_empty() { yield Ok(out); }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Thinking,
    Tool,
}

struct Open {
    index: usize,
    kind: Kind,
    name: Option<String>,
    arguments: String,
}

pub struct Translator {
    alias_model: String,
    started: bool,
    done: bool,
    next_index: usize,
    /// Provider `output_index` -> our open block.
    open: BTreeMap<u64, Open>,
    stop: &'static str,
    input_tokens: u64,
    output_tokens: u64,
    /// Assistant text seen across every round of this turn, for the
    /// continuation decision and for the prefill that drives the next round.
    text: String,
    /// Set when the provider ended a round without a tool call, so the turn
    /// may be continuable.
    made_tool_call: bool,
    /// True once a terminal event arrived; distinguishes "the round ended" from
    /// "the connection dropped".
    completed: bool,
    /// While set, a terminal provider event ends the round but not the
    /// Anthropic message, so another round can be spliced in after it.
    hold_open: bool,
}

impl Translator {
    pub fn new(alias_model: String) -> Self {
        Self {
            alias_model,
            started: false,
            done: false,
            next_index: 0,
            open: BTreeMap::new(),
            stop: "end_turn",
            input_tokens: 0,
            output_tokens: 0,
            text: String::new(),
            made_tool_call: false,
            completed: false,
            hold_open: false,
        }
    }

    /// Hold the Anthropic message open across provider responses, so a
    /// continuation round can be spliced into the same message.
    pub fn hold_open(&mut self, hold: bool) {
        self.hold_open = hold;
    }

    /// Everything the assistant has said this turn, across all rounds.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn made_tool_call(&self) -> bool {
        self.made_tool_call
    }

    /// Did the round end on its own terms? A dropped connection is not
    /// something to paper over with a continuation.
    pub fn completed(&self) -> bool {
        self.completed
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Finish the round without closing the Anthropic message, ready for
    /// another provider response to be fed in.
    ///
    /// Open blocks are closed — the next round's items are separate blocks —
    /// but `next_index` keeps counting, so indices stay unique across rounds,
    /// and the provider's `output_index` numbering starting over at zero
    /// cannot collide with a block from the round before.
    pub fn end_round(&mut self, out: &mut String) {
        for open in std::mem::take(&mut self.open).into_values() {
            self.emit_arguments(&open, out);
            out.push_str(&sse::content_block_stop(open.index));
        }
        self.completed = false;
    }

    /// Feed one SSE line; Anthropic frames are appended to `out`.
    pub fn on_line(&mut self, line: &str, out: &mut String) {
        let Some(payload) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            // A held-open message ends only when the caller stops continuing.
            if !self.hold_open {
                self.finish(out);
            }
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        if self.done {
            return;
        }
        let Some(kind) = event["type"].as_str() else {
            return;
        };
        let output_index = event["output_index"].as_u64().unwrap_or(0);

        match kind {
            "response.created" => {
                let id = event["response"]["id"].as_str().unwrap_or("msg_routed");
                self.start(id, out);
            }
            "response.output_item.added" => {
                self.start("msg_routed", out);
                // Reasoning items are announced before their first summary
                // delta. Opening them here would serialize an empty Anthropic
                // thinking block if the provider omits all reasoning text.
                // `ensure` opens it lazily when a nonempty delta arrives.
                if event["item"]["type"] != Value::String("reasoning".into()) {
                    self.open_item(output_index, &event["item"], out);
                }
            }
            "response.output_text.delta" => {
                if let Some(text) = event["delta"].as_str() {
                    let index = self.ensure(output_index, Kind::Text, out);
                    self.text.push_str(text);
                    out.push_str(&sse::text_delta(index, text));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(text) = event["delta"].as_str().filter(|text| !text.is_empty()) {
                    let index = self.ensure(output_index, Kind::Text, out);
                    out.push_str(&sse::text_delta(index, text));
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(fragment) = event["delta"].as_str() {
                    // The block was opened by output_item.added, which carries
                    // the call id and name; without it there is nothing to
                    // attach arguments to.
                    if let Some(open) = self.open.get_mut(&output_index) {
                        if open.name.as_deref() == Some("Agent") {
                            open.arguments.push_str(fragment);
                        } else {
                            out.push_str(&sse::input_json_delta(open.index, fragment));
                        }
                    }
                }
            }
            "response.output_item.done" => self.close(output_index, out),
            "response.completed" | "response.incomplete" => {
                self.read_usage(&event["response"]);
                if event["response"]["status"] == Value::String("incomplete".into()) {
                    self.stop = "max_tokens";
                }
                self.completed = true;
                if !self.hold_open {
                    self.finish(out);
                }
            }
            "response.failed" | "error" => {
                let message = event["response"]["error"]["message"]
                    .as_str()
                    .or_else(|| event["message"].as_str())
                    .unwrap_or("provider reported an error");
                out.push_str(&sse::error(&format!("[{PROXY_ORIGIN_VALUE}] {message}")));
                self.done = true;
            }
            _ => {}
        }
    }

    /// Close the message; safe to call repeatedly, and used when the provider
    /// ends the stream without a terminal event.
    pub fn finish(&mut self, out: &mut String) {
        if self.done {
            return;
        }
        self.done = true;
        self.start("msg_routed", out);
        for open in std::mem::take(&mut self.open).into_values() {
            self.emit_arguments(&open, out);
            out.push_str(&sse::content_block_stop(open.index));
        }
        out.push_str(&sse::message_delta(
            self.stop,
            self.input_tokens,
            self.output_tokens,
        ));
        out.push_str(&sse::message_stop());
    }

    fn start(&mut self, id: &str, out: &mut String) {
        if !self.started {
            self.started = true;
            out.push_str(&sse::message_start(id, &self.alias_model));
        }
    }

    fn open_item(&mut self, output_index: u64, item: &Value, out: &mut String) {
        if self.open.contains_key(&output_index) {
            return;
        }
        // A block can never precede message_start, even when the provider
        // sends deltas for an item it never announced.
        self.start("msg_routed", out);
        let (kind, block, name) = match item["type"].as_str() {
            Some("function_call") => {
                self.stop = "tool_use";
                self.made_tool_call = true;
                let fallback = format!("toolu_{}", self.next_index);
                let id = item["call_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&fallback)
                    .to_string();
                let name = item["name"].as_str().unwrap_or_default().to_string();
                (Kind::Tool, sse::tool_use_block(&id, &name), Some(name))
            }
            Some("reasoning") => (Kind::Thinking, sse::thinking_block(), None),
            _ => (Kind::Text, sse::text_block(), None),
        };
        let index = self.next_index;
        self.next_index += 1;
        out.push_str(&sse::content_block_start(index, block));
        self.open.insert(
            output_index,
            Open {
                index,
                kind,
                name,
                arguments: String::new(),
            },
        );
    }

    /// Deltas can arrive for an item the provider never announced; open a
    /// block of the right kind rather than dropping the content.
    fn ensure(&mut self, output_index: u64, kind: Kind, out: &mut String) -> usize {
        if let Some(open) = self.open.get(&output_index) {
            if open.kind == kind {
                return open.index;
            }
        }
        let item = match kind {
            Kind::Tool => serde_json::json!({"type": "function_call"}),
            _ => serde_json::json!({"type": "message"}),
        };
        self.close(output_index, out);
        self.open_item(output_index, &item, out);
        self.open[&output_index].index
    }

    fn close(&mut self, output_index: u64, out: &mut String) {
        if let Some(open) = self.open.remove(&output_index) {
            self.emit_arguments(&open, out);
            out.push_str(&sse::content_block_stop(open.index));
        }
    }

    fn emit_arguments(&self, open: &Open, out: &mut String) {
        let Some(name) = &open.name else {
            return;
        };
        if name != "Agent" {
            return;
        }
        let mut input = serde_json::from_str(&open.arguments)
            .unwrap_or_else(|_| serde_json::json!({"raw": open.arguments}));
        crate::agent_schema::without_no_isolation(name, &mut input);
        out.push_str(&sse::input_json_delta(open.index, &input.to_string()));
    }

    /// Usage accumulates: a continued turn is several provider responses
    /// reported to Claude Code as one message, so its token counts are the
    /// sum of every round's.
    fn read_usage(&mut self, response: &Value) {
        if let Some(n) = response["usage"]["input_tokens"].as_u64() {
            self.input_tokens += n;
        }
        if let Some(n) = response["usage"]["output_tokens"].as_u64() {
            self.output_tokens += n;
        }
    }
}
