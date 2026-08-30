//! Defensive normalisation for content blocks replayed from Claude Code history.
//!
//! OpenAI-compatible providers cannot produce Anthropic thinking signatures.
//! A router must never replay their unsigned blocks to the Messages API: such a
//! request fails before backend or model selection.

use serde_json::{json, Value};

/// Replace unsigned thinking with ordinary text and discard empty blocks.
///
/// A valid Anthropic thinking or redacted-thinking block is retained exactly.
/// This runs before routing, protecting ordinary messages and count-token
/// requests for every backend.
pub fn sanitize_thinking(request: &mut Value) -> bool {
    let Some(messages) = request["messages"].as_array_mut() else {
        return false;
    };

    let mut changed = false;
    for message in messages {
        let Some(blocks) = message["content"].as_array_mut() else {
            continue;
        };

        let mut replacement = Vec::with_capacity(blocks.len());
        for block in std::mem::take(blocks) {
            let valid = match block["type"].as_str() {
                Some("thinking") => {
                    block["thinking"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty())
                        && block["signature"]
                            .as_str()
                            .is_some_and(|signature| !signature.is_empty())
                }
                Some("redacted_thinking") => {
                    block["data"].as_str().is_some_and(|data| !data.is_empty())
                }
                _ => true,
            };
            if valid {
                replacement.push(block);
                continue;
            }

            changed = true;
            if block["type"] == "thinking" {
                if let Some(text) = block["thinking"].as_str().filter(|text| !text.is_empty()) {
                    replacement.push(json!({"type": "text", "text": text}));
                }
            }
        }
        *blocks = replacement;
    }
    changed
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::sanitize_thinking;

    #[test]
    fn removes_empty_and_converts_unsigned_thinking() {
        let mut request = json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "", "signature": ""},
                    {"type": "thinking", "thinking": "missing signature"},
                    {"type": "thinking", "thinking": "useful", "signature": "sig"},
                    {"type": "redacted_thinking"},
                    {"type": "text", "text": "answer"}
                ]}
            ]
        });

        assert!(sanitize_thinking(&mut request));
        assert_eq!(
            request["messages"][0]["content"],
            json!([
                {"type": "text", "text": "missing signature"},
                {"type": "thinking", "thinking": "useful", "signature": "sig"},
                {"type": "text", "text": "answer"}
            ])
        );
    }

    #[test]
    fn leaves_string_content_and_valid_blocks_unchanged() {
        let mut request = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "thought", "signature": "sig"},
                    {"type": "redacted_thinking", "data": "opaque"}
                ]}
            ]
        });
        let original = request.clone();

        assert!(!sanitize_thinking(&mut request));
        assert_eq!(request, original);
    }
}
