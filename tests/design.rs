//! What Studio needs of the server for the design and factory pages, through the real router: the run of the design program on a board of a project (and the refusals while
//! the server is not set up for it), the audit, the identifiers of the keys, the change of the switches of security while the server runs, and the quota of the store.
//!
//! The "design program" of these tests is the test program itself: given a command it does not know, the test harness runs no test and ends well, which is all a run needs.
//! So no test of this file may have a name with the words the harness would take as a filter (`fabpack`, `board.json`).

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{admin, call, enc, scoped_token, send, server, server_with, user, Server};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn this_program() -> std::path::PathBuf {
    std::env::current_exe().expect("the test program has a path")
}

fn set_up(name: &str) -> Server {
    server_with(name, |c| {
        c.design_run = true;
        c.design_program = Some(this_program());
    })
}

async fn make_project(s: &Server, token: &str, app: &str, name: &str) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        &format!("/api/v1/apps/{app}/projects"),
        Some(token),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn put_file(s: &Server, token: &str, app: &str, id: &str, name: &str, bytes: &[u8]) {
    let r = send(
        s,
        Method::PUT,
        &format!(
            "/api/v1/apps/{app}/projects/{id}/files/content?name={}",
            enc(name)
        ),
        Some(token),
        &[],
        Some(bytes.to_vec()),
        Some("application/octet-stream"),
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.json());
}

async fn read_file(s: &Server, token: &str, id: &str, name: &str) -> common::Reply {
    send(
        s,
        Method::GET,
        &format!(
            "/api/v1/apps/pcb/projects/{id}/files/content?name={}",
            enc(name)
        ),
        Some(token),
        &[],
        None,
        None,
    )
    .await
}

async fn start_run(s: &Server, token: &str, body: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/apps/pcb/jobs",
        Some(token),
        Some(body),
    )
    .await
}

