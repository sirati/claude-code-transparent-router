//! On-disk OAuth token storage, one file per provider next to the API-key
//! credential store, mode 0600.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// From the access token's `exp` claim, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<SystemTime>,
}

impl Tokens {
    /// True when the access token is expired or within `window` of expiring.
    /// Tokens with no expiry are treated as still valid; a 401 will trigger a
    /// refresh instead.
    pub fn needs_refresh(&self, window: std::time::Duration) -> bool {
        match self.expires_at {
            Some(expires_at) => SystemTime::now() + window >= expires_at,
            None => false,
        }
    }

    /// Never the token itself — just enough to recognise the session.
    pub fn preview(&self) -> String {
        match &self.account_id {
            Some(account) => format!("account {account}"),
            None => "signed in".to_string(),
        }
    }
}

pub struct TokenStore {
    dir: PathBuf,
}

impl TokenStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self { dir: dir.as_ref().to_path_buf() }
    }

    fn path(&self, provider: &str) -> PathBuf {
        self.dir.join(format!("{provider}.oauth.json"))
    }

    pub fn get(&self, provider: &str) -> Option<Tokens> {
        let text = std::fs::read_to_string(self.path(provider)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, provider: &str, tokens: &Tokens) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(&self.dir)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.path(provider))?;
        file.write_all(serde_json::to_string_pretty(tokens)?.as_bytes())
    }

    pub fn clear(&self, provider: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(provider)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tokens(expires_in: Option<u64>) -> Tokens {
        Tokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            id_token: None,
            account_id: Some("acct-1".into()),
            expires_at: expires_in.map(|s| SystemTime::now() + Duration::from_secs(s)),
        }
    }

    #[test]
    fn refresh_window_respected() {
        let window = Duration::from_secs(300);
        assert!(tokens(Some(60)).needs_refresh(window), "expiring inside the window");
        assert!(!tokens(Some(3600)).needs_refresh(window), "far from expiry");
        assert!(!tokens(None).needs_refresh(window), "no expiry means no forced refresh");
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("oauth-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TokenStore::new(&dir);
        assert!(store.get("p").is_none());

        store.save("p", &tokens(Some(600))).unwrap();
        let loaded = store.get("p").unwrap();
        assert_eq!(loaded.access_token, "at");
        assert_eq!(loaded.preview(), "account acct-1");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path("p")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token file must not be world readable");
        }

        store.clear("p").unwrap();
        assert!(store.get("p").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
