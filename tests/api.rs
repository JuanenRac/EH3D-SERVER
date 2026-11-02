//! The server's interface end to end: the requests go through the real router, the jobs run on the simulator, nothing is mocked inside.

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
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-{name}-{}", std::process::id()));
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

/// A server that keeps its logins on the disk (it is off by default).
fn server_persisting(name: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        persist_sessions: true,
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

/// Makes the administrator and returns the token of their login.
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

async fn demo_machine(s: &Server, token: &str, id: &str) {
    let (status, body) = call(s, Method::POST, "/api/v1/machines", Some(token), Some(json!({"id": id, "name": "Bungard", "model": "ccd-mtc", "connection": {"kind": "demo"}}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn drill_job() -> Value {
    json!({
        "kind": "drill", "name": "board",
        "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}, {"number": 2, "feed_mm_min": 800, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}, {"tool": 2, "x_mm": 30, "y_mm": 20}]
    })
}

/// Waits until the job is in one of the states; confirms the dialogs of the operator on the way when asked to.
async fn wait_for(s: &Server, token: &str, job: &str, wanted: &[&str], confirm: bool) -> Value {
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/jobs/{job}"),
            Some(token),
            None,
        )
        .await;
        let state = body["state"].as_str().unwrap_or("").to_string();
        if wanted.contains(&state.as_str()) {
            return body;
        }
        if state == "waiting_operator" && confirm {
            let (status, _) = call(
                s,
                Method::POST,
                &format!("/api/v1/jobs/{job}/confirm"),
                Some(token),
                None,
            )
            .await;
            assert!(status == StatusCode::OK || status == StatusCode::CONFLICT);
        }
        assert!(
            !matches!(state.as_str(), "failed"),
            "the job failed: {body}"
        );
        assert!(Instant::now() < limit, "timeout in state {state}: {body}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn nothing_works_without_a_login_and_the_first_administrator_needs_the_setup_code() {
    let s = server("login");
    assert_eq!(
        call(&s, Method::GET, "/api/v1/health", None, None).await.1["name"],
        "eh3d-server"
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/setup", None, None).await.1["needed"],
        true
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/machines", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/machines", Some("nonsense"), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/setup",
        None,
        Some(json!({"code": "00000000", "name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("bad_setup"))
    );
    let token = admin(&s).await;
    assert_eq!(
        call(&s, Method::GET, "/api/v1/setup", None, None).await.1["needed"],
        false
    );
    let (status, who) = call(&s, Method::GET, "/api/v1/session", Some(&token), None).await;
    assert_eq!(
        (status, who["name"].as_str(), who["role"].as_str()),
        (StatusCode::OK, Some("juan"), Some("admin"))
    );
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "mala"})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("bad_credentials"))
    );
    assert_eq!(
        call(&s, Method::POST, "/api/v1/logout", Some(&token), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/session", Some(&token), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn roles_decide_who_may_do_what() {
    let s = server("roles");
    let boss = admin(&s).await;
    for (name, role) in [("viewer1", "viewer"), ("operator1", "operator")] {
        let (status, _) = call(
            &s,
            Method::POST,
            "/api/v1/users",
            Some(&boss),
            Some(json!({"name": name, "role": role, "password": "clave-larga-123"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let login = |name: &'static str| {
        let s = &s;
        async move {
            call(
                s,
                Method::POST,
                "/api/v1/login",
                None,
                Some(json!({"name": name, "password": "clave-larga-123"})),
            )
            .await
            .1["token"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let viewer = login("viewer1").await;
    let operator = login("operator1").await;
    demo_machine(&s, &boss, "m1").await;
    // a viewer reads and nothing else
    assert_eq!(
        call(&s, Method::GET, "/api/v1/machines", Some(&viewer), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/jobs", Some(&viewer), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &s,
            Method::POST,
            "/api/v1/machines/m1/jobs",
            Some(&viewer),
            Some(drill_job())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/users", Some(&viewer), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // an operator prepares jobs and cannot manage machines or users
    assert_eq!(
        call(
            &s,
            Method::POST,
            "/api/v1/machines/m1/jobs",
            Some(&operator),
            Some(drill_job())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &s,
            Method::POST,
            "/api/v1/machines",
            Some(&operator),
            Some(json!({"id": "m2", "name": "x", "connection": {"kind": "demo"}}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/tokens", Some(&operator), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // the administrator makes a token for a program, shown once, and the program works with it
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&boss),
        Some(json!({"name": "drillpro-pc", "role": "operator"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let program = made["token"].as_str().unwrap().to_string();
    let (_, who) = call(&s, Method::GET, "/api/v1/session", Some(&program), None).await;
    assert_eq!(
        (who["name"].as_str(), who["is_token"].as_bool()),
        (Some("drillpro-pc"), Some(true))
    );
    assert_eq!(
        call(
            &s,
            Method::POST,
            "/api/v1/machines/m1/preview",
            Some(&program),
            Some(drill_job())
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, listed) = call(&s, Method::GET, "/api/v1/tokens", Some(&boss), None).await;
    assert!(!listed.to_string().contains(&program));
    // the last administrator stays
    assert_eq!(
        call(&s, Method::DELETE, "/api/v1/users/juan", Some(&boss), None)
            .await
            .1["error"],
        "last_admin"
    );
}

#[tokio::test]
async fn a_preview_shows_the_work_in_the_machines_order_and_a_bad_job_is_refused_with_a_code() {
    let s = server("preview");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let (status, view) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/preview",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["holes"].as_array().unwrap().len(), 3);
    assert_eq!(view["hole_ends"].as_array().unwrap().len(), 3);
    assert!(view["estimate_s"].as_f64().unwrap() > 1.0);
    assert_eq!(view["bounds_mm"], json!([10.0, 10.0, 30.0, 20.0]));
    // nothing was created by looking
    assert_eq!(
        call(&s, Method::GET, "/api/v1/jobs", Some(&token), None)
            .await
            .1,
        json!([])
    );
    let mut bad = drill_job();
    bad["holes"] = json!([{"tool": 9, "x_mm": 1, "y_mm": 1}]);
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/preview",
        Some(&token),
        Some(bad),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_tool"))
    );
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/nowhere/preview",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_machine"))
    );
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/preview",
        Some(&token),
        Some(json!({"kind": "paint"})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_request"))
    );
}

#[tokio::test]
async fn a_drilling_job_runs_to_the_end_on_the_simulator_with_the_operators_dialogs() {
    let s = server("run");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let mut events = s.state.events.subscribe();
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (job["state"].as_str(), job["holes_total"].as_u64()),
        (Some("ready"), Some(3))
    );
    let id = job["id"].as_str().unwrap().to_string();
    assert!(job["view"]["holes"].is_array());
    // a job does not start by itself
    assert_eq!(
        call(
            &s,
            Method::GET,
            &format!("/api/v1/jobs/{id}"),
            Some(&token),
            None
        )
        .await
        .1["state"],
        "ready"
    );
    let (status, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{id}/start"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // the first dialog is the change of tool 1
    let waiting = wait_for(&s, &token, &id, &["waiting_operator"], false).await;
    assert_eq!(waiting["pause"]["kind"], "tool_change");
    assert_eq!(waiting["pause"]["tool"], 1);
    // it cannot be started twice
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{id}/start"),
            Some(&token),
            None
        )
        .await
        .1["error"],
        "not_ready"
    );
    let done = wait_for(&s, &token, &id, &["done"], true).await;
    assert_eq!(
        (done["holes_done"].as_u64(), done["holes_total"].as_u64()),
        (Some(3), Some(3))
    );
    assert_eq!(done["items_done"], done["items_total"]);
    assert!(done["finished"].as_u64().is_some());
    // every change was told to whoever listens
    let mut states = Vec::new();
    while let Ok(text) = events.try_recv() {
        let v: Value = serde_json::from_str(&text).unwrap();
        if v["type"] == "job" {
            states.push(v["job"]["state"].as_str().unwrap().to_string());
        }
    }
    for wanted in ["ready", "connecting", "waiting_operator", "done"] {
        assert!(
            states.iter().any(|x| x == wanted),
            "no event for {wanted}: {states:?}"
        );
    }
    // it stays in the list, and a finished job can be removed
    assert_eq!(
        call(
            &s,
            Method::GET,
            "/api/v1/jobs?state=done",
            Some(&token),
            None
        )
        .await
        .1
        .as_array()
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        call(
            &s,
            Method::DELETE,
            &format!("/api/v1/jobs/{id}"),
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
            &format!("/api/v1/jobs/{id}"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn one_job_at_a_time_on_a_machine_and_a_running_job_can_be_cancelled() {
    let s = server("busy");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let first = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await
    .1["id"]
        .as_str()
        .unwrap()
        .to_string();
    let second = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await
    .1["id"]
        .as_str()
        .unwrap()
        .to_string();
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
    wait_for(&s, &token, &first, &["waiting_operator"], false).await;
    let (status, body) = call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{second}/start"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("machine_busy"))
    );
    // a running job is not deleted, and the machine is not removed under it
    assert_eq!(
        call(
            &s,
            Method::DELETE,
            &format!("/api/v1/jobs/{first}"),
            Some(&token),
            None
        )
        .await
        .1["error"],
        "job_active"
    );
    assert_eq!(
        call(
            &s,
            Method::DELETE,
            "/api/v1/machines/m1",
            Some(&token),
            None
        )
        .await
        .1["error"],
        "machine_busy"
    );
    // stopping it ends it and frees the machine
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
    wait_for(&s, &token, &first, &["cancelled"], false).await;
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
    wait_for(&s, &token, &second, &["waiting_operator"], false).await;
    // a job that is only ready is cancelled where it stands
    let third = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await
    .1["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{third}/cancel"),
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
            &format!("/api/v1/jobs/{third}"),
            Some(&token),
            None
        )
        .await
        .1["state"],
        "cancelled"
    );
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{third}/cancel"),
            Some(&token),
            None
        )
        .await
        .1["error"],
        "not_cancellable"
    );
    // the dialog of a job that is not running has nobody to answer it
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{third}/confirm"),
            Some(&token),
            None
        )
        .await
        .1["error"],
        "not_running"
    );
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{second}/cancel"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_for(&s, &token, &second, &["cancelled"], false).await;
}

#[tokio::test]
async fn two_programs_queued_on_a_machine_keep_their_tools_off_and_a_resumed_one_leaves_the_done_holes_out(
) {
    let s = server("queue");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    // the drilling goes on after a stop: the first hole of tool 1 was done and tool 2 is off
    let mut drill = drill_job();
    drill["skip_tools"] = json!([2]);
    drill["skip"] = json!(1);
    let route = json!({"kind": "route", "name": "outline", "skip_tools": [2],
        "tools": [
            {"number": 1, "depth_mm": 1.7, "iterations": 1, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000},
            {"number": 2, "depth_mm": 0.3, "iterations": 1, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000}],
        "paths": [{"pen": 1, "points_mm": [[10, 10], [40, 10], [40, 30], [10, 10]]}, {"pen": 2, "points_mm": [[12, 12], [20, 12]]}]});
    let mut ids = Vec::new();
    for body in [drill, route] {
        let (status, job) = call(
            &s,
            Method::POST,
            "/api/v1/machines/m1/jobs",
            Some(&token),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{job}");
        // what the job will do is what is left: the tools that are off and the holes already done are not in it
        assert_eq!(job["holes_total"], 1, "{job}");
        assert_eq!(
            job["view"]["skipped"],
            if job["kind"] == "drill" { 2 } else { 1 },
            "{job}"
        );
        assert_eq!(job["view"]["skipped_tools"], json!([2]));
        ids.push(job["id"].as_str().unwrap().to_string());
    }
    // the fleet shows both ready; they run one after the other, each with the dialogs of the tools that are on
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&token), None).await;
    assert_eq!(fleet["machines"][0]["ready"], 2, "{fleet}");
    for id in &ids {
        let (status, body) = call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{id}/start"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut tools = Vec::new();
        let limit = Instant::now() + Duration::from_secs(60);
        loop {
            let (_, now) = call(
                &s,
                Method::GET,
                &format!("/api/v1/jobs/{id}"),
                Some(&token),
                None,
            )
            .await;
            match now["state"].as_str().unwrap() {
                "done" => break,
                "waiting_operator" => {
                    if now["pause"]["kind"] == "tool_change" {
                        tools.push(now["pause"]["tool"].as_u64().unwrap());
                    }
                    call(
                        &s,
                        Method::POST,
                        &format!("/api/v1/jobs/{id}/confirm"),
                        Some(&token),
                        None,
                    )
                    .await;
                }
                "failed" => panic!("failed: {now}"),
                _ => {}
            }
            assert!(Instant::now() < limit, "timeout: {now}");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        tools.dedup();
        assert_eq!(tools, vec![1], "only tool 1 is asked for");
    }
    // a request whose list of tools off is not a list of numbers is refused
    let mut bad = drill_job();
    bad["skip_tools"] = json!("2");
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(bad),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_request"))
    );
}

#[tokio::test]
async fn a_job_can_come_as_files_and_a_routing_job_runs_too() {
    let s = server("files");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let ncd = "M48\nINCH\nT1C0.8\n%\nT1\nX1000Y1000\nX2000Y1000\n";
    let dtd = format!("{}1 0.0 1000 24000\n", "# comment\n".repeat(19));
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(json!({"kind": "drill", "name": "from files", "files": {"ncd": ncd, "dtd": dtd}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(
        (job["name"].as_str(), job["holes_total"].as_u64()),
        (Some("from files"), Some(2))
    );
    let route = json!({"kind": "route", "name": "outline",
        "tools": [{"number": 1, "depth_mm": 1.7, "iterations": 2, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000}],
        "paths": [{"pen": 1, "points_mm": [[10, 10], [40, 10], [40, 30], [10, 30], [10, 10]]}]});
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(route),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["kind"], "route");
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{id}/start"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let done = wait_for(&s, &token, &id, &["done"], true).await;
    assert_eq!(done["items_done"], done["items_total"]);
}

#[tokio::test]
async fn machines_keep_their_node_token_secret_and_survive_a_restart_of_the_server() {
    let s = server("machines");
    let token = admin(&s).await;
    let node = json!({"id": "bungard", "name": "Bungard CCD", "model": "ccd-mtc", "connection": {"kind": "node", "address": "192.168.0.50:5800", "token": "node-secret-token"}, "cfg": "y_search_hz = 2000\n"});
    let (status, shown) = call(
        &s,
        Method::POST,
        "/api/v1/machines",
        Some(&token),
        Some(node),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["connection"]["token_set"], true);
    assert!(!shown.to_string().contains("node-secret-token"));
    assert!(!call(
        &s,
        Method::GET,
        "/api/v1/machines/bungard",
        Some(&token),
        None
    )
    .await
    .1
    .to_string()
    .contains("node-secret-token"));
    // a typo in the tuning is refused
    let (status, body) = call(&s, Method::PUT, "/api/v1/machines/bungard", Some(&token), Some(json!({"id": "bungard", "name": "x", "connection": {"kind": "demo"}, "cfg": "y_serch_hz = 1\n"}))).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_cfg"))
    );
    // a restart of the server: the data folder is read again
    let again = AppState::open(ServerConfig {
        data_dir: s.dir.clone(),
        ..ServerConfig::default()
    })
    .unwrap();
    assert_eq!(again.machines.lock().unwrap().list().len(), 1);
    assert!(!again.auth.lock().unwrap().needs_setup());
}

#[tokio::test]
async fn several_machines_in_several_stations_run_at_once_and_the_fleet_shows_them_all() {
    let s = server("fleet");
    let token = admin(&s).await;
    // three machines: two in the station of the bench, one in the room, one without a station
    for (id, station) in [
        ("bench-1", "bench"),
        ("bench-2", "bench"),
        ("room-1", "room"),
        ("spare", ""),
    ] {
        let (status, body) = call(
            &s,
            Method::POST,
            "/api/v1/machines",
            Some(&token),
            Some(json!({"id": id, "name": id, "station": station, "connection": {"kind": "demo"}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["station"], station);
    }
    // a station is named like an id
    let (status, body) = call(&s, Method::POST, "/api/v1/machines", Some(&token), Some(json!({"id": "bad", "name": "bad", "station": "Not Ok!", "connection": {"kind": "demo"}}))).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_station"))
    );
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&token), None).await;
    assert_eq!(
        fleet["summary"],
        json!({"machines": 4, "idle": 4, "busy": 0, "waiting": 0, "stations": 2})
    );
    // the stations come first, in order, and the machines without one at the end
    let ids: Vec<&str> = fleet["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["bench", "room", ""]);
    // a job on each of two machines: both run at the same time
    let mut jobs = Vec::new();
    for m in ["bench-1", "room-1"] {
        let (_, job) = call(
            &s,
            Method::POST,
            &format!("/api/v1/machines/{m}/jobs"),
            Some(&token),
            Some(drill_job()),
        )
        .await;
        let id = job["id"].as_str().unwrap().to_string();
        let (status, _) = call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{id}/start"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        jobs.push(id);
    }
    for id in &jobs {
        wait_for(&s, &token, id, &["waiting_operator"], false).await;
    }
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&token), None).await;
    assert_eq!(
        fleet["summary"],
        json!({"machines": 4, "idle": 2, "busy": 0, "waiting": 2, "stations": 2})
    );
    let by_id = |id: &str| {
        fleet["machines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id("bench-1")["state"], "waiting");
    assert_eq!(by_id("bench-1")["job"]["id"], jobs[0].as_str());
    assert_eq!(by_id("room-1")["job"]["id"], jobs[1].as_str());
    assert_eq!(by_id("bench-2")["state"], "idle");
    assert!(by_id("bench-2")["job"].is_null());
    // a viewer can look, nobody without a login can
    assert_eq!(
        call(&s, Method::GET, "/api/v1/fleet", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    // and the machines that were running stop independently of each other
    for id in &jobs {
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{id}/cancel"),
            Some(&token),
            None,
        )
        .await;
        wait_for(&s, &token, id, &["cancelled"], false).await;
    }
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&token), None).await;
    assert_eq!(fleet["summary"]["idle"], 4);
}

async fn raw_get(app: axum::Router, path: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_pages_of_studio_are_served_at_the_root_when_the_folder_is_set() {
    let base = std::env::temp_dir().join(format!("eh3d-server-studio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pages = base.join("pages");
    std::fs::create_dir_all(pages.join("assets")).unwrap();
    std::fs::write(pages.join("index.html"), "<title>Studio</title>").unwrap();
    std::fs::write(pages.join("assets").join("app.js"), "console.log(1)").unwrap();
    let state = AppState::open(ServerConfig {
        data_dir: base.join("data"),
        studio_dir: Some(pages.clone()),
        ..ServerConfig::default()
    })
    .unwrap();
    let app = router(state.clone());
    assert_eq!(
        raw_get(app.clone(), "/").await,
        (StatusCode::OK, "<title>Studio</title>".into())
    );
    assert_eq!(
        raw_get(app.clone(), "/assets/app.js").await.1,
        "console.log(1)"
    );
    // a page of the application that is not a file is the start page, so a reload works
    assert_eq!(
        raw_get(app.clone(), "/machines").await.1,
        "<title>Studio</title>"
    );
    // the API is still the API
    let (status, body) = raw_get(app.clone(), "/api/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("eh3d-server"));
    state.stop_all();

    // without the folder the server answers only the API
    let plain = AppState::open(ServerConfig {
        data_dir: base.join("data2"),
        ..ServerConfig::default()
    })
    .unwrap();
    assert_eq!(
        raw_get(router(plain.clone()), "/").await.0,
        StatusCode::NOT_FOUND
    );
    plain.stop_all();
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn the_updates_are_offered_to_an_administrator_when_the_server_has_a_folder_for_them() {
    // without the folder the server says it does not offer them
    let off = server("updates-off");
    let token = admin(&off).await;
    let (status, body) = call(&off, Method::GET, "/api/v1/updates", Some(&token), None).await;
    assert_eq!(
        (status, body["enabled"].clone()),
        (StatusCode::OK, json!(false))
    );
    let (status, body) = call(
        &off,
        Method::POST,
        "/api/v1/updates/check",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::NOT_IMPLEMENTED, json!("updates_off"))
    );

    // with it
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-updates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("meet");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("components.json"),
        r#"[{"id":"server","latest":"9.9.9","url":"https://example.org/r"}]"#,
    )
    .unwrap();
    let cfg = ServerConfig {
        data_dir: dir.join("data"),
        updates_dir: Some(folder.clone()),
        ..ServerConfig::default()
    };
    let state = AppState::open(cfg).unwrap();
    let on = Server {
        app: router(state.clone()),
        state,
        dir: dir.clone(),
    };
    // nobody gets in without a login
    let (status, _) = call(&on, Method::GET, "/api/v1/updates", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let token = admin(&on).await;
    let (status, body) = call(&on, Method::GET, "/api/v1/updates", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(true));
    let server_row = &body["components"][0];
    assert_eq!(
        (
            server_row["id"].as_str(),
            server_row["latest"].as_str(),
            server_row["newer"].as_bool()
        ),
        (Some("server"), Some("9.9.9"), Some(true))
    );
    // a check and an update are asked for through the folder; what cannot be updated is refused
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/apply",
        Some(&token),
        Some(json!({"components": ["kernel"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/apply",
        Some(&token),
        Some(json!({"components": ["server", "studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let request: Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("request.json")).unwrap())
            .unwrap();
    assert_eq!(
        (request["action"].clone(), request["components"].clone()),
        (json!("apply"), json!(["server", "studio"]))
    );
    // one request at a time
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/check",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, body) = call(&on, Method::GET, "/api/v1/updates", Some(&token), None).await;
    assert_eq!(body["pending"], json!(true));
}

#[tokio::test]
async fn an_administrator_puts_back_the_version_an_update_left_behind_and_empties_the_history() {
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-undo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("meet");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("previous.json"),
        r#"{"studio":{"available":true,"version":"0.0.1"}}"#,
    )
    .unwrap();
    let cfg = ServerConfig {
        data_dir: dir.join("data"),
        updates_dir: Some(folder.clone()),
        ..ServerConfig::default()
    };
    let state = AppState::open(cfg).unwrap();
    let on = Server {
        app: router(state.clone()),
        state,
        dir: dir.clone(),
    };
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/undo",
        None,
        Some(json!({"components": ["studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let token = admin(&on).await;
    let (_, body) = call(&on, Method::GET, "/api/v1/updates", Some(&token), None).await;
    let studio = &body["components"][1];
    assert_eq!(
        (studio["can_undo"].clone(), studio["previous"].clone()),
        (json!(true), json!("0.0.1"))
    );
    assert_eq!(body["components"][0]["can_undo"], json!(false));
    // a viewer cannot ask for it
    let viewer = {
        let (status, _) = call(
            &on,
            Method::POST,
            "/api/v1/users",
            Some(&token),
            Some(json!({"name": "ana", "password": "otra-clave-larga", "role": "viewer"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, b) = call(
            &on,
            Method::POST,
            "/api/v1/login",
            None,
            Some(json!({"name": "ana", "password": "otra-clave-larga"})),
        )
        .await;
        b["token"].as_str().unwrap().to_string()
    };
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/undo",
        Some(&viewer),
        Some(json!({"components": ["studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/clear-history",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // what has no version kept is refused, and so is a component that does not exist
    for bad in ["server", "kernel"] {
        let (status, body) = call(
            &on,
            Method::POST,
            "/api/v1/updates/undo",
            Some(&token),
            Some(json!({"components": [bad]})),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("update_refused")),
            "{bad}"
        );
    }
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/undo",
        Some(&token),
        Some(json!({"components": ["studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let request: Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("request.json")).unwrap())
            .unwrap();
    assert_eq!(
        (request["action"].clone(), request["components"].clone()),
        (json!("undo"), json!(["studio"]))
    );
    // one request at a time, for the history too
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/clear-history",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    std::fs::remove_file(folder.join("request.json")).unwrap();
    let (status, _) = call(
        &on,
        Method::POST,
        "/api/v1/updates/clear-history",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let request: Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("request.json")).unwrap())
            .unwrap();
    assert_eq!(request["action"], json!("clear_history"));
    // without the folder the server says it does not offer them
    let off = server("undo-off");
    let t = admin(&off).await;
    let (status, body) = call(
        &off,
        Method::POST,
        "/api/v1/updates/clear-history",
        Some(&t),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("updates_off"))
    );
}

/// A node of the drill head that says what the real one says: a status, a measure (only with its token) and a camera that sends two pictures.
fn fake_head(token: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 2048];
            let n = c.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let authorised = req.contains(&format!("Authorization: Bearer {token}\r\n"));
            let reply: Vec<u8> = if req.starts_with("GET /api/status ") {
                b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"node\":\"head-1\",\"camera\":true,\"camera_model\":\"ov5640\"}".to_vec()
            } else if req.starts_with("POST /api/measure") && authorised {
                format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{{\"ok\":true,\"distance_mm\":42.5,\"asked\":{}}}", if req.contains("samples=5") { 5 } else { 7 }).into_bytes()
            } else if req.starts_with("GET /stream") && authorised {
                b"HTTP/1.0 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n--frame\r\nPICTURE-ONE\r\n--frame\r\nPICTURE-TWO\r\n".to_vec()
            } else {
                b"HTTP/1.0 401 Unauthorized\r\nContent-Type: application/json\r\n\r\n{\"error\":\"unauthorized\"}".to_vec()
            };
            let _ = c.write_all(&reply);
        }
    });
    address
}

#[tokio::test]
async fn the_node_of_the_drill_head_is_reached_through_the_server_with_the_machines_address_and_token(
) {
    let s = server("head");
    let token = admin(&s).await;
    let node = fake_head("headtoken12");

    // a machine without a node of the head says so
    demo_machine(&s, &token, "plain").await;
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/machines/plain/head/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::NOT_FOUND, json!("no_head"))
    );

    // a machine whose configuration names the node
    let cfg = format!("head_node = server\nhead_address = {node}\nhead_token = headtoken12\n");
    let (status, body) = call(&s, Method::POST, "/api/v1/machines", Some(&token), Some(json!({"id": "bench", "name": "Bench", "model": "ccd-mtc", "connection": {"kind": "demo"}, "cfg": cfg}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/machines/bench/head/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["node"].clone(), body["camera_model"].clone()),
        (StatusCode::OK, json!("head-1"), json!("ov5640"))
    );

    // the measure goes with the token of the node (the one of the machine), and the samples are checked here
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bench/head/measure?samples=5",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["distance_mm"].clone(), body["asked"].clone()),
        (StatusCode::OK, json!(42.5), json!(5))
    );
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bench/head/measure?samples=99",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_samples"))
    );

    // the camera: the picture comes through with a ticket in the address (a browser cannot set a header on it), never with the token
    let (status, made) = call(&s, Method::POST, "/api/v1/tickets", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let ticket = made["ticket"].as_str().unwrap().to_string();
    let (status, _) = raw_get(
        s.app.clone(),
        &format!("/api/v1/machines/bench/head/stream?token={token}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the token in the address is off"
    );
    let stream = format!("/api/v1/machines/bench/head/stream?ticket={ticket}");
    let (status, text) = raw_get(s.app.clone(), &stream).await;
    assert_eq!(status, StatusCode::OK);
    let (again, _) = raw_get(s.app.clone(), &stream).await;
    assert_eq!(again, StatusCode::UNAUTHORIZED, "a ticket is good once");
    assert!(
        text.contains("PICTURE-ONE") && text.contains("PICTURE-TWO"),
        "{text}"
    );
    let (status, _) = raw_get(s.app.clone(), "/api/v1/machines/bench/head/stream").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // a viewer sees the status but may not measure
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/users",
        Some(&token),
        Some(json!({"name": "luis", "role": "viewer", "password": "otra-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, login) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "luis", "password": "otra-clave-larga"})),
    )
    .await;
    let viewer = login["token"].as_str().unwrap().to_string();
    let (status, _) = call(
        &s,
        Method::GET,
        "/api/v1/machines/bench/head/status",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bench/head/measure",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // a node that is not there is a bad gateway, not a hang
    let (status, body) = call(&s, Method::POST, "/api/v1/machines", Some(&token), Some(json!({"id": "gone", "name": "Gone", "model": "ccd-mtc", "connection": {"kind": "demo"}, "cfg": "head_node = server\nhead_address = 127.0.0.1:9\n"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/machines/gone/head/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_GATEWAY, json!("head_unreachable"))
    );
}

/// A board seen from above as the camera of the head sends it: grey board, dark holes of 0.8 mm at 50 pixels to the millimetre, as a JPEG.
fn board_jpeg(holes: &[(f64, f64)]) -> Vec<u8> {
    let (w, h) = (320usize, 240usize);
    let mut rgb = vec![170u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for &(mx, my) in holes {
                let (cx, cy) = (w as f64 / 2.0 + mx * 50.0, h as f64 / 2.0 - my * 50.0);
                if (x as f64 - cx).hypot(y as f64 - cy) <= 0.4 * 50.0 {
                    let i = (y * w + x) * 3;
                    rgb[i..i + 3].copy_from_slice(&[25, 25, 25]);
                }
            }
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 95)
        .encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

/// A node with the ring of LEDs and a camera that sends one picture; it keeps the first line of every request it gets.
fn fake_camera_head(
    token: &'static str,
    picture: Vec<u8>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 2048];
            let n = c.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let line = req.lines().next().unwrap_or("").to_string();
            log.lock().unwrap().push(line.clone());
            let authorised = req.contains(&format!("Authorization: Bearer {token}\r\n"));
            let json = |b: &str| {
                format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{b}").into_bytes()
            };
            let reply: Vec<u8> = if !authorised {
                b"HTTP/1.0 401 Unauthorized\r\nContent-Type: application/json\r\n\r\n{\"error\":\"unauthorized\"}".to_vec()
            } else if line.starts_with("GET /api/snapshot ") {
                let mut r = b"HTTP/1.0 200 OK\r\nContent-Type: image/jpeg\r\n\r\n".to_vec();
                r.extend_from_slice(&picture);
                r
            } else if line.starts_with("GET /api/light") || line.starts_with("POST /api/light") {
                json(
                    r#"{"light":true,"on":false,"level":60,"max_level":80,"tone":"white","sectors":255,"leds":8,"auto_off_s":-1,"color":"ffffff","cue":"none"}"#,
                )
            } else {
                b"HTTP/1.0 404 Not Found\r\n\r\n".to_vec()
            };
            let _ = c.write_all(&reply);
        }
    });
    (address, seen)
}

#[tokio::test]
async fn the_work_is_checked_through_the_camera_of_the_head_and_the_ring_says_how_it_went() {
    let s = server("inspect");
    let token = admin(&s).await;
    let (node, seen) = fake_camera_head("headtoken12", board_jpeg(&[(0.0, 0.0)]));
    let cfg = format!("head_node = server\nhead_address = {node}\nhead_token = headtoken12\n");
    let (status, body) = call(&s, Method::POST, "/api/v1/machines", Some(&token), Some(json!({"id": "cam", "name": "Cam", "model": "ccd-mtc", "connection": {"kind": "demo"}, "cfg": cfg}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // the ring and the picture go through with the node's own words
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/machines/cam/head/light",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["max_level"].clone()),
        (StatusCode::OK, json!(80))
    );
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/light?on=1&level=50&tone=warm",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("POST /api/light?on=1&level=50&tone=warm ")));
    // what the node does not use never reaches it
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/light?on=1%0d%0aX:y",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_light"))
    );
    let (status, _) = raw_get(
        s.app.clone(),
        &format!("/api/v1/machines/cam/head/snapshot?token={token}"),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::UNAUTHORIZED,
        "{status}"
    );

    // nothing was checked yet
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/machines/cam/head/inspect",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::NOT_FOUND, json!("no_inspection"))
    );

    // the profile: the scale of the picture, a preset that keeps it, and what cannot be used is listed
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/cam/head/inspect/profile",
        Some(&token),
        Some(json!({"px_per_mm": 50.0, "check_milling": false, "hole_pos_tol_mm": -3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["px_per_mm"].clone(), body["rejected"].clone()),
        (json!(50.0), json!(["hole_pos_tol_mm"]))
    );
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/cam/head/inspect/profile",
        Some(&token),
        Some(json!({"preset": "prototype"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["name"].clone(), body["px_per_mm"].clone()),
        (json!("prototype"), json!(50.0))
    );
    let (_, body) = call(
        &s,
        Method::GET,
        "/api/v1/machines/cam/head/inspect/profile",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(body["name"], json!("prototype"));
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/cam/head/inspect/profile",
        Some(&token),
        Some(json!({"preset": "nonsense"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // a good board: the hole is there, the verdict is ok and the ring shows green
    seen.lock().unwrap().clear();
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/inspect",
        Some(&token),
        Some(json!({"holes": [{"x_mm": 0.0, "y_mm": 0.0, "dia_mm": 0.8}], "light": 60})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["verdict"], json!("ok"), "{body}");
    assert_eq!(body["holes"][0]["ok"], json!(true));
    {
        let lines = seen.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("POST /api/light?on=1&level=60&tone=white&sectors=all ")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l.starts_with("GET /api/snapshot ")));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("POST /api/light?cue=ok ")),
            "{lines:?}"
        );
    }
    // and it is kept as the last check
    let (status, last) = call(
        &s,
        Method::GET,
        "/api/v1/machines/cam/head/inspect",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, last["verdict"].clone()),
        (StatusCode::OK, json!("ok"))
    );

    // the scale comes from the hole that was measured: the picture is at 50 pixels to the millimetre, a hole of 0.8 mm covers 40 of them
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/inspect/calibrate",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let scale = body["px_per_mm"].as_f64().unwrap();
    assert!((scale - 50.0).abs() < 2.0, "{scale}");
    // the last check was made with the old scale: it is gone, and there is nothing to calibrate from until another one is made
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/inspect/calibrate",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::NOT_FOUND, json!("no_inspection"))
    );
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/cam/head/inspect/profile",
        Some(&token),
        Some(json!({"px_per_mm": 50.0})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/inspect",
        Some(&token),
        Some(json!({"holes": [{"x_mm": 0.0, "y_mm": 0.0, "dia_mm": 0.8}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // a hole that is not there: not ok, which one, and the ring shows red
    seen.lock().unwrap().clear();
    let (status, body) = call(&s, Method::POST, "/api/v1/machines/cam/head/inspect", Some(&token), Some(json!({"holes": [{"x_mm": 0.0, "y_mm": 0.0, "dia_mm": 0.8}, {"x_mm": 1.5, "y_mm": 0.5, "dia_mm": 0.8}]}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["verdict"], json!("not_ok"));
    assert!(body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["code"] == "hole_missing" && f["hole"] == 1));
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("POST /api/light?cue=nok ")));
    // no light was asked for, so the ring was not switched on for the picture
    assert!(!seen
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("tone=white")));

    // too many holes, and a viewer may look but not check
    let many: Vec<Value> = (0..501)
        .map(|_| json!({"x_mm": 0.0, "y_mm": 0.0, "dia_mm": 0.8}))
        .collect();
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/inspect",
        Some(&token),
        Some(json!({"holes": many})),
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("too_many_holes"))
    );
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/users",
        Some(&token),
        Some(json!({"name": "ana", "role": "viewer", "password": "otra-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, login) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "ana", "password": "otra-clave-larga"})),
    )
    .await;
    let viewer = login["token"].as_str().unwrap().to_string();
    let (status, _) = call(
        &s,
        Method::GET,
        "/api/v1/machines/cam/head/inspect",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/inspect",
        Some(&viewer),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/cam/head/light?on=1",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---- the spindle of the machine ---------------------------------------------------------------------------------------------------------

fn route_job(rpm: u32, feed_xy: u32, feed_z: u32) -> Value {
    json!({
        "kind": "route", "name": "outline",
        "tools": [{"number": 1, "depth_mm": 1.7, "iterations": 1, "feed_xy_mm_min": feed_xy, "feed_z_mm_min": feed_z, "spindle_rpm": rpm}],
        "paths": [{"pen": 1, "points_mm": [[10, 10], [40, 10], [40, 30]]}]
    })
}

fn drill_job_at(rpm: u32, feed: u32) -> Value {
    json!({
        "kind": "drill", "name": "board",
        "tools": [{"number": 1, "feed_mm_min": feed, "spindle_rpm": rpm}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}]
    })
}

async fn set_spindle(s: &Server, token: &str, id: &str, body: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::PUT,
        &format!("/api/v1/machines/{id}/spindle"),
        Some(token),
        Some(body),
    )
    .await
}

async fn preview_of(s: &Server, token: &str, id: &str, job: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        &format!("/api/v1/machines/{id}/preview"),
        Some(token),
        Some(job),
    )
    .await
}

#[tokio::test]
async fn a_machine_without_a_spindle_chosen_runs_any_job_as_it_always_did() {
    let s = server("spindle-none");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let (status, view) = call(
        &s,
        Method::GET,
        "/api/v1/machines/m1/spindle",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["spindle"], "");
    assert_eq!(
        view["guard"], "reject",
        "the switch starts on the careful side"
    );
    assert_eq!(view["limits"]["vfd"]["max_rpm"], 24000);
    assert_eq!(view["limits"]["original"]["max_rpm"], 63000);
    assert_eq!(view["limits"]["original"]["min_rpm"], 5000);
    assert!(view["active"].is_null() && view["node_max_rpm"].is_null());
    let (status, view) = preview_of(&s, &token, "m1", drill_job_at(60_000, 9_000)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["spindle_warnings"], json!([]));
}

#[tokio::test]
async fn the_spindle_is_set_by_an_administrator_and_kept_across_a_restart_and_an_update_that_does_not_name_it(
) {
    let s = server("spindle-set");
    let boss = admin(&s).await;
    demo_machine(&s, &boss, "m1").await;
    let (status, view) = set_spindle(
        &s,
        &boss,
        "m1",
        json!({"spindle": "vfd", "vfd": {"max_xy_mm_min": 900}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["spindle"], "vfd");
    assert_eq!(view["node_max_rpm"], 24000);
    assert_eq!(view["active"]["max_xy_mm_min"], 900);
    assert_eq!(
        view["active"]["max_z_mm_min"], 1200,
        "what was not named keeps the starting value"
    );
    // an update of the machine from a client that does not know about spindles leaves it alone
    let (status, shown) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/m1",
        Some(&boss),
        Some(json!({"id": "m1", "name": "Renamed", "connection": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["spindle"], "vfd");
    assert_eq!(shown["spindle_vfd"]["max_xy_mm_min"], 900);
    // and it is on disk
    let again = eh3d_server::machines::Machines::open(&s.dir).unwrap();
    assert_eq!(again.get("m1").unwrap().spindle, "vfd");
    // the limits go back to the starting ones with an empty change
    let (_, view) = set_spindle(&s, &boss, "m1", json!({"vfd": {}})).await;
    assert_eq!(view["active"]["max_xy_mm_min"], 1500);
}

#[tokio::test]
async fn only_an_administrator_changes_the_spindle_and_nonsense_is_refused() {
    let s = server("spindle-roles");
    let boss = admin(&s).await;
    demo_machine(&s, &boss, "m1").await;
    let (_, made) = call(
        &s,
        Method::POST,
        "/api/v1/users",
        Some(&boss),
        Some(json!({"name": "op", "role": "operator", "password": "clave-larga-123"})),
    )
    .await;
    assert!(made.get("error").is_none(), "{made}");
    let (_, login) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "op", "password": "clave-larga-123"})),
    )
    .await;
    let op = login["token"].as_str().unwrap().to_string();
    assert_eq!(
        set_spindle(&s, &op, "m1", json!({"spindle": "vfd"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &s,
            Method::GET,
            "/api/v1/machines/m1/spindle",
            Some(&op),
            None
        )
        .await
        .0,
        StatusCode::OK,
        "an operator may read it"
    );
    for bad in [
        json!({"spindle": "steam"}),
        json!({"guard": "ignore"}),
        json!({"vfd": {"min_rpm": 30000}}),
        json!({"original": {"max_z_mm_min": 0}}),
    ] {
        let (status, body) = set_spindle(&s, &boss, "m1", bad.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad} -> {body}");
        assert_eq!(body["error"], "bad_spindle");
    }
    let (status, _) = set_spindle(&s, &boss, "nothing", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // nothing of the refused changes stuck
    let (_, view) = call(
        &s,
        Method::GET,
        "/api/v1/machines/m1/spindle",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (view["spindle"].as_str(), view["guard"].as_str()),
        (Some(""), Some("reject"))
    );
}

#[tokio::test]
async fn a_job_beyond_the_spindle_is_refused_by_default_with_what_is_over() {
    let s = server("spindle-reject");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    set_spindle(&s, &token, "m1", json!({"spindle": "vfd"})).await;
    // the turns of the original spindle on the one with the drive
    let (status, body) = preview_of(&s, &token, "m1", drill_job_at(60_000, 800)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "spindle_limit");
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("60000 against 24000"));
    // the plunge of a drill over what the spindle holds
    let (status, body) = preview_of(&s, &token, "m1", drill_job_at(24_000, 2_000)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("spindle_limit"))
    );
    // a routing job: the feed along the cut and the plunge are judged apart
    let (status, body) = preview_of(&s, &token, "m1", route_job(24_000, 2_500, 300)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("feed along the cut"));
    let (status, body) = preview_of(&s, &token, "m1", route_job(24_000, 800, 1_500)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("plunge feed"));
    // slower than the spindle can hold
    let (status, _) = preview_of(&s, &token, "m1", drill_job_at(3_000, 800)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // a job that fits goes through, and no job is made out of a refused one
    let (status, view) = preview_of(&s, &token, "m1", drill_job_at(24_000, 1_000)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job_at(60_000, 800)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, jobs) = call(&s, Method::GET, "/api/v1/jobs", Some(&token), None).await;
    assert_eq!(jobs, json!([]));
}

#[tokio::test]
async fn the_original_spindle_takes_the_fast_turns_but_not_a_feed_beyond_its_limit() {
    let s = server("spindle-original");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    set_spindle(&s, &token, "m1", json!({"spindle": "original"})).await;
    let (status, view) = preview_of(&s, &token, "m1", drill_job_at(60_000, 3_500)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (status, body) = preview_of(&s, &token, "m1", route_job(60_000, 3_500, 300)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("3500 against 3000"));
}

#[tokio::test]
async fn with_the_switch_on_warn_the_job_goes_through_cut_to_the_top_turns_and_lists_every_excess()
{
    let s = server("spindle-warn");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let (status, view) =
        set_spindle(&s, &token, "m1", json!({"spindle": "vfd", "guard": "warn"})).await;
    assert_eq!(
        (status, view["guard"].as_str()),
        (StatusCode::OK, Some("warn"))
    );
    let (status, view) = preview_of(&s, &token, "m1", route_job(60_000, 2_500, 1_500)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let codes: Vec<&str> = view["spindle_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        ["rpm_above_max", "feed_xy_above_max", "feed_z_above_max"]
    );
    assert_eq!(view["spindle_warnings"][0]["limit"], 24000);
    assert_eq!(
        view["tools"][0]["spindle_rpm"], 24000,
        "the node is never asked for more than the top of the spindle"
    );
    // the job is kept with its warnings, and a job without excess has none
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job_at(60_000, 800)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["view"]["spindle_warnings"][0]["code"], "rpm_above_max");
    let (_, clean) = preview_of(&s, &token, "m1", drill_job_at(24_000, 800)).await;
    assert_eq!(clean["spindle_warnings"], json!([]));
}

#[tokio::test]
async fn a_ready_job_is_judged_again_when_it_starts_with_the_spindle_the_machine_carries_then() {
    let s = server("spindle-start");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    // made while the machine carries the fast spindle
    set_spindle(&s, &token, "m1", json!({"spindle": "original"})).await;
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job_at(60_000, 800)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    // the spindle is changed for the one with the drive: the job no longer fits and does not start
    set_spindle(&s, &token, "m1", json!({"spindle": "vfd"})).await;
    let (status, body) = call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{id}/start"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "spindle_limit");
    let (_, again) = call(
        &s,
        Method::GET,
        &format!("/api/v1/jobs/{id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(again["state"], "ready", "nothing ran");
}

// ---- free G-code ----------------------------------------------------------------------------------------------------------------------

async fn post(s: &Server, token: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call(s, Method::POST, path, Some(token), body).await
}

async fn get(s: &Server, token: &str, path: &str) -> (StatusCode, Value) {
    call(s, Method::GET, path, Some(token), None).await
}

/// Waits until the job is in one of the states (a failure is one of them only when asked for).
async fn wait_state(s: &Server, token: &str, job: &str, wanted: &[&str]) -> Value {
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, body) = get(s, token, &format!("/api/v1/jobs/{job}")).await;
        let state = body["state"].as_str().unwrap_or("").to_string();
        if wanted.contains(&state.as_str()) {
            return body;
        }
        assert!(Instant::now() < limit, "timeout in state {state}: {body}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn gcode_job(text: &str) -> Value {
    json!({"kind": "gcode", "name": "frame", "gcode": text})
}

#[tokio::test]
async fn a_gcode_job_is_planned_runs_on_the_simulator_and_reports_its_progress() {
    let s = server("gcode-run");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let text = "(a frame)\nG21 G90\nG0 X10 Y10\nG1 X40 F300\nG1 Y30\nG0 X0 Y0\nM30\n";
    let (status, view) = post(
        &s,
        &token,
        "/api/v1/machines/m1/preview",
        Some(gcode_job(text)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["kind"], "gcode");
    assert_eq!(
        view["items"], 6,
        "the lines that are sent, without the comment"
    );
    assert_eq!(view["bounds_mm"], json!([0.0, 0.0, 40.0, 30.0]));
    assert!(view["estimate_s"].as_f64().unwrap() > 1.0);
    assert_eq!(view["holes"], json!([]));

    let (status, job) = post(
        &s,
        &token,
        "/api/v1/machines/m1/jobs",
        Some(gcode_job(text)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(
        (job["state"].as_str(), job["kind"].as_str()),
        (Some("ready"), Some("gcode"))
    );
    assert_eq!(
        (job["holes_total"].as_u64(), job["line"].as_u64()),
        (Some(0), Some(0))
    );
    let id = job["id"].as_str().unwrap().to_string();
    let (status, _) = post(&s, &token, &format!("/api/v1/jobs/{id}/start"), None).await;
    assert_eq!(status, StatusCode::OK);
    let done = wait_state(&s, &token, &id, &["done", "failed"]).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        (done["items_done"].as_u64(), done["items_total"].as_u64()),
        (Some(6), Some(6))
    );
    assert_eq!(done["line"], 7, "the end of the text");
    // the fleet counts it like any other job
    let (_, fleet) = get(&s, &token, "/api/v1/fleet").await;
    assert_eq!(fleet["machines"][0]["state"], "idle");
    // a job that is not G-code cannot be held, and a job that does not run cannot be held either
    let (_, drill) = post(&s, &token, "/api/v1/machines/m1/jobs", Some(drill_job())).await;
    let did = drill["id"].as_str().unwrap();
    let (status, body) = post(&s, &token, &format!("/api/v1/jobs/{did}/pause"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_pausable"))
    );
    let (status, body) = post(&s, &token, &format!("/api/v1/jobs/{id}/pause"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_in_state"))
    );
    let (status, _) = post(&s, &token, "/api/v1/jobs/jnothing/resume", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn what_is_not_gcode_and_the_wrong_kind_of_machine_are_refused_with_a_code() {
    let s = server("gcode-refuse");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    for (text, code) in [
        ("G0 X1\n$H\n", "bad_gcode"),
        ("$100=250\n", "bad_gcode"),
        ("(only a comment)\n\n", "no_work"),
    ] {
        let (status, body) = post(
            &s,
            &token,
            "/api/v1/machines/m1/preview",
            Some(gcode_job(text)),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(code)),
            "{text}"
        );
    }
    let (status, body) = post(
        &s,
        &token,
        "/api/v1/machines/m1/preview",
        Some(json!({"kind": "gcode", "gcode": "G0 X1", "skip": 9})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("no_work"))
    );

    // a machine with a node takes the Bungard jobs only; a GRBL machine takes G-code only
    let (status, body) = post(&s, &token, "/api/v1/machines", Some(json!({"id": "node1", "name": "Node", "connection": {"kind": "node", "address": "127.0.0.1:1", "token": "t"}}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(
        &s,
        &token,
        "/api/v1/machines/node1/preview",
        Some(gcode_job("G0 X1")),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("wrong_machine"))
    );
    let (status, body) = post(&s, &token, "/api/v1/machines", Some(json!({"id": "router", "name": "Router", "connection": {"kind": "grbl", "port": "/dev/ttyUSB0"}}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["connection"]["kind"], "grbl");
    assert_eq!(
        (
            body["connection"]["port"].as_str(),
            body["connection"]["baud"].as_u64()
        ),
        (Some("/dev/ttyUSB0"), Some(115_200))
    );
    let (status, body) = post(
        &s,
        &token,
        "/api/v1/machines/router/preview",
        Some(drill_job()),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("wrong_machine"))
    );
    let (status, view) = post(
        &s,
        &token,
        "/api/v1/machines/router/preview",
        Some(gcode_job("G0 X1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    // the port and the speed are checked
    for bad in [
        json!({"kind": "grbl", "port": ""}),
        json!({"kind": "grbl", "port": "COM3", "baud": 5}),
    ] {
        let (status, body) = post(
            &s,
            &token,
            "/api/v1/machines",
            Some(json!({"id": "bad", "name": "Bad", "connection": bad})),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_address"))
        );
    }
    let (_, fleet) = get(&s, &token, "/api/v1/fleet").await;
    let kinds: Vec<&str> = fleet["machines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["connection"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["demo", "node", "grbl"]);
}

#[tokio::test]
async fn a_gcode_job_that_fails_says_where_and_one_that_goes_on_waits_for_the_operator() {
    let s = server("gcode-resume");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    // the third line is refused by the controller
    let text = "G21 G90\nG0 X1\nG2 X5 I1\nG0 X9\n";
    let (_, job) = post(
        &s,
        &token,
        "/api/v1/machines/m1/jobs",
        Some(gcode_job(text)),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_string();
    post(&s, &token, &format!("/api/v1/jobs/{id}/start"), None).await;
    let failed = wait_state(&s, &token, &id, &["failed", "done"]).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(
        failed["message"].as_str().unwrap().contains("line 3"),
        "{failed}"
    );
    assert_eq!(failed["items_done"], 2);
    let line = failed["line"].as_u64().unwrap();
    assert_eq!(
        line, 2,
        "the line that failed is where a job that goes on starts"
    );

    // the same program from there: the modal codes of the done lines come first and the job waits for a person before it moves
    let fixed = "G21 G90\nG0 X1\nG0 X5\nG0 X9\n";
    let (status, job) = post(
        &s,
        &token,
        "/api/v1/machines/m1/jobs",
        Some(json!({"kind": "gcode", "gcode": fixed, "skip": line})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(
        (
            job["view"]["items"].as_u64(),
            job["view"]["skipped"].as_u64()
        ),
        (Some(2), Some(2))
    );
    let id = job["id"].as_str().unwrap().to_string();
    post(&s, &token, &format!("/api/v1/jobs/{id}/start"), None).await;
    let held = wait_state(&s, &token, &id, &["waiting_operator", "done", "failed"]).await;
    assert_eq!(held["state"], "waiting_operator", "{held}");
    assert_eq!(held["pause"]["kind"], "resume");
    assert_eq!(held["items_done"], 0, "nothing of the program went out");
    let (_, fleet) = get(&s, &token, "/api/v1/fleet").await;
    assert_eq!(fleet["machines"][0]["state"], "waiting");
    let (status, _) = post(&s, &token, &format!("/api/v1/jobs/{id}/resume"), None).await;
    assert_eq!(status, StatusCode::OK);
    let done = wait_state(&s, &token, &id, &["done", "failed"]).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        (done["items_done"].as_u64(), done["line"].as_u64()),
        (Some(2), Some(4))
    );

    // a held job can also be cancelled
    let (_, job) = post(
        &s,
        &token,
        "/api/v1/machines/m1/jobs",
        Some(json!({"kind": "gcode", "gcode": fixed, "skip": 1})),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_string();
    post(&s, &token, &format!("/api/v1/jobs/{id}/start"), None).await;
    wait_state(&s, &token, &id, &["waiting_operator"]).await;
    let (status, _) = post(&s, &token, &format!("/api/v1/jobs/{id}/cancel"), None).await;
    assert_eq!(status, StatusCode::OK);
    wait_state(&s, &token, &id, &["cancelled"]).await;
}

#[tokio::test]
async fn the_spindle_switch_judges_the_turns_and_the_feed_of_a_gcode_job_too() {
    let s = server("gcode-spindle");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let text = "G21 G90\nM3 S30000\nG1 X10 F2000\nM5\n";
    // no spindle chosen: it goes as it always did
    let (status, view) = post(
        &s,
        &token,
        "/api/v1/machines/m1/preview",
        Some(gcode_job(text)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    set_spindle(&s, &token, "m1", json!({"spindle": "vfd"})).await;
    let (status, body) = post(
        &s,
        &token,
        "/api/v1/machines/m1/preview",
        Some(gcode_job(text)),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("spindle_limit"))
    );
    set_spindle(&s, &token, "m1", json!({"guard": "warn"})).await;
    let (status, view) = post(
        &s,
        &token,
        "/api/v1/machines/m1/preview",
        Some(gcode_job(text)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let codes: Vec<&str> = view["spindle_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["rpm_above_max", "feed_xy_above_max"]);
    assert_eq!(view["spindle_warnings"][0]["limit"], 24000);
}

// ---- going on from a stopped job, and the `$` lines of GRBL ---------------------------------------------------------------------------

/// Makes a user of the role and returns the token of their login.
async fn user_with_role(s: &Server, boss: &str, name: &str, role: &str) -> String {
    let (status, _) = post(
        s,
        boss,
        "/api/v1/users",
        Some(json!({"name": name, "role": role, "password": "clave-larga-123"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    call(
        s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": name, "password": "clave-larga-123"})),
    )
    .await
    .1["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Puts a job in the book as stopped where the test says (a drilling job: the holes done; a routing job: the items run; a G-code job: the line).
fn stop_job(
    s: &Server,
    id: &str,
    state: eh3d_server::jobs::JobState,
    holes: usize,
    items: usize,
    line: usize,
) {
    let mut jobs = s.state.jobs.lock().unwrap();
    let job = jobs.get_mut(id).unwrap();
    job.state = state;
    job.holes_done = holes;
    job.items_done = items;
    job.line = line;
    jobs.changed();
}

#[tokio::test]
async fn a_stopped_drilling_job_goes_on_from_the_saved_request_and_the_holes_done_add_up() {
    use eh3d_server::jobs::JobState;
    let s = server("resume-drill");
    let boss = admin(&s).await;
    let operator = user_with_role(&s, &boss, "op1", "operator").await;
    let viewer = user_with_role(&s, &boss, "view1", "viewer").await;
    demo_machine(&s, &boss, "m1").await;
    let mut asked = drill_job();
    asked["skip_tools"] = json!([2]);
    let (_, job) = post(&s, &operator, "/api/v1/machines/m1/jobs", Some(asked)).await;
    let first = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["holes_total"], 2);

    // a job that is not stopped cannot be gone on from
    let path = format!("/api/v1/jobs/{first}/resume-from");
    let (status, body) = post(&s, &operator, &path, None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_stopped"))
    );

    // it stops with one hole done: the new job has the same file and tools, leaves that hole out and waits to be started
    stop_job(&s, &first, JobState::Failed, 1, 0, 0);
    let (status, saved) = get(&s, &operator, &format!("/api/v1/jobs/{first}/request")).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["request"]["holes"].as_array().unwrap().len(), 3);
    assert_eq!(saved["next_skip"].as_u64(), Some(1));
    let (status, second) = post(&s, &operator, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["state"], "ready");
    assert_eq!(second["resumed_from"], first.as_str());
    assert_eq!(second["view"]["skipped"].as_u64(), Some(2));
    assert_eq!(second["holes_total"].as_u64(), Some(1));
    let second_id = second["id"].as_str().unwrap().to_string();
    let (_, kept) = get(&s, &operator, &format!("/api/v1/jobs/{second_id}/request")).await;
    assert_eq!(kept["request"]["skip"].as_u64(), Some(1));
    assert_eq!(kept["request"]["skip_tools"], json!([2]));
    // nothing started by itself
    let (_, listed) = get(&s, &operator, "/api/v1/jobs?state=ready").await;
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // it stops again, one hole later: the sum is carried on; with nothing left the answer is no_work
    stop_job(&s, &second_id, JobState::Cancelled, 1, 0, 0);
    let path2 = format!("/api/v1/jobs/{second_id}/resume-from");
    let (status, body) = post(&s, &operator, &path2, None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("no_work"))
    );
    // the person may name where to go on from instead
    let (status, third) = post(
        &s,
        &operator,
        &path2,
        Some(json!({"skip": 0, "name": "again"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{third}");
    assert_eq!(third["name"].as_str(), Some("again"));
    assert_eq!(third["view"]["skipped"].as_u64(), Some(1));

    // a viewer reads neither the request nor goes on; a missing job is unknown; the request goes with the job
    let request_path = format!("/api/v1/jobs/{first}/request");
    assert_eq!(
        get(&s, &viewer, &request_path).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&s, &viewer, &path, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&s, &operator, "/api/v1/jobs/jnone/resume-from", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/jobs/{first}"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        get(&s, &operator, &request_path).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_stopped_routing_job_goes_on_after_the_paths_it_finished_and_repeats_the_one_it_cut_short(
) {
    use eh3d_server::jobs::JobState;
    let s = server("resume-route");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let route = json!({"kind": "route", "name": "outline",
        "tools": [{"number": 1, "depth_mm": 1.7, "iterations": 1, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000}],
        "paths": [{"pen": 1, "points_mm": [[10, 10], [40, 10], [40, 30]]}, {"pen": 1, "points_mm": [[12, 40], [20, 40]]}, {"pen": 1, "points_mm": [[50, 12], [60, 12]]}]});
    let (status, job) = post(&s, &token, "/api/v1/machines/m1/jobs", Some(route)).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let total = job["items_total"].as_u64().unwrap() as usize;
    let path = format!("/api/v1/jobs/{id}/resume-from");
    // stopped before anything ran, or in the middle of the first path: it starts from the first path again
    for items in [0, 3] {
        stop_job(&s, &id, JobState::Failed, 0, items, 0);
        let (status, again) = post(&s, &token, &path, None).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again["view"]["skipped"], 0, "items {items}");
    }
    // stopped almost at the end: the first two paths are done and the last one, cut short, is cut again
    stop_job(&s, &id, JobState::Cancelled, 0, total - 1, 0);
    let (_, saved) = get(&s, &token, &format!("/api/v1/jobs/{id}/request")).await;
    assert_eq!(saved["next_skip"].as_u64(), Some(2), "{saved}");
    let (status, again) = post(&s, &token, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["view"]["skipped"].as_u64(), Some(2));
    assert_eq!(again["holes_total"].as_u64(), Some(1));
}

#[tokio::test]
async fn a_stopped_gcode_job_goes_on_from_its_line_and_waits_for_the_operator_before_it_moves() {
    let s = server("resume-gcode");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    // twenty lines before the one that fails: the margin of the server (16) goes back from the line reached
    let owned = format!("G21 G90\n{}G2 X5 I1\nG0 X9\n", "G0 X1\n".repeat(20));
    let text = owned.as_str();
    let (_, job) = post(
        &s,
        &token,
        "/api/v1/machines/m1/jobs",
        Some(gcode_job(text)),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_string();
    post(&s, &token, &format!("/api/v1/jobs/{id}/start"), None).await;
    let failed = wait_state(&s, &token, &id, &["failed", "done"]).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["line"].as_u64(), Some(21));
    let (status, second) = post(
        &s,
        &token,
        &format!("/api/v1/jobs/{id}/resume-from"),
        Some(json!({"restore_outputs": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["view"]["skipped"].as_u64(), Some(5));
    assert_eq!(second["resumed_from"].as_str(), Some(id.as_str()));
    let second_id = second["id"].as_str().unwrap().to_string();
    let (_, saved) = get(&s, &token, &format!("/api/v1/jobs/{second_id}/request")).await;
    assert_eq!(
        saved["request"]["gcode"], text,
        "the program is not asked for again"
    );
    // going on from the line that failed: the job waits for a person before the first move
    post(&s, &token, &format!("/api/v1/jobs/{second_id}/start"), None).await;
    let held = wait_state(
        &s,
        &token,
        &second_id,
        &["waiting_operator", "failed", "done"],
    )
    .await;
    assert_eq!(held["state"], "waiting_operator", "{held}");
    assert_eq!(held["pause"]["kind"], "resume");
    assert_eq!(held["items_done"], 0);
}

#[tokio::test]
async fn a_stopped_gcode_job_goes_back_by_the_margin_of_the_server() {
    use eh3d_server::jobs::JobState;
    let s = server("resume-margin");
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let (_, job) = post(
        &s,
        &token,
        "/api/v1/machines/m1/jobs",
        Some(gcode_job("G21 G90\nG0 X1\n")),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_string();
    stop_job(&s, &id, JobState::Failed, 0, 0, 40);
    let (_, saved) = get(&s, &token, &format!("/api/v1/jobs/{id}/request")).await;
    assert_eq!(saved["next_skip"].as_u64(), Some(24), "{saved}");
}

#[tokio::test]
async fn the_dollar_lines_of_grbl_are_refused_unless_an_administrator_allows_them_on_that_machine()
{
    let s = server("dollar");
    let boss = admin(&s).await;
    let operator = user_with_role(&s, &boss, "op1", "operator").await;
    demo_machine(&s, &boss, "m1").await;
    let program = "$H\nG21 G90\nG0 X1\n";
    let (_, m) = get(&s, &operator, "/api/v1/machines/m1").await;
    assert_eq!(m["allow_dollar"], false, "off by default");
    let preview = "/api/v1/machines/m1/preview";
    let (status, body) = post(&s, &operator, preview, Some(gcode_job(program))).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_gcode"))
    );

    // only an administrator changes it
    let record = json!({"id": "m1", "name": "Bungard", "connection": {"kind": "demo"}, "allow_dollar": true});
    let machine = "/api/v1/machines/m1";
    let (status, _) = call(
        &s,
        Method::PUT,
        machine,
        Some(&operator),
        Some(record.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, m) = call(&s, Method::PUT, machine, Some(&boss), Some(record)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(m["allow_dollar"], true);
    // an update that does not name the switch leaves it as it was
    let plain = json!({"id": "m1", "name": "Bungard 2", "connection": {"kind": "demo"}});
    let (_, m) = call(&s, Method::PUT, machine, Some(&boss), Some(plain)).await;
    assert_eq!(m["allow_dollar"], true);

    // with it on the machine takes the `$` lines, but never the wipe of the controller or the real-time characters
    let (status, body) = post(&s, &operator, preview, Some(gcode_job(program))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for bad in ["$RST=*\nG0 X1", "G0 X1\n!", "G0 X1\n?"] {
        let (status, body) = post(&s, &operator, preview, Some(gcode_job(bad))).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_gcode")),
            "{bad}"
        );
    }
    let (status, job) = post(
        &s,
        &operator,
        "/api/v1/machines/m1/jobs",
        Some(gcode_job(program)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().unwrap().to_string();

    // switched off again, the job that was ready is judged again when it starts
    let off = json!({"id": "m1", "name": "Bungard", "connection": {"kind": "demo"}, "allow_dollar": false});
    call(&s, Method::PUT, machine, Some(&boss), Some(off)).await;
    let (status, body) = post(&s, &operator, &format!("/api/v1/jobs/{id}/start"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_gcode"))
    );
}

#[tokio::test]
async fn a_viewer_never_sees_the_secrets_of_the_tuning_text_and_a_busy_machine_is_not_edited() {
    let s = server("secrets-busy");
    let token = admin(&s).await;
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/users",
        Some(&token),
        Some(json!({"name": "viewer1", "role": "viewer", "password": "otra-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, login) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "viewer1", "password": "otra-clave-larga"})),
    )
    .await;
    let viewer = login["token"].as_str().unwrap().to_string();
    let (status, body) = call(&s, Method::POST, "/api/v1/machines", Some(&token), Some(json!({"id": "m1", "name": "Bungard", "model": "ccd-mtc", "connection": {"kind": "demo"}, "cfg": "head_token = SECRETHEAD123\n"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, one) = call(&s, Method::GET, "/api/v1/machines/m1", Some(&viewer), None).await;
    let (_, all) = call(&s, Method::GET, "/api/v1/machines", Some(&viewer), None).await;
    assert!(!one.to_string().contains("SECRETHEAD123"));
    assert!(!all.to_string().contains("SECRETHEAD123"));
    // the panel sends back what it was shown: the secret stays
    let (status, _) = call(&s, Method::PUT, "/api/v1/machines/m1", Some(&token), Some(json!({"id": "m1", "name": "Bungard", "model": "ccd-mtc", "connection": {"kind": "demo"}, "cfg": one["cfg"]}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(s
        .state
        .machines
        .lock()
        .unwrap()
        .get("m1")
        .unwrap()
        .cfg
        .contains("SECRETHEAD123"));
    // a node can be made without a token, and a body that does not read says why
    let (status, body) = call(&s, Method::POST, "/api/v1/machines", Some(&token), Some(json!({"id": "n1", "name": "N", "connection": {"kind": "node", "address": "10.0.0.2:5800"}}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines",
        Some(&token),
        Some(json!({"id": 5})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_request"))
    );
    // a machine with a job running is not edited
    let job = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await
    .1["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        call(
            &s,
            Method::POST,
            &format!("/api/v1/jobs/{job}/start"),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_for(&s, &token, &job, &["waiting_operator"], false).await;
    // the fleet tells the line that a job has reached, like the job does
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&token), None).await;
    assert!(fleet.to_string().contains("\"line\":"), "{fleet}");
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/m1",
        Some(&token),
        Some(json!({"id": "m1", "name": "Other", "connection": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("machine_busy"))
    );
    call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{job}/cancel"),
        Some(&token),
        None,
    )
    .await;
    wait_for(&s, &token, &job, &["cancelled"], false).await;
}

#[tokio::test]
async fn with_local_until_setup_only_this_computer_makes_the_first_administrator() {
    use axum::extract::ConnectInfo;
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        local_until_setup: true,
        ..ServerConfig::default()
    })
    .unwrap();
    let app = router(state.clone());
    let from = |ip: &str| {
        let mut r = Request::builder()
            .uri("/api/v1/setup")
            .body(Body::empty())
            .unwrap();
        r.extensions_mut()
            .insert(ConnectInfo(std::net::SocketAddr::new(
                ip.parse().unwrap(),
                5000,
            )));
        r
    };
    let far = app.clone().oneshot(from("192.168.0.77")).await.unwrap();
    assert_eq!(far.status(), StatusCode::FORBIDDEN);
    let near = app.clone().oneshot(from("127.0.0.1")).await.unwrap();
    assert_eq!(near.status(), StatusCode::OK);
    // once there is a user, whoever reaches the server is answered
    let code = state.auth.lock().unwrap().setup_code().unwrap().to_string();
    state
        .auth
        .lock()
        .unwrap()
        .setup(&code, "juan", "una-clave-larga")
        .unwrap();
    let after = app.oneshot(from("192.168.0.77")).await.unwrap();
    assert_eq!(after.status(), StatusCode::OK);
    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_stop_of_a_machine_ends_the_job_that_runs_on_it_whoever_asks_and_a_viewer_cannot() {
    let s = server("stop-machine");
    let boss = admin(&s).await;
    let operator = user_with_role(&s, &boss, "op1", "operator").await;
    let viewer = user_with_role(&s, &boss, "view1", "viewer").await;
    demo_machine(&s, &boss, "m1").await;
    // nothing runs: the answer says so
    let (status, body) = post(&s, &operator, "/api/v1/machines/m1/stop", None).await;
    assert_eq!(
        (status, body["stopped"].clone()),
        (StatusCode::OK, json!(false)),
        "{body}"
    );
    assert_eq!(
        post(&s, &viewer, "/api/v1/machines/m1/stop", None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&s, &operator, "/api/v1/machines/nope/stop", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // a job waits for the operator: the stop ends it without naming it
    let (_, job) = post(&s, &operator, "/api/v1/machines/m1/jobs", Some(drill_job())).await;
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(
        post(&s, &operator, &format!("/api/v1/jobs/{id}/start"), None)
            .await
            .0,
        StatusCode::OK
    );
    wait_for(&s, &operator, &id, &["waiting_operator"], false).await;
    let (status, body) = post(&s, &operator, "/api/v1/machines/m1/stop", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stopped"], true);
    assert_eq!(body["job"].as_str(), Some(id.as_str()));
    wait_for(&s, &operator, &id, &["cancelled"], false).await;
}

#[tokio::test]
async fn the_health_check_says_little_to_everybody_and_more_to_who_is_logged_in() {
    let s = server("health");
    let (status, open) = call(&s, Method::GET, "/api/v1/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(open["status"], "ok");
    assert_eq!(open["api_revision"], eh3d_server::api::API_REVISION);
    for key in ["uptime_s", "storage", "features", "recovered_locks"] {
        assert!(open.get(key).is_none(), "{key} is for who is logged in");
    }
    // a token that is not valid is the same as none
    let (_, wrong) = call(&s, Method::GET, "/api/v1/health", Some("nope"), None).await;
    assert!(wrong.get("features").is_none());

    let token = admin(&s).await;
    let (_, full) = call(&s, Method::GET, "/api/v1/health", Some(&token), None).await;
    assert_eq!(full["status"], "ok");
    assert_eq!(full["storage"]["writable"], true);
    assert_eq!(full["features"]["studio"], false);
    assert_eq!(full["features"]["updates"], false);
    assert_eq!(full["features"]["url_token"], false);
    assert_eq!(full["features"]["serial"], cfg!(feature = "serial"));
    assert!(full["uptime_s"].is_u64());
}

#[tokio::test]
async fn a_data_folder_that_takes_no_file_makes_the_health_check_degraded() {
    let s = server("health-degraded");
    // the disk went away under the server
    std::fs::remove_dir_all(&s.dir).unwrap();
    let (status, body) = call(&s, Method::GET, "/api/v1/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "degraded");
}

/// The text of a file of the data folder once `wanted` is in it (the lines are written a moment after the state changes).
async fn file_with(path: &std::path::Path, wanted: &str) -> String {
    let limit = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(wanted) || Instant::now() > limit {
            return text;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn the_audit_and_the_log_of_the_jobs_tell_who_did_what_and_never_a_secret() {
    let s = server("audit");
    let token = admin(&s).await;
    // a wrong password is told, with the code and without the password
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "mala-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    demo_machine(&s, &token, "m1").await;
    let (_, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["started_by"], "");
    call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{id}/start"),
        Some(&token),
        None,
    )
    .await;
    wait_for(&s, &token, &id, &["waiting_operator"], false).await;
    let (status, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{id}/cancel"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let done = wait_for(&s, &token, &id, &["cancelled"], false).await;
    assert_eq!(
        (done["started_by"].as_str(), done["stopped_by"].as_str()),
        (Some("juan"), Some("juan")),
        "the detail of the job says who started it and who stopped it"
    );

    let audit = file_with(&s.dir.join("audit.jsonl"), "job_end").await;
    let events: Vec<Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
    for wanted in [
        "setup",
        "login",
        "login_failed",
        "machine_add",
        "job_create",
        "job_start",
        "job_cancel",
        "job_end",
    ] {
        assert!(
            names.contains(&wanted),
            "{wanted} is in the audit: {names:?}"
        );
    }
    let failed = events
        .iter()
        .find(|e| e["event"] == "login_failed")
        .unwrap();
    assert_eq!(
        (failed["who"].as_str(), failed["code"].as_str()),
        (Some("juan"), Some("bad_credentials"))
    );
    let started = events.iter().find(|e| e["event"] == "job_start").unwrap();
    assert_eq!(
        (
            started["who"].as_str(),
            started["role"].as_str(),
            started["job"].as_str()
        ),
        (Some("juan"), Some("admin"), Some(id.as_str()))
    );
    let ended = events.iter().find(|e| e["event"] == "job_end").unwrap();
    assert_eq!(
        (ended["result"].as_str(), ended["stopped_by"].as_str()),
        (Some("cancelled"), Some("juan"))
    );
    for secret in [&token[..], "una-clave-larga", "mala-clave-larga"] {
        assert!(!audit.contains(secret), "no secret in the audit");
    }

    // the log of the jobs has the start and the end of the job
    let log = file_with(&s.dir.join("eh3d-jobs.log"), " end job=").await;
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].contains(" start job=") && lines[0].contains(&format!("job={id}")));
    assert!(lines[0].contains("machine=m1") && lines[0].contains("connection=demo"));
    assert!(lines[0].contains("holes=3") && lines[0].contains("by=juan"));
    assert!(lines[0].contains("request_hash=") && lines[0].contains("estimate_s="));
    assert!(lines[1].contains(" end job=") && lines[1].contains("result=cancelled"));
    assert!(lines[1].contains("stopped_by=juan") && lines[1].contains("duration_s="));
    assert!(lines[0].starts_with("20") && lines[0].split(' ').next().unwrap().ends_with('Z'));
    assert!(!log.contains(&token[..]));
}

#[tokio::test]
async fn the_audit_and_the_log_of_the_jobs_can_be_turned_off() {
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-quiet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        audit_file: false,
        job_log: false,
        ..ServerConfig::default()
    })
    .unwrap();
    let s = Server {
        app: router(state.clone()),
        state,
        dir: dir.clone(),
    };
    let token = admin(&s).await;
    demo_machine(&s, &token, "m1").await;
    let (_, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/m1/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_string();
    call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{id}/start"),
        Some(&token),
        None,
    )
    .await;
    wait_for(&s, &token, &id, &["waiting_operator"], false).await;
    call(
        &s,
        Method::POST,
        &format!("/api/v1/jobs/{id}/cancel"),
        Some(&token),
        None,
    )
    .await;
    wait_for(&s, &token, &id, &["cancelled"], false).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!dir.join("audit.jsonl").exists());
    assert!(!dir.join("eh3d-jobs.log").exists());
}

#[tokio::test]
async fn a_program_token_can_expire_and_the_list_says_when_it_was_last_used() {
    let s = server("token-days");
    let token = admin(&s).await;
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&token),
        Some(json!({"name": "laptop", "role": "operator", "days": 0})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_days"))
    );
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&token),
        Some(json!({"name": "laptop", "role": "operator", "days": 30})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let secret = made["token"].as_str().unwrap().to_string();
    let (_, forever) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&token),
        Some(json!({"name": "fixed", "role": "viewer"})),
    )
    .await;
    assert!(forever["id"].is_string());
    let (_, list) = call(&s, Method::GET, "/api/v1/tokens", Some(&token), None).await;
    let laptop = &list[0];
    let created = laptop["created"].as_u64().unwrap();
    assert_eq!(laptop["expires"].as_u64(), Some(created + 30 * 86_400));
    assert!(laptop["last_used"].is_null());
    assert!(list[1]["expires"].is_null());
    // the program uses it: the list shows it
    let (status, _) = call(&s, Method::GET, "/api/v1/machines", Some(&secret), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(&s, Method::GET, "/api/v1/tokens", Some(&token), None).await;
    assert!(list[0]["last_used"].as_u64().unwrap() >= created);
    assert!(list[1]["last_used"].is_null());
}

#[tokio::test]
async fn the_default_life_of_a_token_is_a_setting() {
    let dir = std::env::temp_dir().join(format!("eh3d-server-api-tdays-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        token_default_days: 7,
        ..ServerConfig::default()
    })
    .unwrap();
    let s = Server {
        app: router(state.clone()),
        state,
        dir,
    };
    let token = admin(&s).await;
    call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&token),
        Some(json!({"name": "a", "role": "viewer"})),
    )
    .await;
    call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&token),
        Some(json!({"name": "b", "role": "viewer", "days": 2})),
    )
    .await;
    let (_, list) = call(&s, Method::GET, "/api/v1/tokens", Some(&token), None).await;
    let created = list[0]["created"].as_u64().unwrap();
    assert_eq!(list[0]["expires"].as_u64(), Some(created + 7 * 86_400));
    assert_eq!(list[1]["expires"].as_u64(), Some(created + 2 * 86_400));
}

#[tokio::test]
async fn a_login_survives_a_restart_of_the_server_and_a_logout_does_not() {
    let s = server_persisting("sessions");
    let token = admin(&s).await;
    let (_, again) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    let second = again["token"].as_str().unwrap().to_string();
    call(&s, Method::POST, "/api/v1/logout", Some(&second), None).await;
    let on_disk = std::fs::read_to_string(s.dir.join("sessions.json")).unwrap();
    assert!(!on_disk.contains(&token) && !on_disk.contains(&second));

    // the server starts again on the same data
    let state = AppState::open(ServerConfig {
        data_dir: s.dir.clone(),
        persist_sessions: true,
        ..ServerConfig::default()
    })
    .unwrap();
    let restarted = Server {
        app: router(state.clone()),
        state,
        dir: s.dir.clone(),
    };
    let (status, who) = call(
        &restarted,
        Method::GET,
        "/api/v1/session",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        (status, who["name"].as_str()),
        (StatusCode::OK, Some("juan"))
    );
    let (status, _) = call(
        &restarted,
        Method::GET,
        "/api/v1/session",
        Some(&second),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // with the keeping off the restart logs everybody out and no file is left
    let state = AppState::open(ServerConfig {
        data_dir: s.dir.clone(),
        persist_sessions: false,
        ..ServerConfig::default()
    })
    .unwrap();
    let off = Server {
        app: router(state.clone()),
        state,
        dir: s.dir.clone(),
    };
    let (status, _) = call(&off, Method::GET, "/api/v1/session", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!s.dir.join("sessions.json").exists());
}

#[tokio::test]
async fn the_health_check_tells_the_switches_of_the_server() {
    let s = server("health-switches");
    let token = admin(&s).await;
    let (_, health) = call(&s, Method::GET, "/api/v1/health", Some(&token), None).await;
    let f = &health["features"];
    assert_eq!(
        f["trust_forwarded_for"], false,
        "the proxy header is not trusted by default"
    );
    assert_eq!(f["exit_on_panic"], true);
    assert_eq!(
        f["persist_sessions"], false,
        "the logins are not kept on the disk by default"
    );
}
