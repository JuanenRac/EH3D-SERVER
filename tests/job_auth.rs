//! The authorization of a job by the hash of what it will do (`POST /api/v1/work/jobs/:id/authorize`, and the hash a start may name), through the real router, against the
//! simulated machines. Off by default (`work_require_authorization`): a job then starts as it always did. Nothing here has run on a real machine.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, scoped_token, server_with, user, Server};
use serde_json::{json, Value};

async fn machine(s: &Server, boss: &str, id: &str, plugin: &str, settings: Value) {
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

async fn prepare(s: &Server, token: &str, machine: &str, request: Value) -> Value {
    let (status, job) = call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": request, "name": "run", "app": "studio"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    job
}

fn load() -> Value {
    json!({"kind": "load_feeders", "feeders": [{"slot": 1, "part": "R_0603_10k", "count": 10}]})
}

async fn post(s: &Server, token: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call(s, Method::POST, path, Some(token), body).await
}

fn required(name: &str) -> Server {
    server_with(name, |c| c.work_require_authorization = true)
}

#[tokio::test]
async fn a_job_has_a_hash_that_depends_on_what_it_will_do_and_not_on_the_order_of_its_keys() {
    let s = common::server("jobauth-hash");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    machine(&s, &boss, "p2", "pnp", json!({"sim_rate": 0})).await;
    let a = prepare(&s, &operator, "p1", load()).await;
    let hash = a["toolpath_sha256"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(a["authorization"], Value::Null);
    // the same request with the keys in another order is the same job
    let same = json!({"feeders": [{"count": 10, "part": "R_0603_10k", "slot": 1}], "kind": "load_feeders"});
    assert_eq!(
        prepare(&s, &operator, "p1", same).await["toolpath_sha256"],
        hash
    );
    // another count, another machine: another hash
    let other = json!({"kind": "load_feeders", "feeders": [{"slot": 1, "part": "R_0603_10k", "count": 11}]});
    assert_ne!(
        prepare(&s, &operator, "p1", other).await["toolpath_sha256"],
        hash
    );
    assert_ne!(
        prepare(&s, &operator, "p2", load()).await["toolpath_sha256"],
        hash
    );
    // the hash is in the read of the job too
    let (_, read) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{}", a["id"].as_str().unwrap()),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(read["toolpath_sha256"], hash);
}

#[tokio::test]
async fn by_default_a_job_starts_as_it_always_did_and_a_start_may_name_its_hash() {
    let s = common::server("jobauth-off");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    let job = prepare(&s, &operator, "p1", load()).await;
    let id = job["id"].as_str().unwrap();
    let hash = job["toolpath_sha256"].as_str().unwrap();
    // a hash that is not the job's stops the start, and nothing starts
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/start"),
        Some(json!({"toolpath_sha256": "0".repeat(64)})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("stale_authorization")),
        "{v}"
    );
    let (_, read) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(read["state"], "queued");
    // a body that does not read is refused, never ignored
    for bad in [
        json!({"toolpath_sha256": "abc"}),
        json!({"other": 1}),
        json!({"toolpath_sha256": 5}),
    ] {
        let (status, v) = post(
            &s,
            &operator,
            &format!("/api/v1/work/jobs/{id}/start"),
            Some(bad),
        )
        .await;
        assert_eq!(
            (status, v["error"].clone()),
            (StatusCode::BAD_REQUEST, json!("bad_request")),
            "{v}"
        );
    }
    // the right hash, or none, starts it
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/start"),
        Some(json!({"toolpath_sha256": hash.to_ascii_uppercase()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // another machine, because the first one is busy with the job that started
    machine(&s, &boss, "p2", "pnp", json!({"sim_rate": 0})).await;
    let second = prepare(&s, &operator, "p2", load()).await;
    let (status, _) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{}/start", second["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn when_it_is_required_a_job_of_a_machine_that_moves_starts_only_with_its_authorization() {
    let s = required("jobauth-on");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    machine(&s, &boss, "d1", "demo", json!({"step_ms": 0})).await;
    let job = prepare(&s, &operator, "p1", load()).await;
    let id = job["id"].as_str().unwrap().to_string();
    let hash = job["toolpath_sha256"].as_str().unwrap().to_string();
    let start = format!("/api/v1/work/jobs/{id}/start");
    // no authorization yet
    let (status, v) = post(&s, &operator, &start, None).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("authorization_required")),
        "{v}"
    );
    let (status, v) = post(
        &s,
        &operator,
        &start,
        Some(json!({"toolpath_sha256": hash})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("authorization_required")),
        "{v}"
    );
    // another hash cannot be authorised; the job is not changed
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/authorize"),
        Some(json!({"toolpath_sha256": "f".repeat(64)})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("stale_authorization")),
        "{v}"
    );
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/authorize"),
        None,
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("bad_request")),
        "{v}"
    );
    // the right hash: it is recorded with who did it
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/authorize"),
        Some(json!({"toolpath_sha256": hash})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["authorization"]["by"], "oper1");
    assert_eq!(v["authorization"]["toolpath_sha256"], json!(hash));
    assert!(v["history"].to_string().contains("authorized"));
    // a start that names another hash is refused even though the job is authorised
    let (status, v) = post(
        &s,
        &operator,
        &start,
        Some(json!({"toolpath_sha256": "a".repeat(64)})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("stale_authorization")),
        "{v}"
    );
    let (status, v) = post(
        &s,
        &operator,
        &start,
        Some(json!({"toolpath_sha256": hash})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // a job that ran cannot be authorised again
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/authorize"),
        Some(json!({"toolpath_sha256": hash})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("not_ready")),
        "{v}"
    );
    // a plugin that moves nothing is not asked for it
    let demo = prepare(&s, &operator, "d1", json!({"kind": "demo", "steps": 1})).await;
    let (status, _) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{}/start", demo["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_scope_and_the_role_of_the_authorization_and_the_audit_of_it() {
    let s = required("jobauth-scope");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    let job = prepare(&s, &operator, "p1", load()).await;
    let id = job["id"].as_str().unwrap();
    let path = format!("/api/v1/work/jobs/{id}/authorize");
    let body = json!({"toolpath_sha256": job["toolpath_sha256"]});
    let (status, _) = post(&s, &viewer, &path, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&s, Method::POST, &path, None, Some(body.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // a token that carries the scopes of a start but not the one of the authorization cannot authorise
    let starter = scoped_token(&s, &boss, "operator", &["jobs:start", "jobs:prepare"]).await;
    let (status, v) = post(&s, &starter, &path, Some(body.clone())).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::FORBIDDEN, json!("forbidden_scope")),
        "{v}"
    );
    // a program of the design suites can never be given the scope
    let (status, v) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&boss),
        Some(json!({"name": "cam program", "role": "operator", "template": "cam", "scopes": ["jobs:authorize"]})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::FORBIDDEN, json!("forbidden_scope")),
        "{v}"
    );
    let authorizer = scoped_token(&s, &boss, "operator", &["jobs:authorize"]).await;
    let (status, v) = post(&s, &authorizer, &path, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, audit) = call(&s, Method::GET, "/api/v1/audit", Some(&boss), None).await;
    assert!(audit.to_string().contains("work_job_authorize"), "{audit}");
}

