//! The switches of security that can be changed while the server runs (`PUT /api/v1/auth/policy`).
//!
//! Only the settings that the server reads each time they matter are here: whether the address may carry the token (`url_token`), whether refusals go to the audit
//! (`audit_denials`), whether the server may run the design program (`design_run`) and the limits of the logins and of the passwords. They start from `server.cfg`, a change
//! is kept in `policy.json` in the data folder and wins over `server.cfg` for that key the next time the server starts (`server.cfg` is never rewritten), and a change takes effect at once.
//! The other switches of `GET /auth/policy` are read from `server.cfg` once and need a restart.
//!
//! A change to a value that is looser than the recommended one is refused (`409 weak_confirm_required`) unless the request says `confirm_weak: true`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::config::ServerConfig;
use crate::store;
use crate::sync::Locked;

/// The values that can change while the server runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Hot {
    pub url_token: bool,
    pub audit_denials: bool,
    pub design_run: bool,
    pub login_ip_limit: u32,
    pub auth_max_failures: u32,
    pub auth_lockout_s: u64,
    pub auth_refresh_max_hours: u64,
    pub password_min_len: usize,
    pub password_classes: usize,
    pub password_deny_name: bool,
}

/// The keys that can be changed, in the order the policy shows them.
pub const KEYS: &[&str] = &[
    "url_token",
    "audit_denials",
    "design_run",
    "login_ip_limit",
    "auth_max_failures",
    "auth_lockout_s",
    "auth_refresh_max_hours",
    "password_min_len",
    "password_classes",
    "password_deny_name",
];

/// Why a change was not made.
#[derive(Debug, PartialEq)]
pub enum PolicyError {
    /// The key is not one that can change while the server runs, or the value is not of its type or out of its range.
    Bad(String),
    /// The value is looser than the recommended one and the request did not confirm it.
    WeakNeedsConfirm,
    /// The change could not be kept in the data folder.
    Storage,
}

fn whole(value: &Value, key: &str, low: u64, high: u64) -> Result<u64, PolicyError> {
    let n = value
        .as_u64()
        .ok_or_else(|| PolicyError::Bad(format!("{key} must be a whole number")))?;
    if n < low || n > high {
        return Err(PolicyError::Bad(format!(
            "{key} must be from {low} to {high}"
        )));
    }
    Ok(n)
}

fn flag(value: &Value, key: &str) -> Result<bool, PolicyError> {
    value
        .as_bool()
        .ok_or_else(|| PolicyError::Bad(format!("{key} must be true or false")))
}

impl Hot {
    pub fn from_config(c: &ServerConfig) -> Hot {
        Hot {
            url_token: c.url_token,
            audit_denials: c.audit_denials,
            design_run: c.design_run,
            login_ip_limit: c.login_ip_limit,
            auth_max_failures: c.auth_max_failures,
            auth_lockout_s: c.auth_lockout_s,
            auth_refresh_max_hours: c.auth_refresh_max_hours,
            password_min_len: c.password_min_len,
            password_classes: c.password_classes,
            password_deny_name: c.password_deny_name,
        }
    }

    /// The value of a key, as the policy shows it.
    pub fn value(&self, key: &str) -> Option<Value> {
        Some(match key {
            "url_token" => json!(self.url_token),
            "audit_denials" => json!(self.audit_denials),
            "design_run" => json!(self.design_run),
            "login_ip_limit" => json!(self.login_ip_limit),
            "auth_max_failures" => json!(self.auth_max_failures),
            "auth_lockout_s" => json!(self.auth_lockout_s),
            "auth_refresh_max_hours" => json!(self.auth_refresh_max_hours),
            "password_min_len" => json!(self.password_min_len),
            "password_classes" => json!(self.password_classes),
            "password_deny_name" => json!(self.password_deny_name),
            _ => return None,
        })
    }

    /// Whether the value of the key is as strict as recommended (the same rule `GET /auth/policy` uses for its `safe`).
    pub fn is_safe(&self, key: &str) -> bool {
        match key {
            "url_token" => !self.url_token,
            "audit_denials" => self.audit_denials,
            "design_run" => !self.design_run,
            "login_ip_limit" => self.login_ip_limit > 0,
            "auth_max_failures" => self.auth_max_failures <= 10,
            "auth_lockout_s" => self.auth_lockout_s >= 10,
            "auth_refresh_max_hours" => self.auth_refresh_max_hours <= 720,
            "password_min_len" => self.password_min_len >= 8,
            "password_classes" => self.password_classes >= 2,
            "password_deny_name" => self.password_deny_name,
            _ => true,
        }
    }

