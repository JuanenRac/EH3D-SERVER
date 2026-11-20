//! The rule "when something goes wrong, go towards what is safer" (`docs/SAFE_DEGRADATION.md`), tried on the error paths of the server: a plugin that fails, a value of a safety
//! setting that is not understood, a write that is refused, a login that is closed or unknown, a policy file that is missing, wrong or unreadable. Each test fails when a path
//! loosens a restriction (the emergency stop, an interlock, a limit, a role) or goes on as if the thing had worked.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, user, Server};
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use serde_json::{json, Value};

const PART: &str = "G28\nM104 S200\nM140 S60\n;LAYER_CHANGE\nG1 X10 Y10 Z0.2 F3000\nG1 X20 Y10\n;LAYER_CHANGE\nG1 X20 Y20 Z0.4\nM104 S0\nM140 S0\n";

async fn make(s: &Server, boss: &str, id: &str, plugin: &str, settings: Value) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": plugin, "settings": settings})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn job(s: &Server, token: &str, machine: &str, request: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": request, "name": "run", "app": "studio"})),
    )
    .await
}

async fn detail(s: &Server, token: &str, machine: &str) -> Value {
    let (status, body) = call(
        s,
        Method::GET,
        &format!("/api/v1/work/machines/{machine}"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["detail"].clone()
}

async fn wait_for(s: &Server, token: &str, id: &str, wanted: &str) -> Value {
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (_, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/work/jobs/{id}"),
            Some(token),
            None,
        )
        .await;
        if body["state"] == wanted {
            return body;
        }
        assert!(std::time::Instant::now() < limit, "{wanted}: {body}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_plugin_that_fails_ends_the_job_failed_and_leaves_the_machine_latched() {
    let s = common::server("safe-plugin-fails");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make(
        &s,
        &boss,
        "p1",
        "printer3d",
        json!({"sim_rate": 1, "step_ms": 3600000, "sim_fault": "sensor_lost"}),
    )
    .await;
    let (status, queued) = job(&s, &operator, "p1", json!({"kind": "print", "text": PART})).await;
    assert_eq!(status, StatusCode::CREATED, "{queued}");
    let id = queued["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/work/jobs/{id}/start"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // the failure is a failure (never "done"), and the machine stays latched: nothing new is accepted on it
    let failed = wait_for(&s, &operator, &id, "failed").await;
    assert_ne!(failed["state"], "done");
    assert_eq!(detail(&s, &operator, "p1").await["latched"], true);
    let (status, refused) = job(&s, &operator, "p1", json!({"kind": "print", "text": PART})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    // an operator cannot lift the latch, and a refused attempt leaves it in place
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/p1/release",
        Some(&operator),
        None,
    )
    .await;
    assert!(status.is_client_error());
    assert_eq!(detail(&s, &operator, "p1").await["latched"], true);
}

#[tokio::test]
async fn a_value_of_an_interlock_that_is_not_understood_is_refused_and_the_interlock_stays() {
    let s = common::server("safe-unknown-interlock");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    // the enclosure says open and the machine requires it closed
    make(
        &s,
        &boss,
        "l1",
        "laser",
        json!({"sim_rate": 0, "enclosure_closed": false}),
    )
    .await;
    let request = json!({"kind": "gcode", "text": "M4 S100\nG1 X10 F600\nM5"});
    let (status, refused) = job(&s, &operator, "l1", request.clone()).await;
    assert_eq!(
        (status, refused["error"].clone()),
        (StatusCode::CONFLICT, json!("enclosure_open"))
    );
    // a value that is not a boolean is not taken as "closed", "off" or "true": it is refused, and the interlock still holds
    for bad in [
        json!("yes"),
        json!("true"),
        json!(1),
        json!(null),
        json!([true]),
    ] {
        for key in [
            "enclosure_closed",
            "require_enclosure_closed",
            "latch_on_door_open",
            "latch_on_stop",
        ] {
            let mut settings = json!({"sim_rate": 0, "enclosure_closed": false});
            settings[key] = bad.clone();
            let (status, body) = call(
                &s,
                Method::PUT,
                "/api/v1/work/machines/l1",
                Some(&boss),
                Some(json!({ "settings": settings })),
            )
            .await;
            assert!(status.is_client_error(), "{key}={bad}: {body}");
            let (status, still) = job(&s, &operator, "l1", request.clone()).await;
            assert_eq!(
                (status, still["error"].clone()),
                (StatusCode::CONFLICT, json!("enclosure_open")),
                "{key}={bad} loosened the interlock"
            );
        }
    }
    // a limit that is out of range is refused as well
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/l1",
        Some(&boss),
        Some(json!({"settings": {"power_limit_pct": 101, "enclosure_closed": false}})),
    )
    .await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn a_refused_write_of_the_policy_changes_nothing() {
    let s = common::server("safe-policy-write");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    assert!(!s.state.policy.get().update_allow_unsigned);
    for (token, body, why) in [
        (
            &boss,
            json!({"key": "update_allow_unsigned", "value": "yes"}),
            "a value that is not a boolean",
        ),
        (
            &boss,
            json!({"key": "update_allow_unsigned", "value": true}),
            "a looser value without confirmation",
        ),
        (
            &boss,
            json!({"key": "no_such_key", "value": true, "confirm_weak": true}),
            "a key that does not exist",
        ),
        (
            &operator,
            json!({"key": "update_allow_unsigned", "value": true, "confirm_weak": true}),
            "a role that may not",
        ),
    ] {
        let (status, answer) = call(
            &s,
            Method::PUT,
            "/api/v1/auth/policy",
            Some(token),
            Some(body),
        )
        .await;
        assert!(status.is_client_error(), "{why}: {answer}");
        assert!(
            !s.state.policy.get().update_allow_unsigned,
            "{why} loosened the policy"
        );
    }
    assert!(!s.state.policy.get().network_open);
    assert!(!s.dir.join("policy.json").exists(), "nothing was kept");
}

#[tokio::test]
async fn a_closed_or_unknown_login_cannot_release_stop_or_change_anything() {
    let s = common::server("safe-closed-session");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make(&s, &boss, "c1", "cnc", json!({})).await;
    let (status, stop) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/c1/emergency-stop",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stop}");
    assert_eq!(detail(&s, &operator, "c1").await["latched"], true);
    // the administrator closes the login; its token is then like any unknown one
    let (status, _) = call(&s, Method::POST, "/api/v1/auth/logout", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    for token in [boss.as_str(), "not-a-token", ""] {
        let t = Some(token).filter(|t| !t.is_empty());
        for (method, path, body) in [
            (Method::POST, "/api/v1/work/machines/c1/release", None),
            (
                Method::PUT,
                "/api/v1/auth/policy",
                Some(json!({"key": "update_allow_unsigned", "value": true, "confirm_weak": true})),
            ),
            (
                Method::PUT,
                "/api/v1/work/machines/c1",
                Some(json!({"settings": {"sim_rate": 5}})),
            ),
            (Method::DELETE, "/api/v1/work/machines/c1", None),
        ] {
            let (status, answer) = call(&s, method.clone(), path, t, body).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path}: {answer}"
            );
        }
    }
    assert_eq!(detail(&s, &operator, "c1").await["latched"], true);
    assert!(!s.state.policy.get().update_allow_unsigned);
    // a machine that does not exist is not "stopped" either
    let (status, nothing) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/nope/emergency-stop",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{nothing}");
}

fn folder(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("eh3d-server-safe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_in(dir: &std::path::Path) -> std::io::Result<std::sync::Arc<AppState>> {
    AppState::open(ServerConfig {
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    })
}

#[test]
fn a_missing_policy_file_gives_the_conservative_values_and_a_wrong_one_is_not_taken_as_loose() {
    // no file: what server.cfg says, which is the safe side by default
    let dir = folder("policy-missing");
    let state = open_in(&dir).unwrap();
    let hot = state.policy.get();
    assert!(!hot.network_open && !hot.update_allow_unsigned && !hot.compat_write);
    assert!(!hot.slice_enabled && !hot.bambu_lan_enabled && !hot.url_token);
    drop(state);
    // values of the wrong type or of keys this version does not know are left out, never read as "true"
    let wrong = r#"{"network_open":"yes","update_allow_unsigned":1,"compat_write":null,"slice_enabled":"true","no_such_key":true}"#;
    std::fs::write(dir.join("policy.json"), wrong).unwrap();
    let state = open_in(&dir).unwrap();
    let hot = state.policy.get();
    assert!(
        !hot.network_open && !hot.update_allow_unsigned && !hot.compat_write && !hot.slice_enabled
    );
    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_policy_file_that_cannot_be_read_stops_the_server_from_starting_and_does_not_guess() {
    let dir = folder("policy-broken");
    std::fs::write(dir.join("policy.json"), "{ not json at all").unwrap();
    assert!(
        open_in(&dir).is_err(),
        "a policy that cannot be read must not be replaced by a guess"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
