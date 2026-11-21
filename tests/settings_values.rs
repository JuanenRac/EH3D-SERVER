//! The light, read-only view of the settings (`GET /api/v1/settings/values`) end to end through the real router: who may read it (a viewer, an operator, an administrator, a
//! login of the `hmi` application and a token with other scopes; nobody who is not logged in), that it has no text, no secret and no path and is small, that its `ETag` and
//! `If-None-Match` work, that a change of the policy changes the tag and shows as `override`, that a query that does not fit is refused with a name, and that a refusal is in the
//! audit. Nothing here is random except the fuzz, which has a fixed seed.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, scoped_token, send, server, user, Server, PASSWORD};
use eh3d_server::settings;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const VALUES: &str = "/api/v1/settings/values";
const CONTRACT: &str = "/api/v1/settings/contract";
const POLICY: &str = "/api/v1/auth/policy";

async fn get(s: &Server, path: &str, token: Option<&str>) -> common::Reply {
    send(s, Method::GET, path, token, &[], None, None).await
}

#[tokio::test]
async fn every_role_reads_the_light_view_and_nobody_who_is_not_logged_in_does() {
    let s = server("settings-values-roles");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "operator-one", "operator").await;
    let viewer = user(&s, &boss, "viewer-one", "viewer").await;
    // a screen of a machine: an operator that logs in as the application hmi
    let (status, login) = call(
        &s,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"name": "operator-one", "password": PASSWORD, "app": "hmi"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let hmi = login["token"].as_str().unwrap().to_string();
    // a token of a program that is limited to scopes that have nothing to do with the settings
    let limited = scoped_token(&s, &boss, "viewer", &["projects:read"]).await;
    let admin_limited = scoped_token(&s, &boss, "admin", &["admin:users"]).await;
    let mut first: Option<Value> = None;
    for (who, token) in [
        ("admin", &boss),
        ("operator", &operator),
        ("viewer", &viewer),
        ("hmi", &hmi),
        ("token", &limited),
        ("admin token without admin:policy", &admin_limited),
    ] {
        let r = get(&s, VALUES, Some(token)).await;
        assert_eq!(r.status, StatusCode::OK, "{who}: {}", r.json());
        let doc = r.json();
        assert_eq!(doc["schema"], "eh3d.settings.values/1", "{who}");
        match &first {
            None => first = Some(doc),
            Some(f) => assert_eq!(&doc, f, "{who} reads the same as the others"),
        }
    }
    // the full contract stays for the administrator
    for token in [&operator, &viewer, &hmi] {
        assert_eq!(
            get(&s, CONTRACT, Some(token)).await.status,
            StatusCode::FORBIDDEN
        );
    }
    let r = get(&s, VALUES, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json());
    assert_eq!(r.error(), "unauthorized");
    let r = get(&s, VALUES, Some("not-a-token")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // only a read: nothing here writes
    for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
        let r = send(&s, method, VALUES, Some(&boss), &[], None, None).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    }
}

