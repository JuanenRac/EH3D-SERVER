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
    values_of(entries(), cfg, hot)
}

/// [`values`] over any list of entries (the tests feed it entries that mark a key as secret).
pub fn values_of(list: &[Value], cfg: &ServerConfig, hot: &Hot) -> Value {
    let from_file = serde_json::to_value(cfg).unwrap_or(Value::Null);
    let mut out = Map::new();
    for e in list {
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
    doc["contract_hash"] = json!(contract_hash());
    doc["values"] = values(cfg, hot);
    doc
}

/// The schema of the light, read-only answer of `GET /api/v1/settings/values`.
pub const VALUES_SCHEMA: &str = "eh3d.settings.values/1";
/// What stands in the place of the value of a key the contract marks as `secret`.
pub const SECRET_MARK: &str = "***";
/// The longest text of a value in the light answer; a longer one is cut and `truncated` is true.
pub const MAX_TEXT_CHARS: usize = 120;
/// The most items of a list in the light answer, and the longest item.
pub const MAX_LIST_ITEMS: usize = 16;
pub const MAX_ITEM_CHARS: usize = 64;
/// The biggest light answer, in bytes. The bounds above keep every answer well under it; the tests build the worst case.
pub const VALUES_MAX_BYTES: usize = 16 * 1024;
/// The longest query the light answer reads, in bytes.
pub const QUERY_MAX_BYTES: usize = 2048;

/// The SHA-256 (lower case hex) of the contract document with its line ends as `\n`, so that it is the same on every machine whatever the checkout did to the file. A client that
/// keeps a copy of the contract asks again for the whole document only when this changes.
pub fn contract_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            CONTRACT_JSON.replace("\r\n", "\n").as_bytes(),
        ))
    })
}

/// Which keys the light answer lists: all, the ones in `keys`, or the ones of a group.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub keys: Option<Vec<String>>,
    pub group: Option<String>,
}

/// Percent-decodes a part of a query (`+` is a space); `None` when it is not well formed or not UTF-8.
fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let pair = raw.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(pair, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Reads the query of `GET /api/v1/settings/values` against a list of entries: `keys=a,b` (keys of the contract) and `group=id`. A parameter
/// the route does not know is ignored; a key or a group that the contract does not have, a query that does not decode and a query that is too long are refused with a text that
/// says what was wrong.
pub fn parse_selection(query: Option<&str>, list: &[Value]) -> Result<Selection, String> {
    let mut selection = Selection::default();
    let query = query.unwrap_or("");
    if query.len() > QUERY_MAX_BYTES {
        return Err(format!("the query is longer than {QUERY_MAX_BYTES} bytes"));
    }
    for part in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        let (Some(name), Some(value)) = (percent_decode(name), percent_decode(value)) else {
            return Err("the query is not well formed".to_string());
        };
        match name.as_str() {
            "keys" => {
                let mut keys: Vec<String> = Vec::new();
                for key in value.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                    if !list.iter().any(|e| e["key"] == key) {
                        return Err(format!(
                            "{} is not a setting of the server",
                            clip(key, 40).0
                        ));
                    }
                    if !keys.iter().any(|k| k == key) {
                        keys.push(key.to_string());
                    }
                }
                selection.keys = Some(keys);
            }
            "group" => {
                let known = contract()["groups"]
                    .as_array()
                    .is_some_and(|g| g.iter().any(|g| g["id"] == value.as_str()));
                if !known {
                    return Err(format!("{} is not a group of settings", clip(&value, 40).0));
                }
                selection.group = Some(value);
            }
            _ => {}
        }
    }
    Ok(selection)
}

/// A text cut at `max` characters, and whether it was cut.
fn clip(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        (text.to_string(), false)
    } else {
        (text.chars().take(max).collect(), true)
    }
}

/// The value of a key as the light answer shows it: a long text or list is cut.
fn light_value(value: &Value) -> (Value, bool) {
    match value {
        Value::String(t) => {
            let (text, cut) = clip(t, MAX_TEXT_CHARS);
            (json!(text), cut)
        }
        Value::Array(items) => {
            let mut cut = items.len() > MAX_LIST_ITEMS;
            let list: Vec<Value> = items
                .iter()
                .take(MAX_LIST_ITEMS)
                .map(|item| match item.as_str() {
                    Some(t) => {
                        let (text, was) = clip(t, MAX_ITEM_CHARS);
                        cut |= was;
                        json!(text)
                    }
                    None => item.clone(),
                })
                .collect();
            (json!(list), cut)
        }
        other => (other.clone(), false),
    }
}

