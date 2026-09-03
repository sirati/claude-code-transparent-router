//! Anthropic Messages in, Anthropic Messages out, an OpenAI Responses API in
//! the middle. Used by providers whose tool calling requires `/v1/responses`
//! rather than chat-completions.

pub mod request;
pub mod response;
pub mod stream;

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::StreamExt;
use serde_json::Value;

use crate::config::{CompactionConfig, ProviderConfig};
use crate::providers::{json_response, provider_error, ProviderAuth};

/// Applies the provider's body knobs, then the compaction protocol's own on
/// top: extras merged, removals applied, and the trigger item appended last so
/// it stays the final input item.
pub fn shape_body(
    provider: &ProviderConfig,
    compaction: Option<&CompactionConfig>,
    outgoing: &mut Value,
) {
    let extra = provider
        .request_extra
        .iter()
        .chain(compaction.iter().flat_map(|c| c.request_extra.iter()));
    for (key, value) in extra {
        if let Ok(value) = serde_json::to_value(value) {
            outgoing[key.as_str()] = value;
        }
    }
    if let Some(object) = outgoing.as_object_mut() {
        let remove = provider
            .request_remove
            .iter()
            .chain(compaction.iter().flat_map(|c| c.request_remove.iter()));
        for key in remove {
            object.remove(key);
        }
    }
    // Codex marks a compaction with a trailing control item rather than a
    // prompt: the instruction itself lives on the server.
    if let Some(item) = compaction.and_then(|c| c.trigger_item.as_deref()) {
        if let Some(input) = outgoing["input"].as_array_mut() {
            input.push(serde_json::json!({"type": item}));
        }
    }
}

pub async fn messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth: ProviderAuth,
    body: Bytes,
    real_model: String,
    compaction: bool,
) -> Response {
    messages_with(client, provider, auth, body, real_model, compaction, None).await
}

/// `reminder` is prepended to the system prompt alongside the provider's own
/// harness instructions when the continuation policy asks for it.
pub async fn messages_with(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth: ProviderAuth,
    body: Bytes,
    real_model: String,
    compaction: bool,
    reminder: Option<&str>,
) -> Response {
    let mut anthropic_req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(err) => {
            return crate::passthrough::proxy_error(&format!(
                "request body is not valid JSON: {err}"
            ))
        }
    };
    // Harness instructions first, then the turn-level reminder, so the
    // injected prefix stays stable while the reminder comes and goes. Both are
    // invisible to the client: only the outgoing body carries them.
    crate::system_prompt::prepend(reminder, &mut anthropic_req);
    crate::system_prompt::prepend(provider.system_prompt.as_deref(), &mut anthropic_req);
    // The CLI gets back the alias it asked for, never the provider's own ID.
    let alias = anthropic_req["model"].as_str().unwrap_or(&real_model).to_string();
    let streaming = anthropic_req["stream"].as_bool().unwrap_or(false);
    // A provider's own compaction protocol only applies to a request the
    // router recognised as one; otherwise a compaction is an ordinary turn
    // carrying Claude Code's summarisation instruction as its last message.
    let compaction = compaction.then_some(provider.compaction.as_ref()).flatten();
    if compaction.is_some() {
        tracing::info!(provider = provider.name, "compacting via the provider's own protocol");
    }

    let mut outgoing = request::to_responses(&anthropic_req, &real_model, streaming);
    shape_body(provider, compaction, &mut outgoing);
    if let Some(level) =
        crate::effort::apply(provider.effort.as_ref(), &anthropic_req, &mut outgoing)
    {
        tracing::debug!(provider = provider.name, effort = level, "effort mapped");
    }

    // A provider may insist on streaming whatever the client asked for, in
    // which case the reply is assembled below rather than passed through.
    let upstream_streams = outgoing["stream"].as_bool().unwrap_or(streaming);

    let path = compaction
        .and_then(|c| c.path.as_deref())
        .unwrap_or("responses")
        .to_string();

    let upstream = match send(client, provider, &auth, &path, &outgoing, upstream_streams).await {
        Ok(upstream) => upstream,
        Err(response) => return *response,
    };

    // Continuation re-asks the provider with the model's own partial answer
    // appended, so it only applies to a plain turn: a compaction has its own
    // protocol, and only the streaming path can splice rounds into one message.
    let continuing = provider.continuation.enabled && compaction.is_none();

    if streaming {
        if continuing {
            return continued_stream(
                client.clone(),
                provider,
                auth,
                path,
                outgoing,
                upstream,
                alias,
            );
        }
        stream::response(upstream, alias)
    } else {
        assemble(provider, upstream, &alias).await
    }
}

