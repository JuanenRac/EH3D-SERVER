//! The chaining of the tool changer on the interface (`request_version: 2` with a `rack`): a request of the first version is planned as it always was, the second
//! chains the changes of the tools and gives the tools their places, what does not fit is refused with `422 bad_rack`, and a field of a later version is left out
//! with a warning. The machine is the one of the sample file of the programs with the automatic tool change, when the sibling repository is next to this one.

use std::path::PathBuf;
use std::sync::Arc;

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
    let dir = std::env::temp_dir().join(format!("eh3d-server-rack-{name}-{}", std::process::id()));
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
    login(s, "juan", "una-clave-larga").await
}

async fn login(s: &Server, name: &str, password: &str) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": name, "password": password})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["token"].as_str().unwrap().to_string()
}

/// A machine with the tool changer of the sample file, or `None` when the sample file is not there.
async fn changer_machine(s: &Server, token: &str) -> Option<()> {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../EH3D-DRILLPRO/testdata/DRILLPRO_CNCA.DCF"
    ))
    .ok()?;
    let dcf = String::from_utf8_lossy(&bytes).into_owned();
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/machines",
        Some(token),
        Some(json!({
            "id": "atc", "name": "Bungard with changer", "model": "ccd-atc", "connection": {"kind": "demo"},
            "dcf": dcf, "cfg": "dry_run = warn\n"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Some(())
}

fn drill(extra: Value) -> Value {
    let mut job = json!({
        "kind": "drill", "name": "board",
        "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}, {"number": 2, "feed_mm_min": 800, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}, {"tool": 2, "x_mm": 30, "y_mm": 20}]
    });
    for (key, value) in extra.as_object().unwrap() {
        job[key] = value.clone();
    }
    job
}

fn route(extra: Value) -> Value {
    let mut job = json!({
        "kind": "route", "name": "outline",
        "tools": [{"number": 1, "depth_mm": 1.7, "iterations": 1, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000}],
        "paths": [{"pen": 1, "points_mm": [[10, 10], [40, 10], [40, 30]]}]
    });
    for (key, value) in extra.as_object().unwrap() {
        job[key] = value.clone();
    }
    job
}

async fn make(s: &Server, token: &str, body: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/machines/atc/jobs",
        Some(token),
        Some(body),
    )
    .await
}

fn codes(job: &Value) -> Vec<String> {
    job["view"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap().to_string())
        // the course of the sample machine is not what these tests are about
        .filter(|c| c != "out_of_course")
        .collect()
}

#[tokio::test]
async fn a_request_of_the_first_version_gets_the_plan_it_always_got() {
    let s = server("v1");
    let token = admin(&s).await;
    if changer_machine(&s, &token).await.is_none() {
        return;
    }
    let (status, plain) = make(&s, &token, drill(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    assert!(plain.get("rack_applied").is_none());
    assert!(plain["view"].get("rack_applied").is_none());
    // the version alone, and a rack where the first version has none, change nothing in the plan
    let (_, versioned) = make(&s, &token, drill(json!({"request_version": 1}))).await;
    let (status, ignored) = make(
        &s,
        &token,
        drill(json!({"request_version": 1, "rack": {"first": 2, "carry_in": true}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ignored}");
    for other in [&versioned, &ignored] {
        assert_eq!(other["items_total"], plain["items_total"]);
        assert_eq!(other["view"]["hole_ends"], plain["view"]["hole_ends"]);
        assert_eq!(other["view"]["estimate_s"], plain["view"]["estimate_s"]);
    }
    assert_eq!(ignored["rack_applied"], false);
    assert_eq!(codes(&ignored), vec!["rack_needs_v2"]);
}

#[tokio::test]
async fn a_second_version_request_chains_the_drilling_and_the_routing() {
    let s = server("chain");
    let token = admin(&s).await;
    if changer_machine(&s, &token).await.is_none() {
        return;
    }
    let (status, alone) = make(&s, &token, drill(json!({"request_version": 2, "rack": {}}))).await;
    assert_eq!(status, StatusCode::OK, "{alone}");
    assert_eq!(alone["rack_applied"], true);
    assert_eq!(alone["view"]["rack_applied"], true);
    let (_, carried_out) = make(
        &s,
        &token,
        drill(json!({"request_version": 2, "rack": {"first": 0, "carry_out": true}})),
    )
    .await;
    assert_eq!(carried_out["rack_applied"], true);
    assert!(
        carried_out["items_total"].as_u64().unwrap() < alone["items_total"].as_u64().unwrap(),
        "the last drill stays in the chuck"
    );
    let (_, apart) = make(
        &s,
        &token,
        route(json!({"request_version": 2, "rack": {"first": 2}})),
    )
    .await;
    let (status, chained) = make(
        &s,
        &token,
        route(json!({"request_version": 2, "rack": {"first": 2, "carry_in": true}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{chained}");
    assert_eq!(chained["rack_applied"], true);
    assert!(codes(&chained).is_empty());
    // one change from the place of the last drill to the place of the cutter, and the spindle is not asked for again: fewer steps than the routing on its own
    assert!(
        chained["items_total"].as_u64().unwrap() < apart["items_total"].as_u64().unwrap(),
        "{} against {}",
        chained["items_total"],
        apart["items_total"]
    );
    // the preview says the same without making a job
    let (status, view) = call(
        &s,
        Method::POST,
        "/api/v1/machines/atc/preview",
        Some(&token),
        Some(route(
            json!({"request_version": 2, "rack": {"first": 2, "carry_in": true}}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["rack_applied"], true);
}

#[tokio::test]
async fn tools_in_the_bar_out_of_order_are_taken_from_their_places() {
    let s = server("slots");
    let token = admin(&s).await;
    if changer_machine(&s, &token).await.is_none() {
        return;
    }
    let (_, ordered) = make(&s, &token, drill(json!({"request_version": 2, "rack": {}}))).await;
    let (status, scattered) = make(
        &s,
        &token,
        drill(json!({
            "request_version": 2,
            "rack": {"slots": [{"tool": 1, "place": 6}, {"tool": 2, "place": 3}]}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{scattered}");
    assert_eq!(scattered["rack_applied"], true);
    assert!(codes(&scattered).is_empty());
    assert_ne!(
        scattered["view"]["estimate_s"], ordered["view"]["estimate_s"],
        "the chuck goes to other places"
    );
}

#[tokio::test]
async fn places_that_repeat_or_do_not_exist_are_refused_with_a_message() {
    let s = server("refused");
    let token = admin(&s).await;
    if changer_machine(&s, &token).await.is_none() {
        return;
    }
    let (status, body) = make(
        &s,
        &token,
        drill(json!({
            "request_version": 2,
            "rack": {"slots": [{"tool": 1, "place": 4}, {"tool": 2, "place": 4}]}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "bad_rack");
    assert_eq!(body["detail"], "rack.slots: place 4 is given to two tools");
    // a place that the machine file does not define
    let (status, body) = make(
        &s,
        &token,
        drill(json!({
            "request_version": 2,
            "rack": {"slots": [{"tool": 1, "place": 19}]}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "bad_rack");
    // nothing was made by the refused requests
    let (_, jobs) = call(&s, Method::GET, "/api/v1/jobs", Some(&token), None).await;
    assert_eq!(jobs.as_array().map(Vec::len), Some(0));
    // a body that is not JSON is the only 400
    let response = s
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/machines/atc/jobs")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from("{\"kind\": \"drill\", "))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_field_of_a_later_version_is_left_out_and_named_in_the_answer() {
    let s = server("later");
    let token = admin(&s).await;
    if changer_machine(&s, &token).await.is_none() {
        return;
    }
    let (status, job) = make(
        &s,
        &token,
        drill(json!({
            "request_version": 3,
            "rack": {"first": 0, "carry_out": true, "spare_place": 9}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["rack_applied"], true);
    assert_eq!(
        codes(&job),
        vec!["request_version_newer", "rack_unknown_field"]
    );
    let unknown = job["view"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "rack_unknown_field")
        .unwrap();
    assert_eq!(unknown["detail"], "spare_place");
}

#[tokio::test]
async fn the_chained_job_asks_for_the_same_permissions_as_any_job() {
    let s = server("roles");
    let boss = admin(&s).await;
    if changer_machine(&s, &boss).await.is_none() {
        return;
    }
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
    let viewer = login(&s, "viewer1", "clave-larga-123").await;
    let operator = login(&s, "operator1", "clave-larga-123").await;
    let chained = drill(json!({"request_version": 2, "rack": {"carry_out": true}}));
    assert_eq!(
        make(&s, &viewer, chained.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(make(&s, &operator, chained.clone()).await.0, StatusCode::OK);
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines/atc/jobs",
        None,
        Some(chained),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
