//! Import the Claude Code CLI's own claude.ai login, so a provider whose OAuth
//! is the same one can reuse it instead of a fresh browser login. The CLI
//! keeps its session at `$CLAUDE_CONFIG_DIR/.credentials.json` (or
//! `~/.claude/.credentials.json`), under the `claudeAiOauth` key.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// The parts of the CLI's credential this router reuses. Field names match
/// what the CLI writes; the rest of the file (scopes, subscription type) is
/// deliberately ignored.
#[derive(serde::Deserialize)]
struct ClaudeCodeOauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
    /// Epoch milliseconds. Absent or non-finite means "unknown".
    #[serde(default, rename = "expiresAt")]
    expires_at: Option<f64>,
}

#[derive(serde::Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeCodeOauth>,
}

/// A usable CLI session, ready to hand to the same token store a browser
/// login fills.
pub struct Imported {
    pub access_token: String,
    pub refresh_token: String,
    /// From `expiresAt`; `None` when the CLI did not record an expiry.
    pub expires_at: Option<SystemTime>,
}

/// Read the CLI's credential when there is a usable one. The Linux path is a
/// plain file; the macOS Keychain item the CLI also keeps is not read here.
pub fn read() -> Option<Imported> {
    let path = credentials_dir().join(".credentials.json");
    let text = std::fs::read_to_string(path).ok()?;
    parse(&text)
}

/// Parse the CLI's credential file. Split from `read` so the shape is tested
/// without touching the user's home directory.
fn parse(text: &str) -> Option<Imported> {
    let file: CredentialsFile = serde_json::from_str(text).ok()?;
    let oauth = file.claude_ai_oauth?;
    if oauth.access_token.is_empty() || oauth.refresh_token.is_empty() {
        return None;
    }
    let expires_at = oauth
        .expires_at
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms as u64));
    Some(Imported {
        access_token: oauth.access_token,
        refresh_token: oauth.refresh_token,
        expires_at,
    })
}

/// `$CLAUDE_CONFIG_DIR` when set, otherwise `~/.claude` — the same directory
/// the CLI uses, so a distinct profile's credential is found.
fn credentials_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".claude"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_cli_credential_file() {
        let text = r#"{
            "claudeAiOauth": {
                "accessToken": "at",
                "refreshToken": "rt",
                "expiresAt": 1755500000000
            }
        }"#;
        let imported = parse(text).unwrap();
        assert_eq!(imported.access_token, "at");
        assert_eq!(imported.refresh_token, "rt");
        assert_eq!(
            imported.expires_at,
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_755_500_000_000))
        );
    }

    #[test]
    fn a_missing_or_empty_session_is_none() {
        assert!(parse(r#"{"claudeAiOauth": {}}"#).is_none());
        assert!(parse(r#"{"claudeAiOauth": {"accessToken":"", "refreshToken":"rt"}}"#).is_none());
        assert!(parse(r#"{"other": true}"#).is_none());
    }
}