/// Waits for a job of the queue to end and returns it.
async fn until_finished(s: &Server, token: &str, job: &str) -> Value {
    for _ in 0..400 {
        let (status, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/work/jobs/{job}"),
            Some(token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if matches!(
            body["state"].as_str(),
            Some("done" | "failed" | "cancelled")
        ) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the job did not end");
}

#[tokio::test]
async fn the_run_is_unavailable_until_the_settings_and_the_switch_allow_it() {
    let boss_server = server("design-off");
    let boss = admin(&boss_server).await;
    let id = make_project(&boss_server, &boss, "pcb", "front").await;
    let ask = json!({"kind": "check", "project": id, "file": "a.json"});
    let (status, body) = start_run(&boss_server, &boss, ask).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("design_run_unavailable"))
    );
    // on, but with no program named
    let only_switch = server_with("design-no-program", |c| c.design_run = true);
    let boss2 = admin(&only_switch).await;
    let id2 = make_project(&only_switch, &boss2, "pcb", "front").await;
    let (status, body) = start_run(
        &only_switch,
        &boss2,
        json!({"kind": "check", "project": id2, "file": "a.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("design_run_unavailable"))
    );
    // a program named, but the switch off
    let only_program = server_with("design-no-switch", |c| {
        c.design_program = Some(this_program())
    });
    let boss3 = admin(&only_program).await;
    let id3 = make_project(&only_program, &boss3, "pcb", "front").await;
    let (status, body) = start_run(
        &only_program,
        &boss3,
        json!({"kind": "check", "project": id3, "file": "a.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("design_run_unavailable"))
    );
}

#[tokio::test]
async fn a_run_keeps_its_report_as_a_file_of_the_project() {
    let s = set_up("design-run");
    let boss = admin(&s).await;
    let id = make_project(&s, &boss, "pcb", "front").await;
    put_file(&s, &boss, "pcb", &id, "boards/main.json", b"{\"board\": 1}").await;
    let (status, job) = start_run(
        &s,
        &boss,
        json!({"kind": "fabpack", "project": id, "file": "boards/main.json", "options": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!(job["plugin"], "phoenix");
    assert_eq!(job["app"], "pcb");
    assert_eq!(job["view"]["report"], "reports/fabpack.json");
    let done = until_finished(&s, &boss, job["id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "done", "{done}");
    let r = read_file(&s, &boss, &id, "reports/fabpack.json").await;
    assert_eq!(r.status, StatusCode::OK);
    let report = r.json();
    assert_eq!(report["kind"], "fabpack");
    assert_eq!(report["file"], "boards/main.json");
    assert_eq!(report["ok"], true);
    assert_eq!(report["exit_code"], 0);
    assert!(report["text"].is_string() && report["report"].is_null());
    // the copy of the board made for the run is gone, and the audit knows who ran what
    assert!(!s
        .dir
        .join("design-scratch")
        .join(done["id"].as_str().unwrap())
        .exists());
    let (_, audit) = call(
        &s,
        Method::GET,
        "/api/v1/audit?event=design_run",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(audit["lines"][0]["kind"], "fabpack");
    assert_eq!(audit["lines"][0]["who"], "juan");
    // a second run writes a new version over the same file
    let (status, again) = start_run(
        &s,
        &boss,
        json!({"kind": "fabpack", "project": id, "options": {"file": "boards/main.json"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    let second = until_finished(&s, &boss, again["id"].as_str().unwrap()).await;
    assert_eq!(second["state"], "done", "{second}");
    // the kind that asks for JSON is refused its text and fails (the harness knows no such option), but its report is kept all the same
    let (status, job) = start_run(
        &s,
        &boss,
        json!({"kind": "check", "project": id, "file": "boards/main.json"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let ended = until_finished(&s, &boss, job["id"].as_str().unwrap()).await;
    assert!(matches!(ended["state"].as_str(), Some("done" | "failed")));
    let kept = read_file(&s, &boss, &id, "reports/check.json").await;
    assert_eq!(kept.status, StatusCode::OK);
    assert_eq!(kept.json()["kind"], "check");
}

#[tokio::test]
async fn a_program_that_is_not_there_fails_the_job_and_writes_nothing() {
    let missing = std::env::temp_dir().join(format!("eh3d-no-program-{}", std::process::id()));
    let s = server_with("design-missing", |c| {
        c.design_run = true;
        c.design_program = Some(missing.clone());
    });
    let boss = admin(&s).await;
    let id = make_project(&s, &boss, "pcb", "front").await;
    put_file(&s, &boss, "pcb", &id, "main.json", b"{}").await;
    let (status, job) = start_run(
        &s,
        &boss,
        json!({"kind": "bom", "project": id, "file": "main.json"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let ended = until_finished(&s, &boss, job["id"].as_str().unwrap()).await;
    assert_eq!(ended["state"], "failed");
    assert!(
        ended["message"].as_str().unwrap().contains("did not start"),
        "{ended}"
    );
    let r = read_file(&s, &boss, &id, "reports/bom.json").await;
    assert_eq!(r.error(), "unknown_file");
}

#[tokio::test]
async fn a_request_that_does_not_fit_is_refused_before_anything_runs() {
    let s = set_up("design-refusals");
    let boss = admin(&s).await;
    let id = make_project(&s, &boss, "pcb", "front").await;
    put_file(&s, &boss, "pcb", &id, "main.json", b"{}").await;
    let (status, body) = start_run(
        &s,
        &boss,
        json!({"kind": "route", "project": id, "file": "main.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_design_request"))
    );
    let (status, body) = start_run(&s, &boss, json!({"kind": "check", "project": id})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_design_request"))
    );
    let (status, body) = start_run(
        &s,
        &boss,
        json!({"kind": "check", "project": id, "file": "main.json", "options": {"profile": "x"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_design_request"))
    );
    let (status, body) = start_run(&s, &boss, 
        json!({"kind": "fabpack", "project": id, "file": "main.json", "options": {"profile": "--out"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_design_request"))
    );
    let (status, body) = start_run(
        &s,
        &boss,
        json!({"kind": "check", "project": id, "file": "nothing.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_file"))
    );
    let (status, body) = start_run(
        &s,
        &boss,
        json!({"kind": "check", "project": id, "file": "../x.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_filename"))
    );
    let (status, body) = start_run(
        &s,
        &boss,
        json!({"kind": "check", "project": "p-nothing", "file": "main.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_project"))
    );
    // nobody else's project exists for another person, and a viewer may not run anything
    let ana = user(&s, &boss, "ana", "operator").await;
    let (status, body) = start_run(
        &s,
        &ana,
        json!({"kind": "check", "project": id, "file": "main.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_project"))
    );
    let eve = user(&s, &boss, "eve", "viewer").await;
    let (status, body) = start_run(
        &s,
        &eve,
        json!({"kind": "check", "project": id, "file": "main.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    // a token that may prepare jobs but not write projects cannot keep a report
    let token = scoped_token(&s, &boss, "operator", &["jobs:prepare"]).await;
    let (status, body) = start_run(
        &s,
        &token,
        json!({"kind": "check", "project": id, "file": "main.json"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden_scope"))
    );
    // no job was made by any of this
    let (_, jobs) = call(&s, Method::GET, "/api/v1/work/jobs", Some(&boss), None).await;
    assert_eq!(jobs["total"], 0);
}

#[tokio::test]
async fn the_switch_of_the_run_changes_while_the_server_runs() {
    let s = server_with("design-hot-switch", |c| {
        c.design_program = Some(this_program())
    });
    let boss = admin(&s).await;
    let id = make_project(&s, &boss, "pcb", "front").await;
    put_file(&s, &boss, "pcb", &id, "main.json", b"{}").await;
    let ask = json!({"kind": "fabpack", "project": id, "file": "main.json"});
    let (status, _) = start_run(&s, &boss, ask.clone()).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/auth/policy",
        Some(&boss),
        Some(json!({"key": "design_run", "value": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("weak_confirm_required"))
    );
    let (status, _) = start_run(&s, &boss, ask.clone()).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "nothing changed");
    let (status, policy) = call(
        &s,
        Method::PUT,
        "/api/v1/auth/policy",
        Some(&boss),
        Some(json!({"key": "design_run", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let switch = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "design_run")
        .cloned()
        .unwrap();
    assert_eq!(
        (switch["value"].clone(), switch["safe"].clone()),
        (json!(true), json!(false))
    );
    assert!(policy["attention"]
        .as_array()
        .unwrap()
        .contains(&json!("design_run")));
    let (status, job) = start_run(&s, &boss, ask.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let ended = until_finished(&s, &boss, job["id"].as_str().unwrap()).await;
    assert_eq!(ended["state"], "done", "{ended}");
    // and back to the strict value, which needs no confirmation
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/auth/policy",
        Some(&boss),
        Some(json!({"key": "design_run", "value": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = start_run(&s, &boss, ask).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn the_policy_changes_what_can_change_and_refuses_the_rest() {
    let s = server("design-policy");
    let boss = admin(&s).await;
    let url = "/api/v1/auth/policy";
    let (status, policy) = call(&s, Method::GET, url, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    let editable: Vec<&str> = policy["editable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(editable.contains(&"password_min_len") && editable.contains(&"design_run"));
    assert!(!editable.contains(&"session_hours"));
    // not an administrator
    let ana = user(&s, &boss, "ana", "operator").await;
    let (status, body) = call(
        &s,
        Method::PUT,
        url,
        Some(&ana),
        Some(json!({"key": "audit_denials", "value": false})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    // a key that cannot change while the server runs, and a value that does not fit
    for bad in [
        json!({"key": "session_hours", "value": 5}),
        json!({"key": "password_min_len", "value": 3}),
        json!({"key": "password_min_len", "value": "long"}),
        json!({"key": "url_token", "value": "yes"}),
        json!({"key": "nothing_like_it", "value": true}),
    ] {
        let (status, body) = call(&s, Method::PUT, url, Some(&boss), Some(bad.clone())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_policy")),
            "{bad}"
        );
    }
    // a stricter value takes effect at once: a ten-character password no longer passes
    let (status, _) = call(
        &s,
        Method::PUT,
        url,
        Some(&boss),
        Some(json!({"key": "password_min_len", "value": 12})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/users",
        Some(&boss),
        Some(json!({"name": "bob", "role": "viewer", "password": "short-pass1"})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("weak_password"))
    );
    // a looser one needs the confirmation, and the address then carries the token
    let (status, body) = call(
        &s,
        Method::PUT,
        url,
        Some(&boss),
        Some(json!({"key": "url_token", "value": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("weak_confirm_required"))
    );
    let (_, health) = call(&s, Method::GET, "/api/v1/health", Some(&boss), None).await;
    assert_eq!(health["features"]["url_token"], false);
    let (status, _) = call(
        &s,
        Method::PUT,
        url,
        Some(&boss),
        Some(json!({"key": "url_token", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, health) = call(&s, Method::GET, "/api/v1/health", Some(&boss), None).await;
    assert_eq!(health["features"]["url_token"], true);
    // the changes are in the audit, with the old and the new value, and are kept for the next start
    let (_, audit) = call(
        &s,
        Method::GET,
        "/api/v1/audit?event=policy_change",
        Some(&boss),
        None,
    )
    .await;
    let lines = audit["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["key"], "url_token");
    assert_eq!(
        (lines[0]["from"].clone(), lines[0]["to"].clone()),
        (json!("false"), json!("true"))
    );
    let kept = std::fs::read_to_string(s.dir.join("policy.json")).unwrap();
    let kept: Value = serde_json::from_str(&kept).unwrap();
    assert_eq!(
        (kept["password_min_len"].clone(), kept["url_token"].clone()),
        (json!(12), json!(true))
    );
}

#[tokio::test]
async fn the_audit_is_read_with_its_filters_by_the_administrator_only() {
    let s = server("design-audit");
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana", "operator").await;
    let (status, body) = call(&s, Method::GET, "/api/v1/audit", Some(&ana), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (status, body) = call(&s, Method::GET, "/api/v1/audit", None, None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
    let (status, all) = call(
        &s,
        Method::GET,
        "/api/v1/audit?limit=1000",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["enabled"], true);
    let total = all["lines"].as_array().unwrap().len();
    assert!(total >= 3, "{all}");
    let times: Vec<u64> = all["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["at"].as_u64().unwrap())
        .collect();
    assert!(
        times.windows(2).all(|w| w[0] >= w[1]),
        "newest first: {times:?}"
    );
    let (_, few) = call(&s, Method::GET, "/api/v1/audit?limit=2", Some(&boss), None).await;
    assert_eq!(few["lines"].as_array().unwrap().len(), 2);
    let (_, mine) = call(&s, Method::GET, "/api/v1/audit?who=juan", Some(&boss), None).await;
    assert!(mine["lines"]
        .as_array()
        .unwrap()
        .iter()
        .all(|l| l["who"] == "juan"));
    let (_, none) = call(
        &s,
        Method::GET,
        "/api/v1/audit?since=99999999999",
        Some(&boss),
        None,
    )
    .await;
    assert!(none["lines"].as_array().unwrap().is_empty());
    let text = all.to_string();
    for secret in [
        boss.as_str(),
        ana.as_str(),
        "una-clave-larga",
        common::PASSWORD,
    ] {
        assert!(!text.contains(secret), "the audit shows {secret}");
    }
    // with the file off there is nothing to read, and it is said
    let quiet = server_with("design-audit-off", |c| c.audit_file = false);
    let boss = admin(&quiet).await;
    let (status, body) = call(&quiet, Method::GET, "/api/v1/audit", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["enabled"].clone(), body["lines"].clone()),
        (json!(false), json!([]))
    );
}

#[tokio::test]
async fn the_keys_are_listed_by_identifier_and_the_certificate_by_fingerprint() {
    let der = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let pem_path =
        std::env::temp_dir().join(format!("eh3d-design-cert-{}.pem", std::process::id()));
    {
        use base64::Engine;
        let body = base64::engine::general_purpose::STANDARD.encode(der);
        std::fs::write(
            &pem_path,
            format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n"),
        )
        .unwrap();
    }
    let with_cert = server_with("design-keys", |c| c.tls_cert_file = Some(pem_path.clone()));
    let boss = admin(&with_cert).await;
    let (status, made) = call(
        &with_cert,
        Method::POST,
        "/api/v1/tokens",
        Some(&boss),
        Some(json!({"name": "pcb tool", "role": "operator"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let secret = made["token"].as_str().unwrap().to_string();
    let (status, keys) = call(
        &with_cert,
        Method::GET,
        "/api/v1/auth/keys",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{keys}");
    let tokens = keys["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0]["id"], made["id"]);
    assert_eq!(tokens[0]["name"], "pcb tool");
    assert!(tokens[0]["age_s"].as_u64().is_some());
    assert_eq!(keys["tls"]["configured"], true);
    assert_eq!(keys["tls"]["sha256"], hex::encode(Sha256::digest(der)));
    assert!(
        !keys.to_string().contains(&secret),
        "the secret is never listed"
    );
    let ana = user(&with_cert, &boss, "ana", "operator").await;
    let (status, body) = call(
        &with_cert,
        Method::GET,
        "/api/v1/auth/keys",
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    // no certificate named: the fingerprint is null and the page says so
    let plain = server("design-keys-plain");
    let boss = admin(&plain).await;
    let (_, keys) = call(&plain, Method::GET, "/api/v1/auth/keys", Some(&boss), None).await;
    assert_eq!(
        (
            keys["tls"]["configured"].clone(),
            keys["tls"]["sha256"].clone()
        ),
        (json!(false), Value::Null)
    );
    // a file that holds no certificate gives a null fingerprint, not an error
    let junk = std::env::temp_dir().join(format!("eh3d-design-junk-{}.pem", std::process::id()));
    std::fs::write(&junk, "not a certificate").unwrap();
    let odd = server_with("design-keys-junk", |c| c.tls_cert_file = Some(junk.clone()));
    let boss = admin(&odd).await;
    let (status, keys) = call(&odd, Method::GET, "/api/v1/auth/keys", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            keys["tls"]["configured"].clone(),
            keys["tls"]["sha256"].clone()
        ),
        (json!(true), Value::Null)
    );
    let _ = std::fs::remove_file(&pem_path);
    let _ = std::fs::remove_file(&junk);
}

#[tokio::test]
async fn the_quota_tells_the_limits_and_what_is_used() {
    let s = server_with("design-quota", |c| {
        c.project_max_mb = 4;
        c.project_max_projects = 10;
    });
    let boss = admin(&s).await;
    let first = make_project(&s, &boss, "pcb", "front").await;
    make_project(&s, &boss, "pcb", "back").await;
    make_project(&s, &boss, "studio", "elsewhere").await;
    put_file(&s, &boss, "pcb", &first, "main.json", &[b'x'; 1500]).await;
    put_file(&s, &boss, "pcb", &first, "notes.txt", b"hello").await;
    let (status, q) = call(&s, Method::GET, "/api/v1/apps/pcb/quota", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{q}");
    assert_eq!(q["app"], "pcb");
    assert_eq!(q["limits"]["project_max_mb"], 4);
    assert_eq!(q["limits"]["project_max_projects"], 10);
    assert_eq!(q["limits"]["project_max_files"], 2000);
    assert_eq!(q["usage"]["projects_in_app"], 2);
    assert_eq!(q["usage"]["projects_left"], 8);
    assert_eq!(q["usage"]["visible_projects"], 2);
    assert_eq!(q["usage"]["files"], 2);
    assert_eq!(q["usage"]["bytes"], 1505);
    let listed = q["projects"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    let mine = listed.iter().find(|p| p["id"] == first.as_str()).unwrap();
    assert_eq!(
        (mine["bytes"].clone(), mine["files"].clone()),
        (json!(1505), json!(2))
    );
    assert_eq!(mine["bytes_left"], 4 * 1024 * 1024 - 1505);
    assert_eq!(mine["files_left"], 1998);
    // another person sees the limit and the number of projects of the application, but only their own projects
    let ana = user(&s, &boss, "ana", "operator").await;
    let (_, q) = call(&s, Method::GET, "/api/v1/apps/pcb/quota", Some(&ana), None).await;
    assert_eq!(q["usage"]["projects_in_app"], 2);
    assert_eq!(q["usage"]["visible_projects"], 0);
    assert_eq!(q["projects"], json!([]));
    // an application the server does not accept, and a token without the scope
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/apps/nope-app/quota",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_app"))
    );
    let narrow = scoped_token(&s, &boss, "viewer", &["machines:view"]).await;
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/apps/pcb/quota",
        Some(&narrow),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden_scope"))
    );
}