#[tokio::test]
async fn with_a_fixed_seed_no_made_up_hash_is_ever_taken_for_the_one_of_a_job() {
    let s = required("jobauth-seed");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    let job = prepare(&s, &operator, "p1", load()).await;
    let id = job["id"].as_str().unwrap();
    let real = job["toolpath_sha256"].as_str().unwrap().to_string();
    // the same 200 made-up hashes on every run, some of them one digit away from the real one
    let mut seed: u64 = 0x0A07_5EED;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for step in 0..200 {
        let mut made: Vec<u8> = real.bytes().collect();
        if step % 2 == 0 {
            let at = (next() % 64) as usize;
            made[at] = if made[at] == b'0' { b'1' } else { b'0' };
        } else {
            made = (0..64)
                .map(|_| b"0123456789abcdef"[(next() % 16) as usize])
                .collect();
        }
        let made = String::from_utf8(made).unwrap();
        let (status, v) = post(
            &s,
            &operator,
            &format!("/api/v1/work/jobs/{id}/authorize"),
            Some(json!({"toolpath_sha256": made})),
        )
        .await;
        assert_eq!(
            (status, v["error"].clone()),
            (StatusCode::CONFLICT, json!("stale_authorization")),
            "step {step}: {made}"
        );
    }
    let (_, read) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(read["authorization"], Value::Null);
    let (status, v) = post(
        &s,
        &operator,
        &format!("/api/v1/work/jobs/{id}/start"),
        None,
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("authorization_required"))
    );
}
