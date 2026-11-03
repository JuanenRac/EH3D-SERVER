//! The computing tasks end to end: the requests go through the real router and the engine of the self-test runs on its own thread. What is tested includes what must be
//! refused: other people's tasks, a second request with the same key and another body, a queue that is full, a cancel at the wrong moment.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Server {
    app: Router,
    state: Arc<AppState>,
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.state.stop_all();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn server_with(name: &str, change: impl FnOnce(&mut ServerConfig)) -> Server {
    let dir = std::env::temp_dir().join(format!(
        "eh3d-server-tasks-api-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = ServerConfig {
        data_dir: dir.clone(),
        tasks_selftest: true,
        ..ServerConfig::default()
    };
    change(&mut cfg);
    let state = AppState::open(cfg).unwrap();
    Server {
        app: router(state.clone()),
        state,
        dir,
    }
}

fn server(name: &str) -> Server {
    server_with(name, |_| {})
}

async fn call(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        request = request.header("authorization", format!("Bearer {t}"));
    }
    let request = match body {
        Some(b) => request
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = s.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn admin(s: &Server) -> String {
    let code = s
        .state
        .auth
        .lock()
        .unwrap()
        .setup_code()
        .unwrap()
        .to_string();
    let (status, _) = call(
        s,
        Method::POST,
        "/api/v1/setup",
        None,
        Some(json!({"code": code, "name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(
        s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    body["token"].as_str().unwrap().to_string()
}

/// A program token with the role.
async fn token(s: &Server, admin: &str, name: &str, role: &str) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/tokens",
        Some(admin),
        Some(json!({"name": name, "role": role})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().unwrap().to_string()
}

async fn wait_state(s: &Server, token: &str, id: &str, wanted: &[&str]) -> Value {
    let limit = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/tasks/{id}"),
            Some(token),
            None,
        )
        .await;
        let state = body["state"].as_str().unwrap_or("").to_string();
        if wanted.contains(&state.as_str()) {
            return body;
        }
        assert!(Instant::now() < limit, "the task stays {state}: {body}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn selftest(params: Value) -> Value {
    json!({"kind": "server.selftest", "name": "a test", "params": params})
}

#[tokio::test]
async fn a_task_runs_to_the_end_and_gives_its_result() {
    let s = server("run");
    let a = admin(&s).await;
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(selftest(json!({"steps": 4, "step_ms": 1, "text": "abc"}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("t-") && eh3d_server::ids::is_valid(eh3d_server::ids::Kind::Task, &id));
    assert!(matches!(body["state"].as_str(), Some("queued" | "running")));
    let done = wait_state(&s, &a, &id, &["done", "failed"]).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        done["progress"],
        json!({"done": 4, "total": 4, "unit": "steps"})
    );
    // the SHA-256 of "abc"
    assert_eq!(
        done["result"]["data"]["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(done["engine"]["version"], "1");
    assert!(done["started"].is_u64() && done["finished"].is_u64());
}

#[tokio::test]
async fn a_task_that_fails_says_why_with_a_coded_diagnostic() {
    let s = server("fail");
    let a = admin(&s).await;
    let (_, body) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(selftest(json!({"steps": 1, "fail": true}))),
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();
    let t = wait_state(&s, &a, &id, &["failed", "done"]).await;
    assert_eq!(t["state"], "failed");
    assert_eq!(t["result"]["diagnostics"][0]["code"], "EH3D-TASK-001");
    assert_eq!(t["result"]["diagnostics"][0]["severity"], "error");
    assert!(t["message"].is_string());
}

#[tokio::test]
async fn the_requests_that_cannot_make_a_task_are_refused_with_their_codes() {
    let s = server("refuse");
    let a = admin(&s).await;
    let post = |body: Value| call(&s, Method::POST, "/api/v1/tasks", Some(&a), Some(body));
    // a kind nobody knows, and a kind of the suite that has no engine in this build
    let (st, b) = post(json!({"kind": "nothing.at.all"})).await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_kind"))
    );
    let (st, b) = post(json!({"kind": "phoenix.check"})).await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("engine_missing"))
    );
    // parameters that do not fit the engine, or are not an object
    let (_, b) = post(selftest(json!({"steps": 0}))).await;
    assert_eq!(b["error"], "bad_params");
    let (_, b) = post(selftest(json!({"nope": 1}))).await;
    assert_eq!(b["error"], "bad_params");
    let (_, b) = post(json!({"kind": "server.selftest", "params": [1]})).await;
    assert_eq!(b["error"], "bad_params");
    // an artifact that does not exist
    let (_, b) = post(
        json!({"kind": "server.selftest", "project": "a-00000000-0000-7000-8000-000000000000"}),
    )
    .await;
    assert_eq!(b["error"], "no_project");
    // the name, the key
    let (_, b) = post(json!({"kind": "server.selftest", "name": "  "})).await;
    assert_eq!(b["error"], "bad_name");
    let (_, b) = post(json!({"kind": "server.selftest", "idempotency_key": "has space"})).await;
    assert_eq!(b["error"], "bad_params");
    // a body that is not a request
    let (st, b) = post(json!({"name": "no kind"})).await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_request"))
    );
    // parameters over the limit
    let big = "x".repeat(70 * 1024);
    let (_, b) = post(selftest(json!({"text": big}))).await;
    assert_eq!(b["error"], "too_big");
    // nothing was made
    let (_, list) = call(&s, Method::GET, "/api/v1/tasks", Some(&a), None).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn a_viewer_reads_and_does_not_write_and_nobody_gets_in_without_a_token() {
    let s = server("roles");
    let a = admin(&s).await;
    let viewer = token(&s, &a, "viewer-prog", "viewer").await;
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&viewer),
        Some(selftest(json!({}))),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&viewer), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        None,
        Some(selftest(json!({}))),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        &s,
        Method::GET,
        "/api/v1/tasks/kinds",
        Some("eh3d_forged"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_tasks_of_another_owner_do_not_exist_for_you_but_the_administrator_sees_all() {
    let s = server("owner");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    // a second token with the same name is another owner: the name is not the identity
    let ana2 = token(&s, &a, "ana", "operator").await;
    let (_, mine) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&ana),
        Some(selftest(json!({"steps": 1, "step_ms": 1}))),
    )
    .await;
    let id = mine["id"].as_str().unwrap().to_string();
    wait_state(&s, &ana, &id, &["done"]).await;
    let path = format!("/api/v1/tasks/{id}");
    for (method, p) in [
        (Method::GET, path.clone()),
        (Method::DELETE, path.clone()),
        (Method::POST, format!("{path}/cancel")),
    ] {
        let (st, b) = call(&s, method, &p, Some(&ana2), None).await;
        assert_eq!(
            (st, b["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_task")),
            "{p}"
        );
    }
    // it does not exist in the list of the other either
    let (_, list) = call(&s, Method::GET, "/api/v1/tasks", Some(&ana2), None).await;
    assert_eq!(list, json!([]));
    // the answer is the same as for an id that was never made
    let (st, b) = call(
        &s,
        Method::GET,
        "/api/v1/tasks/t-00000000-0000-7000-8000-000000000000",
        Some(&ana2),
        None,
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_task"))
    );
    // the owner and the administrator see it
    let (st, _) = call(&s, Method::GET, &path, Some(&ana), None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, list) = call(&s, Method::GET, "/api/v1/tasks", Some(&a), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // and the other one, once cancelled by nobody, is still there
    let (st, _) = call(&s, Method::GET, &path, Some(&a), None).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn a_running_task_is_cancelled_and_leaves_no_result() {
    let s = server("cancel");
    let a = admin(&s).await;
    let (_, t) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(selftest(json!({"steps": 1000, "step_ms": 100}))),
    )
    .await;
    let id = t["id"].as_str().unwrap().to_string();
    wait_state(&s, &a, &id, &["running"]).await;
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/tasks/{id}/cancel"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["accepted"], true);
    let done = wait_state(&s, &a, &id, &["cancelled", "done", "failed"]).await;
    assert_eq!(done["state"], "cancelled", "{done}");
    assert!(
        done["result"].is_null(),
        "a cancelled task leaves no result"
    );
    // cancelling again, after the end: not accepted, nothing changes
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/tasks/{id}/cancel"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!((st, b["accepted"].clone()), (StatusCode::OK, json!(false)));
    assert_eq!(b["task"]["state"], "cancelled");
}

#[tokio::test]
async fn a_queued_task_is_cancelled_at_once_and_a_running_one_cannot_be_deleted() {
    let s = server_with("queue", |c| c.max_concurrent_tasks = 1);
    let a = admin(&s).await;
    let slow = selftest(json!({"steps": 1000, "step_ms": 100}));
    let (_, first) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(slow.clone()),
    )
    .await;
    let (_, second) = call(&s, Method::POST, "/api/v1/tasks", Some(&a), Some(slow)).await;
    let (first, second) = (
        first["id"].as_str().unwrap().to_string(),
        second["id"].as_str().unwrap().to_string(),
    );
    wait_state(&s, &a, &first, &["running"]).await;
    let (_, now) = call(
        &s,
        Method::GET,
        &format!("/api/v1/tasks/{second}"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(now["state"], "queued", "only one runs at a time");
    // a task that has not finished cannot be removed
    let (st, b) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/tasks/{first}"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("task_active"))
    );
    // the queued one is cancelled in the answer itself
    let (_, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/tasks/{second}/cancel"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(
        (b["accepted"].clone(), b["task"]["state"].clone()),
        (json!(true), json!("cancelled"))
    );
    // the filters of the list
    let (_, list) = call(
        &s,
        Method::GET,
        "/api/v1/tasks?state=cancelled",
        Some(&a),
        None,
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = call(
        &s,
        Method::GET,
        "/api/v1/tasks?kind=phoenix.check",
        Some(&a),
        None,
    )
    .await;
    assert_eq!(list, json!([]));
    // when the first is cancelled and gone from the list, the list is empty again
    call(
        &s,
        Method::POST,
        &format!("/api/v1/tasks/{first}/cancel"),
        Some(&a),
        None,
    )
    .await;
    wait_state(&s, &a, &first, &["cancelled"]).await;
    for id in [&first, &second] {
        let (st, _) = call(
            &s,
            Method::DELETE,
            &format!("/api/v1/tasks/{id}"),
            Some(&a),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    let (_, list) = call(&s, Method::GET, "/api/v1/tasks", Some(&a), None).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn the_queue_and_the_user_have_limits() {
    let s = server_with("limits", |c| {
        c.max_concurrent_tasks = 1;
        c.max_tasks = 3;
        c.tasks_per_user = 2;
    });
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let bob = token(&s, &a, "bob", "operator").await;
    let slow = selftest(json!({"steps": 1000, "step_ms": 100}));
    for _ in 0..2 {
        let (st, _) = call(
            &s,
            Method::POST,
            "/api/v1/tasks",
            Some(&ana),
            Some(slow.clone()),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED);
    }
    // a third of the same user: over the quota of the user
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&ana),
        Some(slow.clone()),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("quota"))
    );
    // another user takes the last place of the queue, and the next one finds it full
    let (st, _) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&bob),
        Some(slow.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, b) = call(&s, Method::POST, "/api/v1/tasks", Some(&bob), Some(slow)).await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("queue_full"))
    );
    s.state.stop_all();
}

#[tokio::test]
async fn the_same_key_with_the_same_body_gives_the_same_task_and_with_another_body_a_conflict() {
    let s = server("idem");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let bob = token(&s, &a, "bob", "operator").await;
    let body = json!({"kind": "server.selftest", "name": "once", "params": {"steps": 1, "step_ms": 1, "text": "x"}, "idempotency_key": "key-1"});
    let (st1, one) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&ana),
        Some(body.clone()),
    )
    .await;
    // the same body with the parameters written in another order is the same body
    let again = json!({"idempotency_key": "key-1", "params": {"text": "x", "step_ms": 1, "steps": 1}, "name": "once", "kind": "server.selftest"});
    let (st2, two) = call(&s, Method::POST, "/api/v1/tasks", Some(&ana), Some(again)).await;
    assert_eq!((st1, st2), (StatusCode::CREATED, StatusCode::CREATED));
    assert_eq!(one["id"], two["id"]);
    let (_, list) = call(&s, Method::GET, "/api/v1/tasks", Some(&ana), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "one task, not two");
    // another body under the same key
    let mut other = body.clone();
    other["params"]["steps"] = json!(2);
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&ana),
        Some(other.clone()),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_conflict"))
    );
    // the key is the key of whoever sent it: another user may use the same text for something else
    let (st, three) = call(&s, Method::POST, "/api/v1/tasks", Some(&bob), Some(other)).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_ne!(three["id"], one["id"]);
}

