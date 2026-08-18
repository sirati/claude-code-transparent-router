//! Recognising Claude Code's compaction request.
//!
//! `/compact` is an ordinary `POST /v1/messages`: the whole conversation is
//! replayed with the summarisation instruction appended as the final user
//! message. Measured against Claude Code 2.1.234, that message contains
//! "create a detailed summary of the conversation so far".
//!
//! The wording is Claude Code's, not an API contract, so it will drift
//! between releases. Hence a list of patterns rather than one, extendable
//! from config, and [`Override`] as the escape hatch for when a new release
//! outruns the list.

use serde_json::Value;

/// Patterns known to appear in a compaction request. Config adds to these
/// rather than replacing them, so a new release can be handled by adding one
/// line without losing the ones that still work.
pub const KNOWN_PATTERNS: &[&str] = &[
    "create a detailed summary of the conversation so far",
    "Your entire response must be plain text: an <analysis> block followed by a <summary> block",
];

/// A message the user types to let the next request through untouched, for
/// when Claude Code's wording has changed and the patterns no longer match.
pub const OVERRIDE_PHRASE: &str = "OVERRIDE_SHOULD_COMPACT";

/// Is this request Claude Code compacting the conversation? Matched on the
/// last user message, which is where the instruction is appended.
pub fn is_compaction(body: &Value, extra_patterns: &[String]) -> bool {
    let Some(text) = last_user_text(body) else { return false };
    KNOWN_PATTERNS.iter().any(|pattern| contains_ignore_case(&text, pattern))
        || extra_patterns.iter().any(|pattern| contains_ignore_case(&text, pattern))
}

/// Did the user type the override phrase, and nothing else? Deliberately
/// strict: it arms a bypass, so it should not fire on a message that merely
/// mentions it.
pub fn is_override(body: &Value) -> bool {
    last_user_text(body).is_some_and(|text| text.trim() == OVERRIDE_PHRASE)
}

/// The text of the final user message, with every text block joined.
fn last_user_text(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    let last = messages.iter().rev().find(|m| m["role"] == Value::String("user".into()))?;
    Some(match &last["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    })
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Answer the override message itself, so the user sees it took effect
/// rather than the phrase being sent to a model. Streaming is matched to the
/// request, since Claude Code will not read a plain body when it asked for
/// events.
pub fn armed_reply(request: &Value) -> axum::response::Response {
    let text = format!(
        "{OVERRIDE_PHRASE} accepted: the next request is treated as a compaction \
         and passed through untouched."
    );
    let model = request["model"].as_str().unwrap_or("claude-router");
    if request["stream"].as_bool().unwrap_or(false) {
        return crate::sse::anthropic::single_message_response(model, &text);
    }
    crate::providers::json_response(
        axum::http::StatusCode::OK,
        serde_json::json!({
            "id": "msg_router_override",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0},
        }),
    )
}

/// A one-shot bypass: armed by the override phrase, spent by the next
/// request. Shared across the daemon, since the request that arms it and the
/// request that uses it are different connections.
#[derive(Clone, Default)]
pub struct Override {
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Override {
    pub fn arm(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// True once per arming, so an override cannot silently persist.
    pub fn take(&self) -> bool {
        self.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Abridged from a real capture: Claude Code 2.1.234 running /compact.
    fn compaction_request() -> Value {
        json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": "earlier turn"},
                {"role": "assistant", "content": "earlier reply"},
                {"role": "user", "content": [
                    {"type": "text", "text": "<system-reminder>…</system-reminder>"},
                    {"type": "text", "text": "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\nYour task is to create a detailed summary of the conversation so far, paying close attention to the user's explicit requests."}
                ]},
            ],
        })
    }

    #[test]
    fn recognises_a_real_compaction_request() {
        assert!(is_compaction(&compaction_request(), &[]));
    }

    #[test]
    fn an_ordinary_turn_is_not_compaction() {
        let body = json!({"messages": [
            {"role": "user", "content": "summarise this file for me"},
        ]});
        assert!(!is_compaction(&body, &[]));
    }

    /// Wording drifts between releases, so the list is extendable.
    #[test]
    fn extra_patterns_are_matched_too() {
        let body = json!({"messages": [
            {"role": "user", "content": "Please condense the transcript into a handover note"},
        ]});
        assert!(!is_compaction(&body, &[]));
        assert!(is_compaction(&body, &["condense the transcript".to_string()]));
    }

    #[test]
    fn matching_ignores_case_and_looks_at_the_last_user_message() {
        let body = json!({"messages": [
            {"role": "user", "content": "CREATE A DETAILED SUMMARY OF THE CONVERSATION SO FAR"},
            {"role": "assistant", "content": "done"},
            {"role": "user", "content": "now something else"},
        ]});
        // The instruction is not in the final user message, so this is a
        // normal turn that happens to quote it.
        assert!(!is_compaction(&body, &[]));
    }

    #[test]
    fn override_phrase_must_stand_alone() {
        let armed = json!({"messages": [{"role": "user", "content": "  OVERRIDE_SHOULD_COMPACT\n"}]});
        assert!(is_override(&armed));

        let mentioned = json!({"messages": [
            {"role": "user", "content": "what does OVERRIDE_SHOULD_COMPACT do?"},
        ]});
        assert!(!is_override(&mentioned));
    }

    #[test]
    fn an_override_is_spent_once() {
        let bypass = Override::default();
        assert!(!bypass.take());
        bypass.arm();
        assert!(bypass.take());
        assert!(!bypass.take(), "a single arming must not let two requests through");
    }
}
