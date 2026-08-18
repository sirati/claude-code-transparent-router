//! Credential resolution, deliberately outside the config file. Priority per
//! provider: systemd `LoadCredential` ($CREDENTIALS_DIRECTORY/<name>) > the
//! TUI-managed credential file > <NAME>_API_KEY environment fallback.

use std::fmt;
use std::path::PathBuf;

/// A provider API key. Never printed, never merged into forwarded header
/// maps — the provider module builds its outbound headers from scratch.
pub struct SecretKey(String);

impl SecretKey {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Systemd,
    File,
    Env,
    Unset,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Systemd => "systemd credential",
            Source::File => "credential store",
            Source::Env => "environment",
            Source::Unset => "not set",
        }
    }
}

pub struct CredentialStore {
    /// TUI-managed per-provider key files, mode 0600.
    dir: PathBuf,
    /// $CREDENTIALS_DIRECTORY when running under systemd.
    systemd_dir: Option<PathBuf>,
}

impl CredentialStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, systemd_dir: std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from) }
    }

    /// Reads hit the filesystem every time so a key set or cleared in the TUI
    /// takes effect on the very next request, with no shared mutable state.
    pub fn get(&self, provider: &str) -> Option<SecretKey> {
        self.read(provider).map(|(key, _)| key)
    }

    pub fn source(&self, provider: &str) -> Source {
        self.read(provider).map(|(_, source)| source).unwrap_or(Source::Unset)
    }

    /// Masked form for display: first characters shown, the rest obscured.
    pub fn preview(&self, provider: &str) -> Option<String> {
        self.read(provider).map(|(key, _)| mask(key.expose()))
    }

    pub fn set(&self, provider: &str, key: &str) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(&self.dir)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.dir.join(provider))?;
        writeln!(file, "{}", key.trim())
    }

    pub fn clear(&self, provider: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.dir.join(provider)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    fn read(&self, provider: &str) -> Option<(SecretKey, Source)> {
        if let Some(dir) = &self.systemd_dir {
            if let Some(key) = read_key_file(dir.join(provider)) {
                return Some((key, Source::Systemd));
            }
        }
        if let Some(key) = read_key_file(self.dir.join(provider)) {
            return Some((key, Source::File));
        }
        let var = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
        match std::env::var(&var) {
            Ok(key) if !key.trim().is_empty() => Some((SecretKey(key.trim().into()), Source::Env)),
            _ => None,
        }
    }
}

fn read_key_file(path: PathBuf) -> Option<SecretKey> {
    let text = std::fs::read_to_string(path).ok()?;
    let key = text.trim();
    (!key.is_empty()).then(|| SecretKey(key.to_string()))
}

/// `sk-abc123…` style masking: keep a short prefix, obscure the rest.
pub fn mask(key: &str) -> String {
    const PREFIX: usize = 8;
    const MAX_STARS: usize = 24;
    let prefix: String = key.chars().take(PREFIX).collect();
    let hidden = key.chars().count().saturating_sub(PREFIX);
    format!("{prefix}{}", "*".repeat(hidden.min(MAX_STARS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_all_but_prefix() {
        assert_eq!(mask("sk-abcdef1234567890"), "sk-abcde***********");
        assert_eq!(mask("short"), "short");
    }

    #[test]
    fn set_get_clear_roundtrip() {
        let dir = std::env::temp_dir().join(format!("cred-test-{}", std::process::id()));
        let store =
            CredentialStore { dir: dir.clone(), systemd_dir: None };
        store.set("prov", "  sk-test-key \n").unwrap();
        assert_eq!(store.get("prov").unwrap().expose(), "sk-test-key");
        assert!(matches!(store.source("prov"), Source::File));
        store.clear("prov").unwrap();
        assert!(store.get("prov").is_none());
        assert!(matches!(store.source("prov"), Source::Unset));
        let _ = std::fs::remove_dir_all(dir);
    }
}
