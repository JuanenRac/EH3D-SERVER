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
