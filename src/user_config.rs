//! Per-user configuration for a system-wide daemon.
//!
//! One machine-level daemon should not carry a machine-level list of
//! providers: each person's providers, models and credentials are theirs, and
//! live in their own home under the usual XDG paths. So the daemon resolves
//! the connecting uid to a home directory and reads the config from there,
//! caching it until the file changes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::config::Config;

/// A config and the modification time it was read at.
type Cached = (Arc<Config>, Option<SystemTime>);

pub struct UserConfigs {
    /// uid -> most recently read config.
    cache: Mutex<HashMap<u32, Cached>>,
    /// Used for users with no config of their own, so an unconfigured user
    /// still gets working passthrough.
    fallback: Arc<Config>,
}

impl UserConfigs {
    pub fn new(fallback: Arc<Config>) -> Self {
        Self { cache: Mutex::new(HashMap::new()), fallback }
    }

    /// This user's config, re-read when the file has changed since last time.
    /// Any failure falls back rather than failing the request: a broken
    /// personal config should not take Anthropic passthrough down with it.
    pub fn get(&self, uid: u32) -> Arc<Config> {
        let Some(path) = config_path(uid) else {
            return self.fallback.clone();
        };
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        if let Ok(cache) = self.cache.lock() {
            if let Some((config, cached_mtime)) = cache.get(&uid) {
                if *cached_mtime == mtime {
                    return config.clone();
                }
            }
        }

        let config = match Config::load(Some(path.clone())) {
            Ok(config) => Arc::new(config),
            Err(err) => {
                // Missing is ordinary; malformed is worth saying out loud.
                if path.exists() {
                    tracing::warn!(uid, %err, "ignoring unreadable user config");
                }
                self.fallback.clone()
            }
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(uid, (config.clone(), mtime));
        }
        config
    }
}

/// `$XDG_CONFIG_HOME` belongs to the user's session, not the daemon's, so the
/// well-known default under their home is the only thing resolvable here.
pub fn config_path(uid: u32) -> Option<PathBuf> {
    Some(home_dir(uid)?.join(".config/claude-router/config.toml"))
}

pub fn credentials_dir(uid: u32) -> Option<PathBuf> {
    Some(home_dir(uid)?.join(".local/state/claude-router/credentials"))
}

/// Home directory for a uid. `getent` first, so NSS sources (LDAP, SSSD)
/// resolve; `/etc/passwd` as the fallback for minimal systems.
pub fn home_dir(uid: u32) -> Option<PathBuf> {
    if let Some(home) = getent_home(uid) {
        return Some(home);
    }
    passwd_home(uid, &std::fs::read_to_string("/etc/passwd").ok()?)
}

fn getent_home(uid: u32) -> Option<PathBuf> {
    let output = std::process::Command::new("getent")
        .arg("passwd")
        .arg(uid.to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    passwd_home(uid, &String::from_utf8_lossy(&output.stdout))
}

/// `name:passwd:uid:gid:gecos:home:shell`
fn passwd_home(uid: u32, passwd: &str) -> Option<PathBuf> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(uid))
            .then(|| PathBuf::from(fields[5]))
            .filter(|home| !home.as_os_str().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
                          sirati:x:1000:100:Sirati:/home/sirati:/run/current-system/sw/bin/bash\n\
                          nobody:x:65534:65534:Nobody:/var/empty:/sbin/nologin\n";

    #[test]
    fn reads_home_from_passwd() {
        assert_eq!(passwd_home(1000, PASSWD), Some(PathBuf::from("/home/sirati")));
        assert_eq!(passwd_home(0, PASSWD), Some(PathBuf::from("/root")));
        assert_eq!(passwd_home(4242, PASSWD), None);
    }

    #[test]
    fn ignores_malformed_lines() {
        assert_eq!(passwd_home(1000, "garbage\n\n:::::\n"), None);
    }

    #[test]
    fn resolves_this_users_home() {
        let uid = crate::peer::own_uid().unwrap();
        let home = home_dir(uid).expect("our own home should resolve");
        assert_eq!(Some(home.as_path()), std::env::var_os("HOME").as_ref().map(std::path::Path::new));
    }

    #[test]
    fn config_and_credentials_sit_under_the_home() {
        let uid = crate::peer::own_uid().unwrap();
        let home = home_dir(uid).unwrap();
        assert_eq!(config_path(uid).unwrap(), home.join(".config/claude-router/config.toml"));
        assert_eq!(
            credentials_dir(uid).unwrap(),
            home.join(".local/state/claude-router/credentials")
        );
    }
}
