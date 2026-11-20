//! The contract of the settings of the server (`GET /api/v1/settings/contract`): every key of `server.cfg` with its type, range, default, group, whether it changes while the server
//! runs, which route changes it and its label, short help and warning in the eight languages.
//!
//! The document is `docs/settings-contract.json`, written by `tools/gen_settings_contract.py` from the code (the keys `config::parse` accepts, the defaults and `policy::KEYS`) and
//! from the texts of `tools/settings_contract_texts.py`; the server embeds it, so the contract a build serves is the one it was built with. The clients copy the same file and check
//! it against the server. A key, a switch or a route that is added needs its entry in the same change (`docs/CLIENT_SYNC.md`).
//!
//! The values in force go with the contract in the answer, never the value of a key the contract marks as `secret`.

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::config::ServerConfig;
use crate::policy::Hot;

/// The document of the contract, as the generator wrote it.
pub const CONTRACT_JSON: &str = include_str!("../docs/settings-contract.json");

/// The contract, read once.
pub fn contract() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| {
        serde_json::from_str(CONTRACT_JSON).expect("docs/settings-contract.json is a JSON document")
    })
}

/// The settings of the contract, in the order of the document.
pub fn entries() -> &'static [Value] {
    match contract()["settings"].as_array() {
        Some(list) => list.as_slice(),
        None => &[],
    }
}

/// The entry of a key, if the contract has it.
pub fn entry(key: &str) -> Option<&'static Value> {
    entries().iter().find(|e| e["key"] == key)
}

/// Whether the value of the key is never given back (not in the values, and written as `***` in the audit).
pub fn is_secret(key: &str) -> bool {
    entry(key).is_some_and(|e| e["secret"] == true)
}

/// Whether a change of the key is read only when the server starts.
pub fn applies_at_restart(key: &str) -> bool {
    entry(key).is_some_and(|e| e["apply"] == "restart")
}

/// A value as the audit and the answers show it: a secret one is hidden.
pub fn shown(key: &str, value: &Value) -> Value {
    if is_secret(key) {
        json!("***")
    } else {
        value.clone()
    }
}

/// The value in force of every key of the contract that is not secret. A key that the policy changes follows the policy; the others are what `server.cfg` set at the start.
/// A path that is not set is an empty text and a list that is not set is an empty list.
pub fn values(cfg: &ServerConfig, hot: &Hot) -> Value {
    let from_file = serde_json::to_value(cfg).unwrap_or(Value::Null);
    let mut out = Map::new();
    for e in entries() {
        let Some(key) = e["key"].as_str() else {
            continue;
        };
        if e["secret"] == true {
            continue;
        }
        let value = if key == "local_until_setup" {
            json!(cfg.local_until_setup_in_force(hot.network_open))
        } else if let Some(v) = hot.value(key) {
            v
        } else {
            match from_file.get(key) {
                Some(v) if !v.is_null() => v.clone(),
                _ if e["type"].as_str() == Some("list") => json!([]),
                _ => json!(""),
            }
        };
        out.insert(key.to_string(), value);
    }
    Value::Object(out)
}