/// The light, read-only answer of `GET /api/v1/settings/values`: for each key, the value in force (`v`), where it comes from (`origin`: `override` when a change through the
/// security page differs from `server.cfg`, `file` otherwise, the built-in default included) and whether the security page changes it (`editable`). A key the contract marks
/// as `secret` shows [`SECRET_MARK`] and `secret: true`, never its value; a key that is a path of the computer (`format: path`) has `hidden: true` and no value. No text of the
/// contract is in it: it is a few kilobytes, and `contract_hash` says whether the copy of the contract that a client holds is still the one of this server.
pub fn values_view(cfg: &ServerConfig, hot: &Hot, selection: &Selection) -> Value {
    values_view_of(entries(), cfg, hot, selection)
}

/// [`values_view`] over any list of entries.
pub fn values_view_of(
    list: &[Value],
    cfg: &ServerConfig,
    hot: &Hot,
    selection: &Selection,
) -> Value {
    let shown = values_of(list, cfg, hot);
    let base = Hot::from_config(cfg);
    let mut out = Map::new();
    for e in list {
        let Some(key) = e["key"].as_str() else {
            continue;
        };
        if let Some(wanted) = &selection.keys {
            if !wanted.iter().any(|k| k == key) {
                continue;
            }
        }
        if let Some(group) = &selection.group {
            if e["group"] != group.as_str() {
                continue;
            }
        }
        let mut item = Map::new();
        if e["secret"] == true {
            item.insert("v".into(), json!(SECRET_MARK));
            item.insert("secret".into(), json!(true));
        } else if e["format"] == "path" {
            item.insert("hidden".into(), json!(true));
        } else if let Some(value) = shown.get(key) {
            let (value, cut) = light_value(value);
            item.insert("v".into(), value);
            if cut {
                item.insert("truncated".into(), json!(true));
            }
        }
        if e["secret"] != true {
            let changed = hot.value(key).is_some() && hot.value(key) != base.value(key);
            item.insert(
                "origin".into(),
                json!(if changed { "override" } else { "file" }),
            );
        }
        item.insert("editable".into(), json!(crate::policy::KEYS.contains(&key)));
        out.insert(key.to_string(), Value::Object(item));
    }
    json!({
        "schema": VALUES_SCHEMA,
        "server_version": env!("CARGO_PKG_VERSION"),
        "contract_hash": contract_hash(),
        "count": out.len(),
        "settings": out,
    })
}

