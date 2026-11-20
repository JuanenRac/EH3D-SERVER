//! The contract of the settings (`GET /api/v1/settings/contract`, `docs/settings-contract.json`) end to end through the real router: who may read it, that every key the
//! policy writes can be written the way the contract says, that a refusal names the key, that a change says whether it needs a restart and is in the audit with the old and the
//! new value, and that a key the contract marks as secret never has a value. Nothing here is random: every value comes from the contract itself.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, scoped_token, server, user};
use eh3d_server::settings;
use serde_json::{json, Value};

const CONTRACT: &str = "/api/v1/settings/contract";
const POLICY: &str = "/api/v1/auth/policy";

fn entries() -> &'static [Value] {
    settings::entries()
}

/// The fields every entry of the contract has, with their types: the shape the clients rely on.
fn shape_problem(e: &Value) -> Option<String> {
    let key = e["key"].as_str().unwrap_or("?").to_string();
    for (field, ok) in [
        ("key", e["key"].is_string()),
        ("group", e["group"].is_string()),
        ("type", e["type"].is_string()),
        ("default", !e["default"].is_null()),
        (
            "apply",
            matches!(e["apply"].as_str(), Some("now" | "restart")),
        ),
        ("write", e["write"].is_object()),
        ("confirm_when_looser", e["confirm_when_looser"].is_boolean()),
        ("secret", e["secret"].is_boolean()),
        ("risk", e["risk"].is_string()),
        ("label", e["label"].is_object()),
        ("help", e["help"].is_object()),
        ("warning", e["warning"].is_object()),
    ] {
        if !ok {
            return Some(format!(
                "{key}: field {field} is missing or of another type"
            ));
        }
    }
    match e["type"].as_str().unwrap() {
        "integer" if !(e["min"].is_u64() && e["max"].is_u64() && e["default"].is_u64()) => Some(
            format!("{key}: an integer needs min, max and a whole default"),
        ),
        "boolean" if !e["default"].is_boolean() => {
            Some(format!("{key}: a boolean needs a true or false default"))
        }
        "string" if !e["default"].is_string() => {
            Some(format!("{key}: a string needs a text default"))
        }
        "list" if !e["default"].is_array() => Some(format!("{key}: a list needs a list default")),
        "integer" | "boolean" | "string" | "list" => None,
        other => Some(format!("{key}: unknown type {other}")),
    }
}

#[test]
fn the_contract_is_valid_and_its_texts_are_complete_in_the_eight_languages() {
    let langs = ["en", "es", "de", "fr", "it", "ja", "zh", "pt"];
    assert!(entries().len() >= 80, "{} settings", entries().len());
    for e in entries() {
        assert_eq!(shape_problem(e), None);
        for what in ["label", "help", "warning"] {
            let obj = e[what].as_object().unwrap();
            assert_eq!(obj.len(), 8, "{} {what}", e["key"]);
            for lang in langs {
                assert!(
                    obj.get(lang)
                        .and_then(Value::as_str)
                        .is_some_and(|t| !t.trim().is_empty()),
                    "{} has no {what} in {lang}",
                    e["key"]
                );
            }
        }
    }
}