/// What `GET /api/v1/settings/contract` answers: the contract, the version of this server and the values in force.
pub fn document(cfg: &ServerConfig, hot: &Hot) -> Value {
    let mut doc = contract().clone();
    doc["server_version"] = json!(env!("CARGO_PKG_VERSION"));
    doc["values"] = values(cfg, hot);
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy;

    fn sources() -> (String, String) {
        let config = include_str!("config.rs");
        (
            config.replace("\r\n", "\n"),
            include_str!("../server.cfg.example").replace("\r\n", "\n"),
        )
    }

    /// The keys `config::parse` accepts, read from its source.
    fn parsed_keys(config: &str) -> Vec<String> {
        let start = config.find("match key.as_str() {").unwrap();
        let end = start + config[start..].find("other => return Err(at(").unwrap();
        let mut keys = Vec::new();
        for line in config[start..end].lines() {
            let line = line.trim_start();
            if let Some(rest) = line.strip_prefix('"') {
                if let Some((name, after)) = rest.split_once('"') {
                    if after.trim_start().starts_with("=>")
                        && !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    {
                        keys.push(name.to_string());
                    }
                }
            }
        }
        keys
    }

    #[test]
    fn the_contract_is_the_versioned_document_with_the_eight_languages() {
        let doc = contract();
        assert_eq!(doc["schema"], "eh3d.settings.contract/1");
        let langs: Vec<&str> = doc["languages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(langs, ["en", "es", "de", "fr", "it", "ja", "zh", "pt"]);
        assert!(doc["groups"].as_array().unwrap().len() >= 10);
        assert_eq!(doc["write"]["policy"]["path"], "/api/v1/auth/policy");
        assert_eq!(doc["read"]["path"], "/api/v1/settings/contract");
    }

    #[test]
    fn every_key_of_the_code_is_in_the_contract_and_the_other_way_round() {
        let (config, example) = sources();
        let in_code = parsed_keys(&config);
        assert!(
            in_code.len() > 80,
            "the scan finds the keys: {}",
            in_code.len()
        );
        let in_contract: Vec<&str> = entries()
            .iter()
            .map(|e| e["key"].as_str().unwrap())
            .collect();
        for key in &in_code {
            assert!(
                in_contract.contains(&key.as_str()),
                "{key} is a key of server.cfg and is not in docs/settings-contract.json (python tools/gen_settings_contract.py)"
            );
            assert!(
                example.contains(&format!("{key} =")),
                "{key} is not in server.cfg.example"
            );
        }
        for key in &in_contract {
            assert!(
                in_code.iter().any(|k| k == key),
                "{key} is in the contract and server.cfg does not accept it"
            );
        }
        let mut sorted = in_contract.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), in_contract.len(), "a key is listed twice");
    }

    #[test]
    fn every_key_of_the_policy_is_a_setting_that_the_policy_writes() {
        for key in policy::KEYS {
            let e = entry(key).unwrap_or_else(|| panic!("{key} is not in the contract"));
            assert_eq!(e["write"]["via"], "policy", "{key}");
            assert_eq!(e["write"]["method"], "PUT", "{key}");
            assert_eq!(e["write"]["path"], "/api/v1/auth/policy", "{key}");
            assert_eq!(e["role"], "admin", "{key}");
            assert_eq!(e["scope"], "admin:policy", "{key}");
            assert_eq!(e["confirm_when_looser"], true, "{key}");
            let restart = policy::RESTART_KEYS.contains(key);
            assert_eq!(applies_at_restart(key), restart, "{key}");
        }
        for e in entries() {
            let key = e["key"].as_str().unwrap();
            if !policy::KEYS.contains(&key) {
                assert_eq!(e["write"]["via"], "file", "{key}");
                assert!(
                    e["write"]["method"].is_null() && e["role"].is_null(),
                    "{key}"
                );
                assert!(
                    applies_at_restart(key),
                    "{key} is read when the server starts"
                );
            }
        }
    }

    #[test]
    fn every_setting_has_its_type_default_and_the_texts_in_the_eight_languages() {
        let cfg = ServerConfig::default();
        let hot = Hot::from_config(&cfg);
        let shown_values = values(&cfg, &hot);
        for e in entries() {
            let key = e["key"].as_str().unwrap();
            assert!(
                ["boolean", "integer", "string", "list"].contains(&e["type"].as_str().unwrap()),
                "{key}"
            );
            assert!(!e["default"].is_null(), "{key} has a default");
            // the default of the contract is what the server starts with
            assert_eq!(shown_values[key], e["default"], "{key}");
            if e["type"] == "integer" {
                let (lo, hi, d) = (
                    e["min"].as_u64().unwrap(),
                    e["max"].as_u64().unwrap(),
                    e["default"].as_u64().unwrap(),
                );
                assert!(lo <= hi, "{key}");
                // a key that turns a limit off (0) has 0 in its range; the others start inside it
                assert!(d >= lo && d <= hi, "{key}: {d} is not from {lo} to {hi}");
            }
            for what in ["label", "help", "warning"] {
                for lang in doc_languages() {
                    let text = e[what][lang.as_str()].as_str().unwrap_or("");
                    assert!(!text.trim().is_empty(), "{key}: no {what} in {lang}");
                }
            }
            assert!(
                ["none", "security", "exposure", "capacity", "machine", "data"]
                    .contains(&e["risk"].as_str().unwrap()),
                "{key}"
            );
        }
    }

    fn doc_languages() -> Vec<String> {
        contract()["languages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_groups_are_named_in_the_eight_languages_and_every_setting_belongs_to_one() {
        let groups: Vec<&str> = contract()["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| {
                for lang in doc_languages() {
                    assert!(g["label"][lang.as_str()]
                        .as_str()
                        .is_some_and(|t| !t.is_empty()));
                }
                g["id"].as_str().unwrap()
            })
            .collect();
        for e in entries() {
            let group = e["group"].as_str().unwrap();
            assert!(
                groups.contains(&group),
                "{} is in an unknown group",
                e["key"]
            );
        }
    }

    #[test]
    fn the_values_follow_the_policy_and_leave_out_a_secret_key() {
        let cfg = ServerConfig::default();
        let mut hot = Hot::from_config(&cfg);
        hot.url_token = true;
        hot.password_min_len = 14;
        let doc = document(&cfg, &hot);
        assert_eq!(doc["values"]["url_token"], true);
        assert_eq!(doc["values"]["password_min_len"], 14);
        assert_eq!(doc["values"]["session_hours"], 12);
        assert_eq!(doc["values"]["listen"], "127.0.0.1:18200");
        assert_eq!(doc["values"]["studio_dir"], "");
        assert_eq!(doc["values"]["auth_extra_apps"], json!([]));
        assert_eq!(doc["server_version"], env!("CARGO_PKG_VERSION"));
        // the contract marks no key of server.cfg as secret today; the rule is that one that is marked never has a value and is hidden in the audit
        let count = entries().len();
        assert_eq!(
            doc["values"].as_object().unwrap().len(),
            count - entries().iter().filter(|e| e["secret"] == true).count()
        );
        assert!(!is_secret("password_min_len"));
        assert_eq!(shown("password_min_len", &json!(14)), json!(14));
    }
}
