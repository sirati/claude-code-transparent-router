//! Force-continuation: keeping a model working when it ends its turn early.
//!
//! Some models — Muse Spark in particular — narrate the tool call they are
//! about to make and then simply stop, with a well-formed `end_turn` and no
//! tool call. Claude Code cannot tell that from a finished job, so the agent
//! sits dead until a human nudges it.
//!
//! Two mechanisms, both configuration:
//!
//! * A **reminder**, prepended to the system prompt, teaching the model an
//!   explicit end-of-work signal ("answer 'Done.' and nothing else"). It is
//!   re-stated at most once every `reminder_interval_turns`, so it costs
//!   almost nothing over a long conversation.
//! * **Continuation** proper: a turn that ends without that signal, and
//!   without a tool call, is not passed to the client as finished. The router
//!   re-asks the provider with the model's own partial answer appended as an
//!   assistant item — a prefill, so no user or system message is fabricated —
//!   and splices the second reply into the same Anthropic message. Claude Code
//!   sees one continuous turn and never learns this happened.
//!
//! A turn ending in a tool call is always left alone: the client has to run
//! the tool before anything can continue.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;

/// Conversations tracked for reminder pacing. Each entry is two integers, so
/// this bound is about keeping a long-lived daemon's map from growing without
/// end rather than about size.
const MAX_TRACKED_CONVERSATIONS: usize = 512;

#[derive(Deserialize, Debug, Clone)]
pub struct ContinuationConfig {
    /// Master switch. Absent section or `enabled = false` means the provider
    /// behaves exactly as it does today.
    #[serde(default)]
    pub enabled: bool,
    /// A turn shorter than this many words counts as a stall and prompts the
    /// reminder. Continuation itself does not depend on length: any turn that
    /// ends without the done phrase is continued.
    #[serde(default = "default_min_words")]
    pub min_words: usize,
    /// What the model must say, alone, to end its work.
    #[serde(default = "default_done_phrase")]
    pub done_phrase: String,
    /// The instruction taught to the model.
    #[serde(default = "default_reminder")]
    pub reminder: String,
    /// Assistant turns that must pass before the reminder is stated again.
    #[serde(default = "default_reminder_interval")]
    pub reminder_interval_turns: u64,
    /// Hard cap on extra provider round-trips within one turn. Without it a
    /// model that never says the done phrase would be re-asked forever, which
    /// on a free tier is an expensive way to hang.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
}

fn default_min_words() -> usize {
    10
}

fn default_done_phrase() -> String {
    "Done.".into()
}

fn default_reminder() -> String {
    "if you are done with all tasks you must answer 'Done.' and nothing else".into()
}

fn default_reminder_interval() -> u64 {
    50
}

fn default_max_rounds() -> usize {
    8
}

impl Default for ContinuationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_words: default_min_words(),
            done_phrase: default_done_phrase(),
            reminder: default_reminder(),
            reminder_interval_turns: default_reminder_interval(),
            max_rounds: default_max_rounds(),
        }
    }
}

impl ContinuationConfig {
    /// Is `text` the done signal and nothing else? Compared on letters and
    /// digits only, so trailing punctuation, case and stray whitespace do not
    /// leave a finished agent spinning.
    pub fn is_done(&self, text: &str) -> bool {
        fn normalize(s: &str) -> String {
            s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
        }
        let phrase = normalize(&self.done_phrase);
        !phrase.is_empty() && normalize(text) == phrase
    }

    /// Should a turn that produced `text` and made no tool call be re-asked?
    pub fn should_continue(&self, text: &str, made_tool_call: bool) -> bool {
        // A tool call is the client's turn to act; there is nothing to
        // continue and re-asking would duplicate the call.
        self.enabled && !made_tool_call && !self.is_done(text)
    }

    /// Did this turn stall — too little said, and no tool call to explain it?
    pub fn is_stall(&self, text: &str, made_tool_call: bool) -> bool {
        !made_tool_call && word_count(text) < self.min_words
    }
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Per-conversation reminder pacing.
///
/// Claude Code replays the whole conversation every turn, so the turn number
/// is read off the request itself; only "when did we last remind this
/// conversation" needs remembering. Conversations are identified by their
/// opening user message, which is stable for the life of a session — a hash
/// collision merely makes two conversations share a counter, which costs at
/// most one skipped reminder.
#[derive(Default)]
pub struct ReminderTracker {
    seen: Mutex<HashMap<u64, u64>>,
}

impl ReminderTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide whether this request should carry the reminder, and record it.
    ///
    /// The reminder goes out when the previous turn stalled, or while the
    /// model has not yet used the done phrase at all — in both cases no more
    /// often than `reminder_interval_turns` assistant turns.
    pub fn should_remind(&self, config: &ContinuationConfig, request: &Value) -> bool {
        if !config.enabled || config.reminder.trim().is_empty() {
            return false;
        }
        let messages = request["messages"].as_array().map(Vec::as_slice).unwrap_or_default();
        let turn = messages.iter().filter(|m| m["role"] == "assistant").count() as u64;

        let stalled = last_assistant_stalled(config, messages);
        let never_signalled = !messages.iter().any(|m| {
            m["role"] == "assistant" && config.is_done(&assistant_text(&m["content"]))
        });
        if !stalled && !never_signalled {
            return false;
        }

        let key = conversation_key(request);
        let mut seen = self.seen.lock().unwrap_or_else(|err| err.into_inner());
        // A daemon that outlives many sessions would otherwise accumulate one
        // entry per conversation forever. Dropping the whole map is fine: the
        // only consequence is that the next reminder comes one interval early.
        if seen.len() >= MAX_TRACKED_CONVERSATIONS && !seen.contains_key(&key) {
            seen.clear();
        }
        match seen.get(&key) {
            Some(&last) if turn < last.saturating_add(config.reminder_interval_turns) => false,
            _ => {
                seen.insert(key, turn);
                true
            }
        }
    }
}