#[tokio::test]
async fn the_light_view_has_every_key_with_its_value_its_origin_and_whether_the_policy_changes_it()
{
    let s = server("settings-values-shape");
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "viewer-one", "viewer").await;
    let r = get(&s, VALUES, Some(&viewer)).await;
    let doc = r.json();
    assert_eq!(doc["server_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["contract_hash"], settings::contract_hash());
    assert_eq!(doc["count"], settings::entries().len());
    let items = doc["settings"].as_object().unwrap();
    assert_eq!(items.len(), settings::entries().len());
    let (_, full) = call(&s, Method::GET, CONTRACT, Some(&boss), None).await;
    assert_eq!(full["contract_hash"], doc["contract_hash"]);
    for e in settings::entries() {
        let key = e["key"].as_str().unwrap();
        let item = &items[key];
        assert_eq!(
            item["editable"] == true,
            eh3d_server::policy::KEYS.contains(&key),
            "{key}"
        );
        assert_eq!(item["origin"], "file", "{key}");
        if e["format"] == "path" {
            assert_eq!(item["hidden"], true, "{key}");
            assert!(item.get("v").is_none(), "{key}");
        } else {
            assert_eq!(item["v"], full["values"][key], "{key}");
        }
    }
    // no text of the contract and no folder of this computer
    let body = String::from_utf8(r.bytes.clone()).unwrap();
    assert!(!body.contains("label") && !body.contains("help") && !body.contains("warning"));
    assert!(!body.contains(&s.dir.display().to_string()));
    assert!(!body.contains("\"secret\":false"));
}

#[tokio::test]
async fn the_light_view_is_a_few_kilobytes_and_never_more_than_its_limit() {
    let s = server("settings-values-size");
    let boss = admin(&s).await;
    let light = get(&s, VALUES, Some(&boss)).await.bytes.len();
    let full = serde_json::to_vec(&call(&s, Method::GET, CONTRACT, Some(&boss), None).await.1)
        .unwrap()
        .len();
    assert!(light < 10 * 1024, "{light} bytes");
    assert!(light <= settings::VALUES_MAX_BYTES);
    assert!(light * 20 < full, "{light} against {full}");
    // a server with the biggest values it can be given still answers inside the limit
    let big = common::server_with("settings-values-size-big", |c| {
        c.mdns_name = "n".repeat(60);
        c.auth_extra_apps = (0..200)
            .map(|i| format!("app-{i:04}-{}", "x".repeat(40)))
            .collect();
    });
    let boss = admin(&big).await;
    let r = get(&big, VALUES, Some(&boss)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.bytes.len() <= settings::VALUES_MAX_BYTES);
    let apps = &r.json()["settings"]["auth_extra_apps"];
    assert_eq!(
        apps["v"].as_array().unwrap().len(),
        settings::MAX_LIST_ITEMS
    );
    assert_eq!(apps["truncated"], true);
}

#[tokio::test]
async fn the_tag_lets_a_client_ask_again_only_when_something_changed() {
    let s = server("settings-values-etag");
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "viewer-one", "viewer").await;
    let first = get(&s, VALUES, Some(&viewer)).await;
    let tag = first.header("etag");
    assert!(
        tag.starts_with('"') && tag.ends_with('"') && tag.len() == 66,
        "{tag}"
    );
    assert_eq!(first.header("cache-control"), "private, no-cache");
    // the same values, the same tag, whoever asks and however often
    assert_eq!(get(&s, VALUES, Some(&boss)).await.header("etag"), tag);
    assert_eq!(get(&s, VALUES, Some(&viewer)).await.header("etag"), tag);
    // the tag is the SHA-256 of the answer as the client got it
    let body: Value = first.json();
    assert_eq!(tag, settings::values_etag(&body));
    for header in [
        tag.clone(),
        format!("W/{tag}"),
        "*".to_string(),
        format!("\"x\", {tag}"),
    ] {
        let r = send(
            &s,
            Method::GET,
            VALUES,
            Some(&viewer),
            &[("if-none-match", header.as_str())],
            None,
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_MODIFIED, "{header}");
        assert!(r.bytes.is_empty(), "{header}");
        assert_eq!(r.header("etag"), tag);
    }
    let other = send(
        &s,
        Method::GET,
        VALUES,
        Some(&viewer),
        &[("if-none-match", "\"another\"")],
        None,
        None,
    )
    .await;
    assert_eq!(other.status, StatusCode::OK);
    // a refusal is never a 304: a client without a login is refused even with the right tag
    let r = send(
        &s,
        Method::GET,
        VALUES,
        None,
        &[("if-none-match", tag.as_str())],
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // a change of the policy changes the tag and shows as an override
    let (status, answer) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "password_min_len", "value": 12})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let after = send(
        &s,
        Method::GET,
        VALUES,
        Some(&viewer),
        &[("if-none-match", tag.as_str())],
        None,
        None,
    )
    .await;
    assert_eq!(after.status, StatusCode::OK);
    assert_ne!(after.header("etag"), tag);
    let item = &after.json()["settings"]["password_min_len"];
    assert_eq!(item["v"], 12);
    assert_eq!(item["origin"], "override");
    assert_eq!(item["editable"], true);
    // the contract has not changed: the hash is the same, so a client keeps its copy
    assert_eq!(after.json()["contract_hash"], body["contract_hash"]);
}

#[test]
fn the_hash_of_the_contract_is_the_hash_of_the_file_of_the_repository() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/settings-contract.json"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    assert_eq!(
        hex::encode(Sha256::digest(text.as_bytes())),
        settings::contract_hash()
    );
}