#[tokio::test]
async fn an_administrator_reads_the_contract_with_the_values_and_nobody_else_does() {
    let s = server("settings-contract-read");
    let boss = admin(&s).await;
    let (status, doc) = call(&s, Method::GET, CONTRACT, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["schema"], "eh3d.settings.contract/1");
    assert_eq!(doc["server_version"], env!("CARGO_PKG_VERSION"));
    // the answer is the document of the repository plus the values
    let mut without = doc.clone();
    let object = without.as_object_mut().unwrap();
    object.remove("values");
    object.remove("server_version");
    assert_eq!(&without, settings::contract());
    let values = doc["values"].as_object().unwrap();
    for e in entries() {
        let key = e["key"].as_str().unwrap();
        if e["secret"] == true {
            assert!(!values.contains_key(key), "{key} is secret and has a value");
        } else {
            assert!(values.contains_key(key), "{key} has no value");
        }
    }
    assert_eq!(values["listen"], "127.0.0.1:18200");
    assert_eq!(values["url_token"], false);

    // an operator and a viewer are refused with the code of the role; nobody at all is not logged in
    for role in ["operator", "viewer"] {
        let token = user(&s, &boss, &format!("{role}-one"), role).await;
        let (status, body) = call(&s, Method::GET, CONTRACT, Some(&token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {body}");
        assert_eq!(body["error"], "forbidden", "{role}");
        assert!(body.get("values").is_none(), "{role} got values");
    }
    let (status, body) = call(&s, Method::GET, CONTRACT, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // a token of an administrator that is limited to other scopes is refused with the code of the scope
    let limited = scoped_token(&s, &boss, "admin", &["admin:users"]).await;
    let (status, body) = call(&s, Method::GET, CONTRACT, Some(&limited), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden_scope")),
        "{body}"
    );
    let right = scoped_token(&s, &boss, "admin", &["admin:policy"]).await;
    let (status, _) = call(&s, Method::GET, CONTRACT, Some(&right), None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn every_key_the_contract_says_the_policy_writes_is_written_the_way_it_says() {
    let s = server("settings-contract-write");
    let boss = admin(&s).await;
    let mut written = 0;
    for e in entries() {
        let key = e["key"].as_str().unwrap();
        let write = &e["write"];
        if write["via"] != "policy" {
            continue;
        }
        assert_eq!(write["method"], "PUT");
        let method = Method::PUT;
        let path = write["path"].as_str().unwrap();
        // the value the server starts with is inside the range and never needs more than the confirmation
        let body = json!({"key": key, "value": e["default"], "confirm_weak": true});
        let (status, answer) = call(&s, method.clone(), path, Some(&boss), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{key}: {answer}");
        let restart = e["apply"] == "restart";
        assert_eq!(answer["changed"]["key"], key);
        assert_eq!(answer["changed"]["restart_required"], restart, "{key}");
        assert_eq!(
            answer["changed"]["apply"],
            if restart { "restart" } else { "now" }
        );
        assert_eq!(
            answer["restart_required"]
                .as_array()
                .unwrap()
                .contains(&json!(key)),
            restart,
            "{key}"
        );
        // a value of another type is refused with the code of the policy, and the detail names the key
        let wrong = if e["type"] == "boolean" {
            json!("yes")
        } else {
            json!("many")
        };
        let (status, answer) = call(
            &s,
            method.clone(),
            path,
            Some(&boss),
            Some(json!({"key": key, "value": wrong})),
        )
        .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_policy")),
            "{key}"
        );
        assert!(
            answer["detail"].as_str().unwrap().contains(key),
            "{key}: {answer}"
        );
        if e["type"] == "integer" {
            // one past the end of the range is refused too
            let past = e["max"].as_u64().unwrap() + 1;
            let (status, answer) = call(
                &s,
                method,
                path,
                Some(&boss),
                Some(json!({"key": key, "value": past, "confirm_weak": true})),
            )
            .await;
            assert_eq!(
                (status, answer["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_policy")),
                "{key}"
            );
            assert!(
                answer["detail"].as_str().unwrap().contains(key),
                "{key}: {answer}"
            );
        }
        written += 1;
    }
    assert_eq!(written, eh3d_server::policy::KEYS.len());
}

#[tokio::test]
async fn a_key_that_is_only_in_the_file_is_refused_with_a_detail_that_names_the_key_and_the_file() {
    let s = server("settings-contract-file");
    let boss = admin(&s).await;
    let mut tried = 0;
    for e in entries() {
        let key = e["key"].as_str().unwrap();
        if e["write"]["via"] != "file" {
            continue;
        }
        assert_eq!(e["apply"], "restart", "{key}");
        let (status, answer) = call(
            &s,
            Method::PUT,
            POLICY,
            Some(&boss),
            Some(json!({"key": key, "value": e["default"], "confirm_weak": true})),
        )
        .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_policy")),
            "{key}"
        );
        let detail = answer["detail"].as_str().unwrap();
        assert!(
            detail.contains(key) && detail.contains("server.cfg"),
            "{key}: {detail}"
        );
        tried += 1;
    }
    assert_eq!(tried, entries().len() - eh3d_server::policy::KEYS.len());
    // a key that is nowhere is refused the same way, naming it
    let (status, answer) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "no_such_key", "value": 1})),
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_policy"))
    );
    assert!(answer["detail"].as_str().unwrap().contains("no_such_key"));
}

#[tokio::test]
async fn a_change_takes_effect_at_once_is_in_the_values_and_is_in_the_audit_with_the_old_and_the_new_value(
) {
    let s = server("settings-contract-audit");
    let boss = admin(&s).await;
    let (status, answer) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "password_min_len", "value": 12})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        answer["changed"],
        json!({"key": "password_min_len", "from": 8, "to": 12, "apply": "now", "restart_required": false})
    );
    let (_, doc) = call(&s, Method::GET, CONTRACT, Some(&boss), None).await;
    assert_eq!(doc["values"]["password_min_len"], 12);
    // a key that is read when the server starts says so in the answer
    let (status, answer) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "network_open", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["changed"]["apply"], "restart");
    assert_eq!(answer["changed"]["restart_required"], true);
    let (status, audit) = call(
        &s,
        Method::GET,
        "/api/v1/audit?event=policy_change",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let lines = audit["lines"].as_array().unwrap();
    let line = lines
        .iter()
        .find(|l| l["key"] == "password_min_len")
        .expect("the change is in the audit");
    assert_eq!(
        (line["from"].as_str(), line["to"].as_str()),
        (Some("8"), Some("12"))
    );
    assert_eq!(line["who"], "juan");
    assert!(lines.iter().any(|l| l["key"] == "network_open"));
}

#[test]
fn a_secret_key_has_no_value_and_is_hidden_in_the_audit() {
    // the contract marks no key of server.cfg as secret today, so the rule is exercised on the functions the route uses
    for e in entries() {
        let key = e["key"].as_str().unwrap();
        assert_eq!(settings::is_secret(key), e["secret"] == true, "{key}");
        let value = json!("anything");
        let shown = settings::shown(key, &value);
        assert_eq!(shown == json!("***"), e["secret"] == true, "{key}");
        assert!(e["secret"] == true || shown == value, "{key}");
    }
    assert!(!settings::is_secret("not_a_key"));
    let cfg = eh3d_server::config::ServerConfig::default();
    let hot = eh3d_server::policy::Hot::from_config(&cfg);
    let values = settings::values(&cfg, &hot);
    for e in entries().iter().filter(|e| e["secret"] == true) {
        assert!(values.get(e["key"].as_str().unwrap()).is_none());
    }
}