#[tokio::test]
async fn the_kinds_say_what_is_available_and_the_switch_turns_the_tasks_off() {
    let s = server("kinds");
    let a = admin(&s).await;
    let (st, b) = call(&s, Method::GET, "/api/v1/tasks/kinds", Some(&a), None).await;
    assert_eq!(st, StatusCode::OK);
    let kinds = b["kinds"].as_array().unwrap();
    let find = |k: &str| kinds.iter().find(|x| x["kind"] == k).cloned().unwrap();
    assert_eq!(find("server.selftest")["available"], true);
    assert_eq!(find("phoenix.check")["available"], false);
    drop(s);
    // without the self-test engine only the kinds of the suite are named, none available in this build
    let s = server_with("kinds-b", |c| c.tasks_selftest = false);
    let a = admin(&s).await;
    let (_, b) = call(&s, Method::GET, "/api/v1/tasks/kinds", Some(&a), None).await;
    assert!(b["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .all(|k| k["available"] == false));
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(selftest(json!({}))),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_kind"))
    );
    drop(s);
    // the switch: every request of the tasks answers that they are off
    let s = server_with("kinds-c", |c| c.tasks_enabled = false);
    let a = admin(&s).await;
    for (method, path) in [
        (Method::GET, "/api/v1/tasks"),
        (Method::GET, "/api/v1/tasks/kinds"),
        (Method::POST, "/api/v1/tasks"),
    ] {
        let body = (method == Method::POST).then(|| selftest(json!({})));
        let (st, b) = call(&s, method, path, Some(&a), body).await;
        assert_eq!(
            (st, b["error"].as_str()),
            (StatusCode::NOT_IMPLEMENTED, Some("tasks_off")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_task_that_runs_too_long_fails_with_the_timeout_and_a_restart_marks_the_running_one() {
    let s = server_with("timeout", |c| c.task_timeout_s = 1);
    let a = admin(&s).await;
    let (_, t) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(selftest(json!({"steps": 1000, "step_ms": 200}))),
    )
    .await;
    let id = t["id"].as_str().unwrap().to_string();
    let done = wait_state(&s, &a, &id, &["failed", "done", "cancelled"]).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(done["result"]["diagnostics"][0]["code"], "EH3D-TASK-002");
}

#[tokio::test]
async fn the_tasks_survive_a_restart_and_the_one_that_was_running_is_failed() {
    let dir = std::env::temp_dir().join(format!(
        "eh3d-server-tasks-api-restart-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        tasks_selftest: true,
        max_concurrent_tasks: 1,
        ..ServerConfig::default()
    };
    let (running, queued, finished);
    {
        let state = AppState::open(cfg.clone()).unwrap();
        let s = Server {
            app: router(state.clone()),
            state,
            dir: dir.clone(),
        };
        let a = admin(&s).await;
        let quick = selftest(json!({"steps": 1, "step_ms": 1}));
        let slow = selftest(json!({"steps": 1000, "step_ms": 100}));
        let (_, f) = call(&s, Method::POST, "/api/v1/tasks", Some(&a), Some(quick)).await;
        finished = f["id"].as_str().unwrap().to_string();
        wait_state(&s, &a, &finished, &["done"]).await;
        let (_, r) = call(
            &s,
            Method::POST,
            "/api/v1/tasks",
            Some(&a),
            Some(slow.clone()),
        )
        .await;
        let (_, q) = call(&s, Method::POST, "/api/v1/tasks", Some(&a), Some(slow)).await;
        running = r["id"].as_str().unwrap().to_string();
        queued = q["id"].as_str().unwrap().to_string();
        wait_state(&s, &a, &running, &["running"]).await;
        // the server "stops" here without ending the work: the file holds what a power cut would leave
        std::mem::forget(s);
    }
    let state = AppState::open(cfg).unwrap();
    {
        let book = state.tasks.book.lock().unwrap();
        let r = book.get(&running).unwrap();
        assert_eq!(r.state.name(), "failed");
        assert_eq!(r.message.as_deref(), Some("interrupted by restart"));
        assert_eq!(book.get(&finished).unwrap().state.name(), "done");
    }
    // the queued one went on by itself after the restart
    let limit = Instant::now() + Duration::from_secs(10);
    loop {
        let st = state
            .tasks
            .book
            .lock()
            .unwrap()
            .get(&queued)
            .unwrap()
            .state
            .name();
        if st == "running" {
            break;
        }
        assert!(Instant::now() < limit, "the queued task is {st}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    state.stop_all();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_task_never_moves_a_machine_and_a_job_never_starts_by_a_task() {
    // the two families are apart: a finished task changes no job, and the list of jobs knows nothing of tasks
    let s = server("apart");
    let a = admin(&s).await;
    let (_, t) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(selftest(json!({"steps": 1, "step_ms": 1}))),
    )
    .await;
    wait_state(&s, &a, t["id"].as_str().unwrap(), &["done"]).await;
    let (_, jobs) = call(&s, Method::GET, "/api/v1/jobs", Some(&a), None).await;
    assert_eq!(jobs, json!([]));
    let (st, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/jobs/{}", t["id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
