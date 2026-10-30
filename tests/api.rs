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

    // the camera: the picture comes through, with the token in the address because a browser cannot set a header on it
    let (status, text) = raw_get(
        s.app.clone(),
        &format!("/api/v1/machines/bench/head/stream?token={token}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
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
