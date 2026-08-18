//! Generic OAuth 2.0 authorization-code + PKCE support for providers whose
//! credential is a token rather than an API key. Every endpoint, identifier
//! and parameter comes from the provider's config, so adding another OAuth
//! provider is a preset file rather than a code change.

pub mod login;
pub mod store;

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::{OauthConfig, RefreshFormat};

pub use store::{TokenStore, Tokens};

/// Refresh this long before the access token's own expiry.
pub const REFRESH_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// RFC 7636 S256: verifier is 64 random bytes base64url-encoded, challenge is
/// the base64url-encoded SHA-256 of the verifier's ASCII.
pub fn generate_pkce() -> Pkce {
    use rand::RngCore;
    let mut bytes = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = B64.encode(bytes);
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

pub fn random_state() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    B64.encode(bytes)
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}

/// Exchange a refresh token for a fresh access token. The body encoding is
/// configurable because providers disagree: RFC 6749 says form, some want
/// JSON.
pub async fn refresh(
    client: &reqwest::Client,
    config: &OauthConfig,
    refresh_token: &str,
) -> Result<Tokens, String> {
    let endpoint = format!("{}/oauth/token", config.issuer.trim_end_matches('/'));
    let request = client.post(&endpoint);
    let request = match config.refresh_format {
        RefreshFormat::Json => request.json(&serde_json::json!({
            "client_id": config.client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        })),
        RefreshFormat::Form => request.form(&[
            ("client_id", config.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ]),
    };

    let response = request.send().await.map_err(|err| format!("refresh request failed: {err}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("refresh rejected ({status}): {}", body.trim()));
    }
    let parsed: TokenResponse =
        serde_json::from_str(&body).map_err(|err| format!("refresh response unreadable: {err}"))?;

    let access_token = parsed.access_token.ok_or("refresh response had no access_token")?;
    Ok(Tokens {
        // Providers may or may not rotate the refresh token; keep the old one
        // when they don't, or the session ends at the next refresh.
        refresh_token: parsed.refresh_token.unwrap_or_else(|| refresh_token.to_string()),
        account_id: parsed
            .id_token
            .as_deref()
            .and_then(|jwt| account_id(jwt, config.account_id_claim.as_deref())),
        expires_at: jwt_expiry(&access_token),
        id_token: parsed.id_token,
        access_token,
    })
}

/// Read the configured account-identifier claim out of an id_token. The claim
/// path is dotted, but claim *names* are often URIs containing dots, so the
/// longest matching literal key is tried at each step before splitting.
pub fn account_id(id_token: &str, claim_path: Option<&str>) -> Option<String> {
    let claims = jwt_claims(id_token)?;
    let path = claim_path?;
    lookup(&claims, path).and_then(|v| v.as_str().map(str::to_string))
}

fn lookup(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    if let Some(found) = value.get(path) {
        return Some(found.clone());
    }
    // Split at each dot, preferring the longest prefix that names a real key.
    let mut split = path.len();
    while let Some(index) = path[..split].rfind('.') {
        let (head, tail) = (&path[..index], &path[index + 1..]);
        if let Some(child) = value.get(head) {
            if let Some(found) = lookup(child, tail) {
                return Some(found);
            }
        }
        split = index;
    }
    None
}

/// `exp` claim as a wall-clock instant, if the token carries one.
pub fn jwt_expiry(token: &str) -> Option<std::time::SystemTime> {
    let exp = jwt_claims(token)?.get("exp")?.as_u64()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(exp))
}

/// Decode a JWT's payload. Signature verification is the issuer's business:
/// we received this token over TLS from the issuer and only read hints from
/// it, never trusting it for authorization.
fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = B64.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: serde_json::Value) -> String {
        format!("header.{}.signature", B64.encode(claims.to_string()))
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = generate_pkce();
        assert_eq!(pkce.challenge, B64.encode(Sha256::digest(pkce.verifier.as_bytes())));
        assert!(pkce.verifier.len() >= 43 && pkce.verifier.len() <= 128);
        assert_ne!(generate_pkce().verifier, pkce.verifier);
    }

    #[test]
    fn reads_claim_whose_name_contains_dots() {
        let token = jwt(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-123"},
        }));
        assert_eq!(
            account_id(&token, Some("https://api.openai.com/auth.chatgpt_account_id")),
            Some("acct-123".into())
        );
    }

    #[test]
    fn reads_plain_nested_claim() {
        let token = jwt(serde_json::json!({"org": {"id": "o-1"}}));
        assert_eq!(account_id(&token, Some("org.id")), Some("o-1".into()));
        assert_eq!(account_id(&token, Some("org.missing")), None);
    }

    #[test]
    fn expiry_comes_from_exp_claim() {
        let token = jwt(serde_json::json!({"exp": 1_800_000_000u64}));
        assert_eq!(
            jwt_expiry(&token),
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000))
        );
        assert_eq!(jwt_expiry("not-a-jwt"), None);
    }
}