/// Stream a turn that may span several provider responses.
///
/// When a round ends without a tool call and without the configured done
/// phrase, the model's own answer so far is appended to the input as an
/// assistant item and the provider is asked again. That prefill is the whole
/// trick: no user or system message is fabricated, the model simply sees its
/// own unfinished turn and carries on. The Anthropic message stays open
/// across rounds, so Claude Code sees one continuous assistant turn.
fn continued_stream(
    client: reqwest::Client,
    provider: &ProviderConfig,
    auth: ProviderAuth,
    path: String,
    outgoing: Value,
    upstream: reqwest::Response,
    alias: String,
) -> Response {
    let name = provider.name.clone();
    let base_url = provider.base_url.clone();
    let policy = provider.continuation.clone();

    let body = axum::body::Body::from_stream(async_stream::stream! {
        let mut translator = stream::Translator::new(alias);
        let mut outgoing = outgoing;
        let mut upstream = upstream;
        let mut round = 0usize;

        loop {
            // Hold the message open for every round but the last, so a
            // terminal provider event ends the round rather than the turn.
            translator.hold_open(round < policy.max_rounds);
            // Length of the answer before this round, so a round that adds
            // nothing can be told from one that made progress.
            let text_before = translator.text().len();
            let mut buf: Vec<u8> = Vec::new();
            let mut stream = std::pin::pin!(upstream.bytes_stream());
            let mut failed = false;
            while let Some(chunk) = stream.next().await {
                match stream::drain_round(&mut translator, &mut buf, chunk) {
                    Ok(out) => {
                        if !out.is_empty() {
                            yield Ok::<_, std::convert::Infallible>(out);
                        }
                    }
                    Err(message) => {
                        yield Ok(stream::error_frame(&message));
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                // The error frame said what went wrong; close the message
                // behind it so a client that keeps waiting for message_stop
                // is not left hanging on a half-open turn.
                let mut out = String::new();
                translator.hold_open(false);
                translator.finish(&mut out);
                if !out.is_empty() {
                    yield Ok(out);
                }
                return;
            }
            if translator.is_done() {
                return;
            }

            let continuable = translator.completed()
                && round < policy.max_rounds
                // No new text and no tool call means the round added nothing.
                // The prefill would be unchanged, so asking again just repeats
                // an identical request — on a rate-limited free tier that is
                // the worst possible way to spend the remaining budget.
                && translator.text().len() > text_before
                && policy.should_continue(translator.text(), translator.made_tool_call());
            if !continuable {
                break;
            }

            // Close this round's blocks; the next round opens its own.
            let mut out = String::new();
            translator.end_round(&mut out);
            if !out.is_empty() {
                yield Ok(out);
            }

            round += 1;
            tracing::info!(
                provider = name,
                round,
                words = crate::continuation::word_count(translator.text()),
                "continuing a turn that ended without the done phrase"
            );

            append_assistant_prefill(&mut outgoing, translator.text());
            let sent = client
                .post(format!("{base_url}/{path}"))
                .headers(auth.headers())
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(outgoing.to_string())
                .send()
                .await;
            upstream = match sent {
                Ok(response) if response.status().is_success() => response,
                // A refused continuation ends the turn cleanly rather than
                // raising an error mid-stream. The message is already open and
                // carries real content, so it has to be closed with the normal
                // message_delta/message_stop pair: an error frame at this point
                // would leave a client that does not treat it as terminal
                // waiting on a message_stop that never comes. Ending the turn
                // is at worst the early stop this feature exists to prevent —
                // exactly the behaviour of not having the feature at all —
                // which is a far better failure than a hang. The reason is in
                // the log, since the client cannot be told in-band.
                Ok(response) => {
                    tracing::warn!(
                        provider = name,
                        status = %response.status(),
                        round,
                        "continuation round refused; ending the turn with what the model produced"
                    );
                    break;
                }
                Err(err) => {
                    tracing::warn!(
                        provider = name,
                        %err,
                        round,
                        "continuation round failed; ending the turn with what the model produced"
                    );
                    break;
                }
            };
        }

        let mut out = String::new();
        translator.hold_open(false);
        translator.finish(&mut out);
        if !out.is_empty() {
            yield Ok(out);
        }
    });
    stream::sse_response(body)
}

/// Append the assistant's partial answer to the Responses input, so the next
/// round continues it rather than starting over.
fn append_assistant_prefill(outgoing: &mut Value, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if let Some(input) = outgoing["input"].as_array_mut() {
        input.push(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        }));
    }
}

/// One provider round-trip. Errors are already Anthropic-shaped responses,
/// boxed because that type dwarfs the success value.
async fn send(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth: &ProviderAuth,
    path: &str,
    outgoing: &Value,
    streaming: bool,
) -> Result<reqwest::Response, Box<Response>> {
    // Fresh header map from the auth material only: nothing inbound, so the
    // Anthropic credential cannot reach this provider.
    let sent = client
        .post(format!("{}/{path}", provider.base_url))
        .headers(auth.headers())
        .header("content-type", "application/json")
        .header("accept", if streaming { "text/event-stream" } else { "application/json" })
        .body(outgoing.to_string())
        .send()
        .await;

    let upstream = match sent {
        Err(err) => {
            return Err(Box::new(crate::passthrough::proxy_error(&format!(
                "provider '{}' request failed: {err}",
                provider.name
            ))))
        }
        Ok(upstream) => upstream,
    };
    let status = upstream.status();
    if !status.is_success() {
        let detail = upstream.text().await.unwrap_or_default();
        return Err(Box::new(provider_error(status, &provider.name, &detail)));
    }
    Ok(upstream)
}

async fn assemble(
    provider: &ProviderConfig,
    upstream: reqwest::Response,
    alias: &str,
) -> Response {
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            return crate::passthrough::proxy_error(&format!(
                "provider '{}' response read failed: {err}",
                provider.name
            ))
        }
    };
    let body = String::from_utf8_lossy(&bytes);
    let message = match serde_json::from_str::<Value>(&body) {
        Ok(parsed) => Some(response::to_anthropic(&parsed, alias)),
        Err(_) => response::from_event_stream(&body, alias),
    };
    match message {
        Some(message) => json_response(StatusCode::OK, message),
        None => crate::passthrough::proxy_error(&format!(
            "provider '{}' returned neither JSON nor a readable event stream",
            provider.name
        )),
    }
}