/// The tag of an answer of the light route: the SHA-256 of its text, in quotes. The same values give the same tag, so a client that sends it back in `If-None-Match` is told
/// `304` while nothing changed.
pub fn values_etag(answer: &Value) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "\"{}\"",
        hex::encode(Sha256::digest(answer.to_string().as_bytes()))
    )
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

    // ---- the light view (`GET /api/v1/settings/values`) ----

    /// A small generator with a fixed seed: the same sequence on every machine.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn the_light_view_limits_in_the_contract_are_the_constants_of_the_code() {
        let light = &contract()["read_light"];
        assert_eq!(light["path"], "/api/v1/settings/values");
        assert_eq!(light["method"], "GET");
        assert_eq!(light["role"], "viewer");
        assert!(light["scope"].is_null());
        assert_eq!(light["schema"], VALUES_SCHEMA);
        assert_eq!(light["secret_mark"], SECRET_MARK);
        let limits = &light["limits"];
        assert_eq!(limits["max_bytes"], VALUES_MAX_BYTES);
        assert_eq!(limits["max_text_chars"], MAX_TEXT_CHARS);
        assert_eq!(limits["max_list_items"], MAX_LIST_ITEMS);
        assert_eq!(limits["max_item_chars"], MAX_ITEM_CHARS);
        assert_eq!(limits["max_query_bytes"], QUERY_MAX_BYTES);
    }

    #[test]
    fn the_hash_of_the_contract_is_stable_and_does_not_depend_on_the_line_ends() {
        let hash = contract_hash();
        assert_eq!(hash.len(), 64);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        assert_eq!(hash, contract_hash());
        // a checkout that turned the line ends into CRLF reads the same document
        let crlf = CONTRACT_JSON.replace("\r\n", "\n").replace('\n', "\r\n");
        use sha2::{Digest, Sha256};
        assert_eq!(
            hex::encode(Sha256::digest(crlf.replace("\r\n", "\n").as_bytes())),
            hash
        );
        let cfg = ServerConfig::default();
        let hot = Hot::from_config(&cfg);
        assert_eq!(document(&cfg, &hot)["contract_hash"], hash);
        assert_eq!(
            values_view(&cfg, &hot, &Selection::default())["contract_hash"],
            hash
        );
    }

    #[test]
    fn a_secret_key_shows_the_fixed_mark_and_its_value_is_nowhere_in_the_view() {
        // no key of server.cfg is secret today: mark each kind of key as secret, one at a time and then all together, and give each a value that must not come out
        let cfg = ServerConfig {
            mdns_name: "SENTINEL-NAME".to_string(),
            auth_extra_apps: vec!["sentinel-app".to_string()],
            session_hours: 8191,
            ..ServerConfig::default()
        };
        let mut hot = Hot::from_config(&cfg);
        hot.password_min_len = 9173;
        let sentinels = ["SENTINEL-NAME", "sentinel-app", "8191", "9173"];
        let keys = [
            "mdns_name",
            "auth_extra_apps",
            "session_hours",
            "password_min_len",
        ];
        let mut all: Vec<Value> = entries().to_vec();
        for (n, key) in keys.iter().enumerate() {
            let marked: Vec<Value> = entries()
                .iter()
                .map(|e| {
                    let mut e = e.clone();
                    if e["key"] == *key {
                        e["secret"] = json!(true);
                    }
                    e
                })
                .collect();
            let view = values_view_of(&marked, &cfg, &hot, &Selection::default());
            let item = &view["settings"][*key];
            assert_eq!(item["v"], SECRET_MARK, "{key}");
            assert_eq!(item["secret"], true, "{key}");
            assert!(item.get("origin").is_none(), "{key}");
            assert!(
                !view["settings"].to_string().contains(sentinels[n]),
                "{key}"
            );
            for e in all.iter_mut().filter(|e| e["key"] == *key) {
                e["secret"] = json!(true);
            }
        }
        let view = values_view_of(&all, &cfg, &hot, &Selection::default());
        for s in sentinels {
            assert!(!view["settings"].to_string().contains(s), "{s}");
        }
        // and the keys the contract itself marks as secret, if it ever has any
        let real = values_view(&cfg, &hot, &Selection::default());
        for e in entries().iter().filter(|e| e["secret"] == true) {
            let item = &real["settings"][e["key"].as_str().unwrap()];
            assert_eq!(item["v"], SECRET_MARK);
            assert_eq!(item["secret"], true);
        }
    }

    #[test]
    fn a_key_that_is_a_path_of_the_computer_has_no_value_in_the_light_view() {
        let cfg = ServerConfig {
            data_dir: "/private/place/of/the/data".into(),
            studio_dir: Some("/private/studio".into()),
            ..ServerConfig::default()
        };
        let hot = Hot::from_config(&cfg);
        let view = values_view(&cfg, &hot, &Selection::default());
        assert!(!view.to_string().contains("private"));
        let mut paths = 0;
        for e in entries().iter().filter(|e| e["format"] == "path") {
            let item = &view["settings"][e["key"].as_str().unwrap()];
            assert_eq!(item["hidden"], true, "{}", e["key"]);
            assert!(item.get("v").is_none(), "{}", e["key"]);
            paths += 1;
        }
        assert!(paths >= 5, "{paths} path keys");
    }

    #[test]
    fn the_light_view_is_small_and_bounded_even_with_the_biggest_values() {
        let plain = ServerConfig::default();
        let hot = Hot::from_config(&plain);
        let size = values_view(&plain, &hot, &Selection::default())
            .to_string()
            .len();
        assert!(size < 10 * 1024, "{size} bytes");
        // the contract with its texts is much bigger
        assert!(size * 20 < CONTRACT_JSON.len());
        let mut cfg = ServerConfig {
            mdns_name: "n".repeat(100_000),
            auth_extra_apps: (0..5000)
                .map(|i| format!("{i}{}", "a".repeat(500)))
                .collect(),
            ..ServerConfig::default()
        };
        let hot = Hot::from_config(&cfg);
        let view = values_view(&cfg, &hot, &Selection::default());
        let text = view.to_string();
        assert!(text.len() <= VALUES_MAX_BYTES, "{} bytes", text.len());
        let name = &view["settings"]["mdns_name"];
        assert_eq!(name["truncated"], true);
        assert_eq!(name["v"].as_str().unwrap().chars().count(), MAX_TEXT_CHARS);
        let apps = &view["settings"]["auth_extra_apps"];
        assert_eq!(apps["truncated"], true);
        assert_eq!(apps["v"].as_array().unwrap().len(), MAX_LIST_ITEMS);
        // multi-byte text is cut on a character, never in the middle of one
        cfg.mdns_name = "\u{f1}".repeat(500);
        let view = values_view(&cfg, &Hot::from_config(&cfg), &Selection::default());
        assert_eq!(
            view["settings"]["mdns_name"]["v"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MAX_TEXT_CHARS
        );
    }

    #[test]
    fn the_origin_is_override_only_for_a_key_the_security_page_changed() {
        let cfg = ServerConfig::default();
        let mut hot = Hot::from_config(&cfg);
        hot.url_token = !hot.url_token;
        let view = values_view(&cfg, &hot, &Selection::default());
        for (key, item) in view["settings"].as_object().unwrap() {
            let want = if key == "url_token" {
                "override"
            } else {
                "file"
            };
            assert_eq!(item["origin"], want, "{key}");
            assert_eq!(
                item["editable"] == true,
                policy::KEYS.contains(&key.as_str()),
                "{key}"
            );
        }
        assert_eq!(view["count"], entries().len());
        // the same values give the same tag and another value gives another
        let again = values_view(&cfg, &hot, &Selection::default());
        assert_eq!(values_etag(&view), values_etag(&again));
        let other = values_view(&cfg, &Hot::from_config(&cfg), &Selection::default());
        assert_ne!(values_etag(&view), values_etag(&other));
    }

    #[test]
    fn the_selection_takes_known_keys_and_groups_and_refuses_the_rest() {
        let list = entries();
        let ok = parse_selection(Some("keys=url_token,session_hours,url_token&x=1"), list).unwrap();
        assert_eq!(
            ok.keys.as_deref(),
            Some(&["url_token".to_string(), "session_hours".to_string()][..])
        );
        let ok = parse_selection(Some("keys=url_token%2Csession_hours"), list).unwrap();
        assert_eq!(ok.keys.unwrap().len(), 2);
        assert_eq!(
            parse_selection(Some("group=login"), list).unwrap().group,
            Some("login".to_string())
        );
        assert_eq!(parse_selection(None, list).unwrap(), Selection::default());
        assert_eq!(
            parse_selection(Some("other=1&&"), list).unwrap(),
            Selection::default()
        );
        for bad in [
            "keys=no_such_key",
            "keys=url_token,../etc",
            "group=nowhere",
            "keys=%zz",
            "keys=%",
            "keys=%ff%fe",
            "%ff=1",
        ] {
            assert!(parse_selection(Some(bad), list).is_err(), "{bad}");
        }
        let long = format!("keys={}", "a".repeat(QUERY_MAX_BYTES));
        assert!(parse_selection(Some(&long), list).is_err());
        // a selection lists only what it names
        let cfg = ServerConfig::default();
        let hot = Hot::from_config(&cfg);
        let chosen = parse_selection(Some("keys=url_token,listen"), list).unwrap();
        assert_eq!(values_view(&cfg, &hot, &chosen)["count"], 2);
        let group = parse_selection(Some("group=login"), list).unwrap();
        let n = entries().iter().filter(|e| e["group"] == "login").count();
        assert_eq!(values_view(&cfg, &hot, &group)["count"], n);
    }

    #[test]
    fn the_query_fuzz_from_a_fixed_seed_never_panics_and_only_gives_known_keys() {
        let alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyz_0123456789,&=%+.:/ \u{e9}\u{4e2d}[]"
            .chars()
            .collect();
        let known = [
            "keys=",
            "group=",
            "keys=url_token,",
            "group=login",
            "%",
            "%2C",
            "&",
            "=",
            "x=",
        ];
        let mut rng = Lcg(0x5E77_1465);
        let (mut accepted, mut refused) = (0, 0);
        for _ in 0..4000 {
            let mut q = String::new();
            for _ in 0..rng.below(8) {
                if rng.below(3) == 0 {
                    q.push_str(known[rng.below(known.len())]);
                } else {
                    for _ in 0..rng.below(12) {
                        q.push(alphabet[rng.below(alphabet.len())]);
                    }
                }
            }
            match parse_selection(Some(&q), entries()) {
                Ok(sel) => {
                    accepted += 1;
                    for k in sel.keys.iter().flatten() {
                        assert!(entry(k).is_some(), "{q}");
                    }
                    if let Some(g) = &sel.group {
                        assert!(entries().iter().any(|e| e["group"] == g.as_str()), "{q}");
                    }
                }
                Err(text) => {
                    refused += 1;
                    assert!(!text.is_empty() && text.chars().count() < 200, "{q}");
                }
            }
        }
        assert!(accepted > 100 && refused > 100, "{accepted} / {refused}");
    }
}