/// Did the conversation's most recent assistant turn stall? A turn holding a
/// tool call did not, whatever its length.
fn last_assistant_stalled(config: &ContinuationConfig, messages: &[Value]) -> bool {
    let Some(last) = messages.iter().rev().find(|m| m["role"] == "assistant") else {
        return false;
    };
    let content = &last["content"];
    let made_tool_call = content
        .as_array()
        .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_use"));
    config.is_stall(&assistant_text(content), made_tool_call)
}

/// The text of an assistant message, whichever shape it arrived in.
fn assistant_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Identify a conversation by its opening user message. Bounded so a huge
/// first message (a pasted file, say) does not make this expensive.
fn conversation_key(request: &Value) -> u64 {
    let opening = request["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|m| m["role"] == "user"))
        .map(|m| match &m["content"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    opening.as_bytes().iter().take(4096).collect::<Vec<_>>().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> ContinuationConfig {
        ContinuationConfig { enabled: true, ..Default::default() }
    }

    #[test]
    fn the_done_phrase_is_matched_loosely() {
        let config = config();
        assert!(config.is_done("Done."));
        assert!(config.is_done("done"));
        assert!(config.is_done("  DONE!  "));
        assert!(!config.is_done("Done. Now let me check the tests."));
        assert!(!config.is_done(""));
    }

    #[test]
    fn a_tool_call_is_never_continued() {
        let config = config();
        assert!(!config.should_continue("Let me look at that file.", true));
        assert!(config.should_continue("Let me look at that file.", false));
    }

    #[test]
    fn the_done_phrase_ends_the_turn() {
        let config = config();
        assert!(!config.should_continue("Done.", false));
    }

    #[test]
    fn disabled_never_continues() {
        let config = ContinuationConfig::default();
        assert!(!config.should_continue("anything at all", false));
    }

    /// The observed Muse stall: a short sentence announcing a tool call that
    /// never arrives.
    #[test]
    fn a_short_turn_without_a_tool_call_is_a_stall() {
        let config = config();
        assert!(config.is_stall("Let me check the tests.", false));
        assert!(!config.is_stall("Let me check the tests.", true));
        assert!(!config.is_stall(
            "This sentence is quite a lot longer than ten words, so it does not count as a stall.",
            false
        ));
    }

    #[test]
    fn the_reminder_is_paced_by_turn_count() {
        let tracker = ReminderTracker::new();
        let config = config();
        let conversation = |assistant_turns: usize, last: &str| {
            let mut messages = vec![json!({"role": "user", "content": "start the work"})];
            for i in 0..assistant_turns {
                let text = if i + 1 == assistant_turns { last } else { "a" };
                messages.push(json!({"role": "assistant", "content": text}));
                messages.push(json!({"role": "user", "content": "go on"}));
            }
            json!({"messages": messages})
        };

        // First sight of a stalling conversation: remind.
        assert!(tracker.should_remind(&config, &conversation(1, "short one")));
        // Two turns later it is still stalling, but the interval has not passed.
        assert!(!tracker.should_remind(&config, &conversation(3, "short one")));
        // Past the interval, state it again.
        assert!(tracker.should_remind(&config, &conversation(60, "short one")));
    }

    /// A conversation where the model already answered "Done." at some point
    /// and whose last turn is substantial needs no reminder.
    #[test]
    fn a_healthy_conversation_is_left_alone() {
        let tracker = ReminderTracker::new();
        let config = config();
        let request = json!({"messages": [
            {"role": "user", "content": "start"},
            {"role": "assistant", "content": "Done."},
            {"role": "user", "content": "next task"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "That is a long enough answer to clear the stall threshold easily."}
            ]},
        ]});
        assert!(!tracker.should_remind(&config, &request));
    }

    #[test]
    fn a_turn_holding_a_tool_call_is_not_a_stall_however_short() {
        let tracker = ReminderTracker::new();
        let config = config();
        let request = json!({"messages": [
            {"role": "user", "content": "start"},
            {"role": "assistant", "content": "Done."},
            {"role": "user", "content": "next"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "Reading."},
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {}},
            ]},
        ]});
        assert!(!tracker.should_remind(&config, &request));
    }

    #[test]
    fn disabled_never_reminds() {
        let tracker = ReminderTracker::new();
        let request = json!({"messages": [{"role": "user", "content": "start"}]});
        assert!(!tracker.should_remind(&ContinuationConfig::default(), &request));
    }

    #[test]
    fn distinct_conversations_are_paced_separately() {
        let tracker = ReminderTracker::new();
        let config = config();
        let first = json!({"messages": [{"role": "user", "content": "first conversation"}]});
        let second = json!({"messages": [{"role": "user", "content": "second conversation"}]});
        assert!(tracker.should_remind(&config, &first));
        assert!(tracker.should_remind(&config, &second));
        assert!(!tracker.should_remind(&config, &first));
    }
}