#[tokio::test]
async fn a_query_selects_keys_and_a_name_that_is_not_one_is_refused_with_the_name() {
    let s = server("settings-values-query");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "operator-one", "operator").await;
    let r = get(
        &s,
        &format!("{VALUES}?keys=url_token,session_hours&unknown=1"),
        Some(&operator),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let doc = r.json();
    assert_eq!(doc["count"], 2);
    assert_eq!(doc["settings"]["session_hours"]["v"], 12);
    assert_ne!(
        r.header("etag"),
        get(&s, VALUES, Some(&operator)).await.header("etag")
    );
    let r = get(&s, &format!("{VALUES}?group=login"), Some(&operator)).await;
    let n = settings::entries()
        .iter()
        .filter(|e| e["group"] == "login")
        .count();
    assert_eq!(r.json()["count"], n);
    // a key or a group that the contract does not have
    for (query, name) in [
        ("keys=url_token,no_such_key", "no_such_key"),
        ("group=nowhere", "nowhere"),
    ] {
        let r = get(&s, &format!("{VALUES}?{query}"), Some(&operator)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(r.error(), "bad_request");
        assert!(r.json()["detail"].as_str().unwrap().contains(name));
    }
    // a query too long and a query that does not decode
    let long = format!(
        "{VALUES}?keys={}",
        "a".repeat(settings::QUERY_MAX_BYTES + 1)
    );
    assert_eq!(get(&s, &long, Some(&operator)).await.error(), "bad_request");
    assert_eq!(
        get(&s, &format!("{VALUES}?keys=%zz"), Some(&operator))
            .await
            .error(),
        "bad_request"
    );
}

#[tokio::test]
async fn a_fuzz_of_the_query_from_a_fixed_seed_never_gives_a_server_error_or_a_leak() {
    let s = server("settings-values-fuzz");
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "viewer-one", "viewer").await;
    let alphabet: Vec<char> = "abcxyz_0123,&=%+.:/ []{}\"'<>\\;".chars().collect();
    let mut state: u64 = 0xD0D0_2025;
    let mut next = |n: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) as usize) % n
    };
    let (mut ok, mut refused) = (0, 0);
    for _ in 0..300 {
        let mut q = String::new();
        for _ in 0..next(6) {
            q.push_str(["keys=", "group=", "url_token", ",", "login", "%", "&"][next(7)]);
            for _ in 0..next(6) {
                let c = alphabet[next(alphabet.len())];
                // the characters that cannot stand in an address are sent encoded
                if c.is_ascii_alphanumeric() || "_,&=%+.:/".contains(c) {
                    q.push(c);
                } else {
                    q.push_str(&format!("%{:02X}", c as u32));
                }
            }
        }
        let r = get(&s, &format!("{VALUES}?{q}"), Some(&viewer)).await;
        match r.status {
            StatusCode::OK => {
                ok += 1;
                assert!(r.bytes.len() <= settings::VALUES_MAX_BYTES, "{q}");
            }
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::BAD_REQUEST => {
                refused += 1;
                assert_eq!(r.error(), "bad_request", "{q}");
            }
            other => panic!("{q}: {other}"),
        }
    }
    assert!(ok > 20 && refused > 20, "{ok} / {refused}");
}

#[tokio::test]
async fn a_refusal_of_the_light_view_is_in_the_audit_like_the_other_refusals() {
    let s = server("settings-values-audit");
    let boss = admin(&s).await;
    assert_eq!(get(&s, VALUES, None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        get(&s, VALUES, Some("not-a-token")).await.status,
        StatusCode::UNAUTHORIZED
    );
    // a read that is allowed is a read like the others: no line, and the audit is still readable
    let viewer = user(&s, &boss, "viewer-one", "viewer").await;
    assert_eq!(get(&s, VALUES, Some(&viewer)).await.status, StatusCode::OK);
    let (_, audit) = call(
        &s,
        Method::GET,
        "/api/v1/audit?event=denied",
        Some(&boss),
        None,
    )
    .await;
    let lines = audit["lines"].as_array().unwrap();
    let mine: Vec<&Value> = lines.iter().filter(|l| l["path"] == VALUES).collect();
    assert_eq!(mine.len(), 2, "{lines:?}");
    assert!(mine
        .iter()
        .all(|l| l["status"] == "401" && l["method"] == "GET"));
    assert!(mine.iter().any(|l| l["token"] == "no"));
    assert!(mine.iter().any(|l| l["token"] == "yes"));
    // nothing of the token reaches the audit
    let text = std::fs::read_to_string(s.dir.join("audit.jsonl")).unwrap();
    assert!(!text.contains("not-a-token") && !text.contains(&viewer));
}

#[tokio::test]
async fn the_policy_still_needs_an_administrator_and_the_light_view_opens_no_write() {
    let s = server("settings-values-policy-unchanged");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "operator-one", "operator").await;
    for token in [&operator] {
        let (status, body) = call(
            &s,
            Method::PUT,
            POLICY,
            Some(token),
            Some(json!({"key": "password_min_len", "value": 12})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let (status, _) = call(&s, Method::GET, POLICY, Some(&operator), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // the values did not move
    let r = get(&s, VALUES, Some(&operator)).await;
    assert_eq!(r.json()["settings"]["password_min_len"]["v"], 8);
}
