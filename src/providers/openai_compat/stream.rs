//! OpenAI SSE -> Anthropic SSE, incrementally. The CLI requires well-formed
//! `message_start` -> (`content_block_start` -> deltas -> `content_block_stop`)*
//! -> `message_delta` -> `message_stop` with sequential block indices; the
//! [`Translator`] state machine guarantees that shape whatever the provider
//! emits, and is synchronous so it can be tested without a network.

use std::convert::Infallible;

use axum::body::{Body, Bytes};
use axum::response::Response;
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use super::response::stop_reason;
use crate::passthrough::PROXY_ORIGIN_VALUE;
use crate::sse::anthropic as sse;

pub fn response(upstream: reqwest::Response, alias_model: String) -> Response {
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header(crate::passthrough::PROXY_ORIGIN_HEADER, PROXY_ORIGIN_VALUE)
        .body(Body::from_stream(translate(upstream.bytes_stream(), alias_model)))
        .expect("stream response")
}

fn translate(
    upstream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    alias_model: String,
) -> impl Stream<Item = Result<String, Infallible>> + Send {
    async_stream::stream! {
        let mut translator = Translator::new(alias_model);
        // Byte-level line buffer: chunk boundaries can split UTF-8 sequences,
        // so decoding happens per complete line, never per chunk.
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
                    // Surface the failure in-band; the CLI shows the error
                    // event instead of hanging on a half-open stream.
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

enum Open {
    Text(usize),
    Thinking(usize),
    Tool { index: usize, openai_index: u64 },
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Thinking,
}

pub struct Translator {
    alias_model: String,
    started: bool,
    done: bool,
    next_index: usize,
    open: Option<Open>,
    stop: &'static str,
    input_tokens: u64,
    output_tokens: u64,
}

impl Translator {
    pub fn new(alias_model: String) -> Self {
        Self {
            alias_model,
            started: false,
            done: false,
            next_index: 0,
            open: None,
            stop: "end_turn",
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Feed one SSE line (with or without trailing newline); Anthropic SSE
    /// frames are appended to `out`. Non-data lines are ignored.
    pub fn on_line(&mut self, line: &str, out: &mut String) {
        let Some(payload) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            self.finish(out);
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else { return };
        if self.done {
            return;
        }
        if !self.started {
            self.started = true;
            let id = chunk["id"].as_str().filter(|s| !s.is_empty()).unwrap_or("msg_routed");
            out.push_str(&sse::message_start(id, &self.alias_model));
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            if let Some(n) = usage["prompt_tokens"].as_u64() {
                self.input_tokens = n;
            }
            if let Some(n) = usage["completion_tokens"].as_u64() {
                self.output_tokens = n;
            }
        }
        let Some(choice) = chunk["choices"].get(0) else { return };
        let delta = &choice["delta"];
        if let Some(text) = delta["reasoning_content"].as_str().filter(|s| !s.is_empty()) {
            let index = self.ensure(Kind::Thinking, out);
            out.push_str(&sse::thinking_delta(index, text));
        }
        if let Some(text) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            let index = self.ensure(Kind::Text, out);
            out.push_str(&sse::text_delta(index, text));
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            self.on_tool_call(call, out);
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.stop = stop_reason(Some(reason));
        }
    }

    /// Close out the message; safe to call repeatedly. Also invoked when the
    /// provider ends the stream without a `[DONE]` sentinel.
    pub fn finish(&mut self, out: &mut String) {
        if self.done {
            return;
        }
        self.done = true;
        if !self.started {
            self.started = true;
            out.push_str(&sse::message_start("msg_routed", &self.alias_model));
        }
        self.close_open(out);
        out.push_str(&sse::message_delta(self.stop, self.input_tokens, self.output_tokens));
        out.push_str(&sse::message_stop());
    }

    fn on_tool_call(&mut self, call: &Value, out: &mut String) {
        let openai_index = call["index"].as_u64().unwrap_or(0);
        let announces_new = call["id"].as_str().is_some_and(|s| !s.is_empty())
            || call["function"]["name"].as_str().is_some_and(|s| !s.is_empty());
        let same_call =
            matches!(self.open, Some(Open::Tool { openai_index: oi, .. }) if oi == openai_index);
        if announces_new && !same_call {
            self.close_open(out);
            let index = self.next_index;
            self.next_index += 1;
            let fallback_id = format!("toolu_{index}");
            let id = call["id"].as_str().filter(|s| !s.is_empty()).unwrap_or(&fallback_id);
            let name = call["function"]["name"].as_str().unwrap_or_default();
            out.push_str(&sse::content_block_start(index, sse::tool_use_block(id, name)));
            self.open = Some(Open::Tool { index, openai_index });
        }
        if let Some(args) = call["function"]["arguments"].as_str().filter(|s| !s.is_empty()) {
            if let Some(Open::Tool { index, .. }) = self.open {
                out.push_str(&sse::input_json_delta(index, args));
            }
        }
    }

    fn ensure(&mut self, kind: Kind, out: &mut String) -> usize {
        match (&self.open, kind) {
            (Some(Open::Text(i)), Kind::Text) => *i,
            (Some(Open::Thinking(i)), Kind::Thinking) => *i,
            _ => {
                self.close_open(out);
                let index = self.next_index;
                self.next_index += 1;
                let (block, open) = match kind {
                    Kind::Text => (sse::text_block(), Open::Text(index)),
                    Kind::Thinking => (sse::thinking_block(), Open::Thinking(index)),
                };
                out.push_str(&sse::content_block_start(index, block));
                self.open = Some(open);
                index
            }
        }
    }

    fn close_open(&mut self, out: &mut String) {
        if let Some(open) = self.open.take() {
            let index = match open {
                Open::Text(i) | Open::Thinking(i) => i,
                Open::Tool { index, .. } => index,
            };
            out.push_str(&sse::content_block_stop(index));
        }
    }
}