    /// Sets a key from a JSON value, with the same ranges `server.cfg` has.
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), PolicyError> {
        match key {
            "url_token" => self.url_token = flag(value, key)?,
            "audit_denials" => self.audit_denials = flag(value, key)?,
            "design_run" => self.design_run = flag(value, key)?,
            "login_ip_limit" => self.login_ip_limit = whole(value, key, 0, 1_000)? as u32,
            "auth_max_failures" => self.auth_max_failures = whole(value, key, 1, 100)? as u32,
            "auth_lockout_s" => self.auth_lockout_s = whole(value, key, 1, 86_400)?,
            "auth_refresh_max_hours" => self.auth_refresh_max_hours = whole(value, key, 1, 8_760)?,
            "password_min_len" => self.password_min_len = whole(value, key, 8, 128)? as usize,
            "password_classes" => self.password_classes = whole(value, key, 1, 4)? as usize,
            "password_deny_name" => self.password_deny_name = flag(value, key)?,
            other => {
                return Err(PolicyError::Bad(format!(
                    "{other} cannot be changed while the server runs"
                )))
            }
        }
        Ok(())
    }
}

/// The values in force, and the file that keeps the changes.
pub struct Policy {
    path: PathBuf,
    hot: Mutex<Hot>,
}

impl Policy {
    /// The values of `server.cfg` with the changes kept in `policy.json` of the data folder over them.
    pub fn open(cfg: &ServerConfig, dir: &Path) -> std::io::Result<Policy> {
        let path = dir.join("policy.json");
        let mut hot = Hot::from_config(cfg);
        let saved: Option<serde_json::Map<String, Value>> = store::read_json(&path)?;
        if let Some(map) = saved {
            for (key, value) in &map {
                // a key that this version no longer knows, or a value out of range, is left out: the file of the settings stands
                let _ = hot.set(key, value);
            }
        }
        Ok(Policy {
            path,
            hot: Mutex::new(hot),
        })
    }

    /// The values in force now.
    pub fn get(&self) -> Hot {
        self.hot.locked().clone()
    }

    /// Changes one key. Returns the values before and after. A looser value needs `confirm_weak`; nothing changes when it is refused or cannot be kept.
    pub fn change(
        &self,
        key: &str,
        value: &Value,
        confirm_weak: bool,
    ) -> Result<(Hot, Hot), PolicyError> {
        let mut guard = self.hot.locked();
        let before = guard.clone();
        let mut after = before.clone();
        after.set(key, value)?;
        if !after.is_safe(key) && !confirm_weak {
            return Err(PolicyError::WeakNeedsConfirm);
        }
        // only the keys that were changed here are kept, so a key nobody changed here still follows the file of the settings
        let mut kept: serde_json::Map<String, Value> = store::read_json(&self.path)
            .map_err(|_| PolicyError::Storage)?
            .unwrap_or_default();
        if let Some(v) = after.value(key) {
            kept.insert(key.to_string(), v);
        }
        store::write_json(&self.path, &kept).map_err(|_| PolicyError::Storage)?;
        *guard = after.clone();
        Ok((before, after))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eh3d-policy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn every_key_has_a_value_and_a_rule() {
        let hot = Hot::from_config(&ServerConfig::default());
        for key in KEYS {
            assert!(hot.value(key).is_some(), "{key}");
        }
        // the defaults of the server are the conservative values, except the ones the first version kept as they were
        for key in KEYS {
            if *key != "password_classes" {
                assert!(hot.is_safe(key), "{key} is safe by default");
            }
        }
        assert!(!hot.is_safe("password_classes"));
        assert!(hot.value("session_hours").is_none());
    }

    #[test]
    fn values_are_checked_against_the_ranges_of_the_settings() {
        let mut hot = Hot::from_config(&ServerConfig::default());
        assert!(hot.set("password_min_len", &json!(7)).is_err());
        assert!(hot.set("password_min_len", &json!(12)).is_ok());
        assert_eq!(hot.password_min_len, 12);
        assert!(hot.set("auth_max_failures", &json!(0)).is_err());
        assert!(hot.set("auth_lockout_s", &json!("ten")).is_err());
        assert!(hot.set("url_token", &json!(1)).is_err());
        assert!(hot.set("session_hours", &json!(5)).is_err());
        assert!(hot.set("login_ip_limit", &json!(0)).is_ok() && !hot.is_safe("login_ip_limit"));
    }

    #[test]
    fn a_looser_value_needs_the_confirmation_and_a_change_survives_a_restart() {
        let d = dir("change");
        let cfg = ServerConfig::default();
        let policy = Policy::open(&cfg, &d).unwrap();
        assert_eq!(
            policy.change("url_token", &json!(true), false),
            Err(PolicyError::WeakNeedsConfirm)
        );
        assert!(!policy.get().url_token, "nothing changed");
        assert!(!d.join("policy.json").exists(), "nothing was kept");
        let (before, after) = policy.change("url_token", &json!(true), true).unwrap();
        assert!(!before.url_token && after.url_token && policy.get().url_token);
        // stricter values need no confirmation
        assert!(policy.change("password_min_len", &json!(14), false).is_ok());
        let again = Policy::open(&cfg, &d).unwrap();
        assert!(again.get().url_token);
        assert_eq!(again.get().password_min_len, 14);
        let _ = std::fs::remove_dir_all(&d);
    }
}
