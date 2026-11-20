//! The routes of Moonraker and OctoPrint at the root of the address: off until the switch `compat_api` is on, opened by the key of one machine of the plugin `printer3d`
//! (made by an administrator, shown once, kept only as a hash), read-only, and for the shape the machine is set to give.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, send, server, user, Server};
use serde_json::{json, Value};

const POLICY: &str = "/api/v1/auth/policy";

async fn make_printer(s: &Server, boss: &str, id: &str, compat: &str) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": "printer3d", "settings": {"compat": compat}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn issue_key(s: &Server, boss: &str, id: &str) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        &format!("/api/v1/work/machines/{id}/compat/key"),
        Some(boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["header"], "X-Api-Key");
    body["key"].as_str().unwrap().to_string()
}

async fn switch(s: &Server, boss: &str, on: bool) {
    let (status, body) = call(
        s,
        Method::PUT,
        POLICY,
        Some(boss),
        Some(json!({"key": "compat_api", "value": on, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A request to a root path with the key in `X-Api-Key` (or none).
async fn root(s: &Server, path: &str, key: Option<&str>) -> (StatusCode, Value) {
    let headers: Vec<(&str, &str)> = key.map(|k| vec![("X-Api-Key", k)]).unwrap_or_default();
    let reply = send(s, Method::GET, path, None, &headers, None, None).await;
    (reply.status, reply.json())
}

const MOONRAKER: [&str; 3] = ["/server/info", "/printer/info", "/printer/objects/query"];
const OCTOPRINT: [&str; 3] = ["/api/version", "/api/printer", "/api/job"];

#[tokio::test]
async fn the_routes_are_off_by_default_whatever_the_key() {
    let s = server("compat-off");
    let boss = admin(&s).await;
    make_printer(&s, &boss, "p1", "both").await;
    let key = issue_key(&s, &boss, "p1").await;
    for path in MOONRAKER.iter().chain(OCTOPRINT.iter()) {
        for key in [None, Some(key.as_str())] {
            let (status, body) = root(&s, path, key).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::NOT_FOUND, Some("compat_off")),
                "{path}"
            );
        }
    }
    // the switch is listed, off and as strict as recommended
    let (_, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    let listed = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "compat_api")
        .cloned()
        .unwrap();
    assert_eq!(
        (listed["value"].clone(), listed["safe"].clone()),
        (json!(false), json!(true))
    );
}

#[tokio::test]
async fn with_the_switch_on_the_key_of_the_machine_opens_the_state_of_that_machine() {
    let s = server("compat-on");
    let boss = admin(&s).await;
    make_printer(&s, &boss, "p1", "both").await;
    make_printer(&s, &boss, "p2", "both").await;
    let key1 = issue_key(&s, &boss, "p1").await;
    let key2 = issue_key(&s, &boss, "p2").await;
    assert!(key1.starts_with("eh3d_") && key1.len() == 69 && key1 != key2);
    switch(&s, &boss, true).await;
    // no key, a wrong key, a key of another shape
    for bad in [None, Some("eh3d_nothing"), Some("not a key"), Some("")] {
        for path in MOONRAKER.iter().chain(OCTOPRINT.iter()) {
            let (status, body) = root(&s, path, bad).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNAUTHORIZED, Some("unauthorized")),
                "{path} {bad:?}"
            );
        }
    }
    // Moonraker
    let (status, info) = root(&s, "/printer/info", Some(&key1)).await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["result"]["state"], "ready");
    assert_eq!(info["result"]["hostname"], "p1");
    let (_, other) = root(&s, "/printer/info", Some(&key2)).await;
    assert_eq!(other["result"]["hostname"], "p2");
    let (status, server_info) = root(&s, "/server/info", Some(&key1)).await;
    assert_eq!(status, StatusCode::OK, "{server_info}");
    assert_eq!(server_info["result"]["klippy_connected"], true);
    assert_eq!(server_info["result"]["klippy_state"], "ready");
    assert!(server_info["result"]["moonraker_version"]
        .as_str()
        .unwrap()
        .starts_with("eh3d-server"));
    let (status, query) = root(&s, "/printer/objects/query", Some(&key1)).await;
    assert_eq!(status, StatusCode::OK, "{query}");
    assert!(query["result"]["status"]["print_stats"]["state"].is_string());
    assert!(query["result"]["status"]["extruder"].is_object());
    // OctoPrint
    let (status, version) = root(&s, "/api/version", Some(&key1)).await;
    assert_eq!(status, StatusCode::OK, "{version}");
    assert_eq!(version["api"], "0.1");
    let (status, printer) = root(&s, "/api/printer", Some(&key1)).await;
    assert_eq!(status, StatusCode::OK, "{printer}");
    assert_eq!(printer["state"]["flags"]["operational"], true);
    assert!(printer["temperature"]["tool0"].is_object());
    let (status, job) = root(&s, "/api/job", Some(&key1)).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert!(job["progress"].is_object());
    // the key is not a login: the rest of the interface does not take it
    let (status, _) = call(&s, Method::GET, "/api/v1/work/machines", Some(&key1), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // and the routes write nothing
    for path in [
        "/printer/print/start",
        "/printer/gcode/script",
        "/api/files/local",
    ] {
        let reply = send(
            &s,
            Method::POST,
            path,
            None,
            &[("X-Api-Key", key1.as_str())],
            Some(b"{}".to_vec()),
            Some("application/json"),
        )
        .await;
        assert!(
            matches!(
                reply.status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{path}: {}",
            reply.status
        );
    }
    // off again
    switch(&s, &boss, false).await;
    let (status, body) = root(&s, "/printer/info", Some(&key1)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("compat_off"))
    );
}

#[tokio::test]
async fn a_machine_gives_only_the_shapes_it_is_set_to_give() {
    let s = server("compat-shape");
    let boss = admin(&s).await;
    make_printer(&s, &boss, "m1", "moonraker").await;
    make_printer(&s, &boss, "o1", "octoprint").await;
    make_printer(&s, &boss, "n1", "off").await;
    let (km, ko, kn) = (
        issue_key(&s, &boss, "m1").await,
        issue_key(&s, &boss, "o1").await,
        issue_key(&s, &boss, "n1").await,
    );
    switch(&s, &boss, true).await;
    assert_eq!(root(&s, "/printer/info", Some(&km)).await.0, StatusCode::OK);
    assert_eq!(root(&s, "/api/printer", Some(&ko)).await.0, StatusCode::OK);
    for (path, key) in [
        ("/printer/info", &ko),
        ("/api/printer", &km),
        ("/printer/info", &kn),
        ("/api/job", &kn),
    ] {
        let (status, body) = root(&s, path, Some(key.as_str())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_IMPLEMENTED, Some("not_supported")),
            "{path}"
        );
    }
    // the OctoPrint version is the same whatever the shape: it only says which server this is
    assert_eq!(root(&s, "/api/version", Some(&km)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_key_is_made_by_an_administrator_shown_once_replaced_and_revoked() {
    let s = server("compat-key");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_printer(&s, &boss, "p1", "both").await;
    let path = "/api/v1/work/machines/p1/compat/key";
    let (_, state) = call(&s, Method::GET, path, Some(&boss), None).await;
    assert_eq!(state["set"], false);
    let (status, _) = call(&s, Method::POST, path, Some(&operator), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&s, Method::GET, path, Some(&operator), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let first = issue_key(&s, &boss, "p1").await;
    let (_, state) = call(&s, Method::GET, path, Some(&boss), None).await;
    assert_eq!(state["set"], true);
    assert!(state.get("key").is_none(), "the key is not shown again");
    // only its hash is on the disk
    let file = std::fs::read_to_string(s.dir.join("compat-keys.json")).unwrap();
    assert!(!file.contains(&first) && file.contains("p1"));
    switch(&s, &boss, true).await;
    assert_eq!(
        root(&s, "/printer/info", Some(&first)).await.0,
        StatusCode::OK
    );
    // a new key replaces the old one
    let second = issue_key(&s, &boss, "p1").await;
    assert_ne!(first, second);
    assert_eq!(
        root(&s, "/printer/info", Some(&first)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        root(&s, "/printer/info", Some(&second)).await.0,
        StatusCode::OK
    );
    // revoked
    let (status, body) = call(&s, Method::DELETE, path, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], true);
    assert_eq!(
        root(&s, "/printer/info", Some(&second)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, again) = call(&s, Method::DELETE, path, Some(&boss), None).await;
    assert_eq!(again["revoked"], false);
    // a key for the machine goes when the machine goes
    let third = issue_key(&s, &boss, "p1").await;
    let (status, _) = call(
        &s,
        Method::DELETE,
        "/api/v1/work/machines/p1",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        root(&s, "/printer/info", Some(&third)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = call(&s, Method::GET, path, Some(&boss), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_machine"))
    );
}

#[tokio::test]
async fn only_a_printer_has_a_key() {
    let s = server("compat-kind");
    let boss = admin(&s).await;
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "l1", "name": "L1", "plugin": "laser", "settings": {}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/l1/compat/key",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("not_supported"))
    );
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/nothing/compat/key",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_machine"))
    );
    assert!(!s.dir.join("compat-keys.json").exists());
}
