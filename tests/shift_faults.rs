//! A shift at the workshop with things going wrong, through the real router and the simulated machine: a machine whose node does not answer, a job that is stopped and
//! removed, and a seeded storm of requests (make, start, stop) that must never answer with a server error and never leave two jobs working on one machine.
//! The storm comes from a seeded generator, so every run is the same. Nothing here has run with a real machine.

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

fn server(name: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("eh3d-server-shift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    let state = AppState::open(cfg).unwrap();
    Server {
        app: router(state.clone()),
        state,
        dir,
    }
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
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["token"].as_str().unwrap().to_string()
}

async fn machine(s: &Server, token: &str, id: &str, connection: Value) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/machines",
        Some(token),
        Some(json!({"id": id, "name": "Bungard", "model": "ccd-mtc", "connection": connection})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn drill_job() -> Value {
    json!({
        "kind": "drill", "name": "board",
        "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}, {"number": 2, "feed_mm_min": 800, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}, {"tool": 2, "x_mm": 30, "y_mm": 20}]
    })
}

async fn make_job(s: &Server, token: &str, machine_id: &str) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        &format!("/api/v1/machines/{machine_id}/jobs"),
        Some(token),
        Some(drill_job()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn state_of(s: &Server, token: &str, job: &str) -> String {
    let (_, body) = call(
        s,
        Method::GET,
        &format!("/api/v1/jobs/{job}"),
        Some(token),
        None,
    )
    .await;
    body["state"].as_str().unwrap_or("").to_string()
}

/// Waits until the job is in one of the states; the dialogs of the operator are confirmed on the way when asked.
async fn wait_state(s: &Server, token: &str, job: &str, wanted: &[&str], confirm: bool) -> String {
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let state = state_of(s, token, job).await;
        if wanted.contains(&state.as_str()) {
            return state;
        }
        if confirm && state == "waiting_operator" {
            let _ = call(
                s,
                Method::POST,
                &format!("/api/v1/jobs/{job}/confirm"),
                Some(token),
                None,
            )
            .await;
        }
        assert!(Instant::now() < limit, "timeout in state {state}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// How many machines of the fleet have a job that is working.
async fn working(s: &Server, token: &str, machine_id: &str) -> usize {
    let (_, fleet) = call(s, Method::GET, "/api/v1/fleet", Some(token), None).await;
    fleet["machines"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|m| m["id"] == machine_id && !m["job"].is_null())
                .count()
        })
        .unwrap_or(0)
}

/// A small deterministic generator (SplitMix64): the same seed always gives the same numbers.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[tokio::test]
async fn a_machine_whose_node_does_not_answer_ends_the_job_and_is_free_again() {
    let s = server("dead-node");
    let token = admin(&s).await;
    machine(
        &s,
        &token,
        "far",
        json!({"kind": "node", "address": "127.0.0.1:1", "token": "node-token"}),
    )
    .await;
    let job = make_job(&s, &token, "far").await;
    let (status, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{job}/start"),
        Some(&token),
        None,
    )
    .await;
    assert!(status != StatusCode::INTERNAL_SERVER_ERROR);
    if status == StatusCode::OK {
        // the job does not hang: it ends in a state that is not working
        let state = wait_state(&s, &token, &job, &["failed", "cancelled", "done"], false).await;
        assert_ne!(
            state, "done",
            "a job cannot be done on a node that is not there"
        );
    }
    let state = state_of(&s, &token, &job).await;
    assert!(
        !matches!(
            state.as_str(),
            "running" | "connecting" | "waiting_operator"
        ),
        "{state}"
    );
    assert_eq!(working(&s, &token, "far").await, 0);
}

#[tokio::test]
async fn a_stopped_job_ends_as_cancelled_is_removed_and_the_machine_takes_the_next_one() {
    let s = server("stop-remove");
    let token = admin(&s).await;
    machine(&s, &token, "m1", json!({"kind": "demo"})).await;
    let first = make_job(&s, &token, "m1").await;
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{first}/start"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_state(&s, &token, &first, &["waiting_operator"], false).await;
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{first}/cancel"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_state(&s, &token, &first, &["cancelled"], false).await;
    assert_eq!(working(&s, &token, "m1").await, 0);
    assert_eq!(
        call(
            &s,
            Method::DELETE,
            &format!("/api/v1/jobs/{first}"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &s,
            Method::GET,
            &format!("/api/v1/jobs/{first}"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // the machine is free for the next job, which runs to the end
    let second = make_job(&s, &token, "m1").await;
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{second}/start"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_state(&s, &token, &second, &["done"], true).await;
}

#[tokio::test]
async fn a_seeded_storm_of_requests_never_gives_a_server_error_and_never_two_jobs_on_one_machine() {
    let s = server("storm");
    let token = admin(&s).await;
    machine(&s, &token, "m1", json!({"kind": "demo"})).await;
    let mut rng = Seeded(20_261_115);
    let mut jobs: Vec<String> = Vec::new();
    let mut busy_refusals = 0;
    for _ in 0..40 {
        let (status, body) = match rng.below(3) {
            0 => {
                let (status, body) = call(
                    &s,
                    Method::POST,
                    "/api/v1/machines/m1/jobs",
                    Some(&token),
                    Some(drill_job()),
                )
                .await;
                if status == StatusCode::OK {
                    jobs.push(body["id"].as_str().unwrap().to_string());
                }
                (status, body)
            }
            1 if !jobs.is_empty() => {
                let job = jobs[rng.below(jobs.len())].clone();
                let (status, body) = call(
                    &s,
                    Method::POST,
                    &format!("/api/v1/jobs/{job}/start"),
                    Some(&token),
                    None,
                )
                .await;
                if body["error"] == "machine_busy" {
                    busy_refusals += 1;
                }
                (status, body)
            }
            _ if !jobs.is_empty() => {
                let job = jobs[rng.below(jobs.len())].clone();
                call(
                    &s,
                    Method::POST,
                    &format!("/api/v1/jobs/{job}/cancel"),
                    Some(&token),
                    None,
                )
                .await
            }
            _ => (StatusCode::OK, Value::Null),
        };
        assert!(!status.is_server_error(), "{status}: {body}");
        assert!(working(&s, &token, "m1").await <= 1);
    }
    // the end of the shift: everything that works is stopped, and the machine is free
    for job in &jobs {
        let _ = call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{job}/cancel"),
            Some(&token),
            None,
        )
        .await;
    }
    let limit = Instant::now() + Duration::from_secs(60);
    while working(&s, &token, "m1").await > 0 {
        assert!(Instant::now() < limit, "the machine did not become free");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        !jobs.is_empty(),
        "the storm made no job: the test tries nothing (busy refusals: {busy_refusals})"
    );
}
