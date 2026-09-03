//! Provider-supplied system-prompt injection.
//!
//! Some models are co-trained with a particular agent harness and behave
//! markedly better when that harness's own instructions lead the system
//! prompt. The text is configuration, prepended to whatever Claude Code sent,
//! and never travels back to the client: the CLI's transcript is untouched
//! and the injection is invisible to the person driving it.
//!
//! Prepending rather than appending is deliberate. It keeps the injected text
//! at a stable offset across every turn of a conversation, so a provider that
//! caches prompt prefixes still gets a hit.

use serde_json::{json, Value};

/// Put `text` in front of the request's `system`, whatever shape it arrived
/// in. Returns whether the request was changed.
pub fn prepend(text: Option<&str>, request: &mut Value) -> bool {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return false;
    };
    let block = json!({"type": "text", "text": text});
    let replacement = match request.get("system") {
        None | Some(Value::Null) => json!([block]),
        Some(Value::String(existing)) => json!([block, {"type": "text", "text": existing}]),
        Some(Value::Array(blocks)) => {
            let mut out = vec![block];
            out.extend(blocks.iter().cloned());
            Value::Array(out)
        }
        // Anything else is a shape neither Claude Code nor the translators
        // produce; leave it rather than guess at it.
        Some(_) => return false,
    };
    request["system"] = replacement;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_system_becomes_a_single_block() {
        let mut request = json!({"messages": []});
        assert!(prepend(Some("harness rules"), &mut request));
        assert_eq!(request["system"], json!([{"type": "text", "text": "harness rules"}]));
    }

    #[test]
    fn a_string_system_is_kept_after_the_injection() {
        let mut request = json!({"system": "you are claude"});
        assert!(prepend(Some("harness rules"), &mut request));
        assert_eq!(
            request["system"],
            json!([
                {"type": "text", "text": "harness rules"},
                {"type": "text", "text": "you are claude"},
            ])
        );
    }

    /// Claude Code sends blocks carrying cache_control; those must survive
    /// intact, only pushed along by one.
    #[test]
    fn block_arrays_keep_their_metadata_and_order() {
        let mut request = json!({
            "system": [
                {"type": "text", "text": "first", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "second"},
            ]
        });
        assert!(prepend(Some("harness rules"), &mut request));
        let blocks = request["system"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["text"], "harness rules");
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(blocks[2]["text"], "second");
    }

    #[test]
    fn nothing_configured_leaves_the_request_alone() {
        let mut request = json!({"system": "unchanged"});
        assert!(!prepend(None, &mut request));
        assert!(!prepend(Some("   "), &mut request));
        assert_eq!(request["system"], "unchanged");
    }
}
