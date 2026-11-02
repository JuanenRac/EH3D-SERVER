//! The examples of what the server answers (`tests/examples/*.json`). The tests make them from the real router and compare them with the files, so they cannot drift
//! from the server; the clients (EH3D-STUDIO, EH3D-ANDROID-CONTROL and EH3D-IOS-CONTROL) keep a copy of them (`tools/sync_api_examples.py` copies them) and read them
//! in their own tests, so a change of the answers that a client does not follow shows up in its tests.
//!
//! To make them again after a deliberate change of the answers:  EH3D_WRITE_EXAMPLES=1 cargo test --test examples

use std::path::{Path, PathBuf};
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

fn server(name: &str, updates: bool) -> Server {
    let dir = std::env::temp_dir().join(format!("eh3d-server-ex-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = ServerConfig {
        data_dir: dir.join("data"),
        ..ServerConfig::default()
    };
    if updates {
        let meet = dir.join("meet");
        std::fs::create_dir_all(&meet).unwrap();
        std::fs::write(
            meet.join("components.json"),
            r#"[{"id":"server","latest":"9.9.9","url":"https://example.org/server","published":"2000-01-01T00:00:00Z","notes":"- A change\n- Another change"},
                {"id":"studio","latest":"9.9.9","url":"https://example.org/studio","published":"2000-01-01T00:00:00Z","notes":""}]"#,
        )
        .unwrap();
        std::fs::write(
            meet.join("previous.json"),
            r#"{"server":{"available":false,"version":null},"studio":{"available":true,"version":"9.9.8"}}"#,
        )
        .unwrap();
        std::fs::write(
            meet.join("status.json"),
            r#"{"state":"done","message":"updated: studio","log":["studio: installed 9.9.9"],"percent":100,"steps":[{"id":"studio.download","state":"done","percent":100},{"id":"studio.verify","state":"done","percent":100},{"id":"studio.install","state":"done","percent":100}]}"#,
        )
        .unwrap();
        std::fs::write(
            meet.join("history.json"),
            r#"[{"id":"946684800","started":946684800,"kind":"apply","label":"studio","state":"done"}]"#,
        )
        .unwrap();
        cfg.updates_dir = Some(meet);
    }
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

/// What changes from one run to the next (identifiers, times, the version of this build, secrets) is replaced by a fixed value, so the files stay the same.
fn steady(v: &mut Value, key: &str) {
    match v {
        Value::Object(map) => {
            for (k, x) in map.iter_mut() {
                steady(x, k);
            }
        }
        Value::Array(items) => {
            for x in items.iter_mut() {
                steady(x, key);
            }
        }
        Value::String(s) => {
            if key == "id" && s.starts_with('j') && s.len() > 8 {
                *s = "j-0000000000000000".to_string();
            } else if key == "token" || key == "node_token" || key == "head_token" {
                *s = "secret-token".to_string();
            } else if key == "installed" {
                *s = "0.0.0".to_string();
            } else if key == "id" && s.chars().all(|c| c.is_ascii_digit()) {
                *s = "946684800".to_string();
            }
        }
        Value::Number(n) => {
            if matches!(
                key,
                "created" | "created_at" | "started_at" | "finished_at" | "updated_at" | "at" | "t"
            ) {
                *v = json!(946684800);
            } else if let Some(x) = n.as_f64().filter(|_| n.is_f64()) {
                // the sines and roots of the plan may differ in the last digits from one system to another
                *v = json!((x * 10.0).round() / 10.0);
            }
        }
        _ => {}
    }
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/examples")
}

/// Compares what the server answered with the example file, or writes the file when asked to.
fn example(name: &str, mut answer: Value) {
    steady(&mut answer, "");
    let text = serde_json::to_string_pretty(&answer).unwrap() + "\n";
    let file = examples_dir().join(format!("{name}.json"));
    if std::env::var("EH3D_WRITE_EXAMPLES").is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(examples_dir()).unwrap();
        std::fs::write(&file, text).unwrap();
        return;
    }
    let kept = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "{} is missing: EH3D_WRITE_EXAMPLES=1 makes it",
            file.display()
        )
    });
    assert_eq!(
        kept.replace("\r\n", "\n"),
        text,
        "{name}.json is not what the server answers now: EH3D_WRITE_EXAMPLES=1 cargo test --test examples makes the files again, and the clients take them with tools/sync_api_examples.py"
    );
}

fn drill_job() -> Value {
    json!({
        "kind": "drill", "name": "board",
        "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}, {"number": 2, "feed_mm_min": 800, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}, {"tool": 2, "x_mm": 30, "y_mm": 20}]
    })
}

#[tokio::test]
async fn the_answers_of_the_jobs_the_fleet_and_the_spindle_match_the_examples_the_clients_read() {
    let s = server("jobs", false);
    let token = admin(&s).await;
    for (id, name) in [("bungard", "Bungard CCD"), ("spare", "Spare")] {
        let (status, body) = call(
            &s,
            Method::POST,
            "/api/v1/machines",
            Some(&token),
            Some(json!({"id": id, "name": name, "model": "ccd-mtc", "station": "bench", "connection": {"kind": "demo"}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, session) = call(&s, Method::GET, "/api/v1/session", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    example("session", session);
    let (status, health) = call(&s, Method::GET, "/api/v1/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let mut health = health;
    health["version"] = json!("0.0.0");
    example("health", health);
    let (status, mut detail) = call(&s, Method::GET, "/api/v1/health", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    detail["version"] = json!("0.0.0");
    detail["uptime_s"] = json!(0);
    // the same whether or not the server was built with the serial port support
    detail["features"]["serial"] = json!(false);
    example("health_detail", detail);

    let (status, preview) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bungard/preview",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    example("preview", preview);
    let mut bad = drill_job();
    bad["holes"] = json!([{"tool": 9, "x_mm": 1, "y_mm": 1}]);
    let (status, refused) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bungard/preview",
        Some(&token),
        Some(bad),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    example("error", refused);

    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bungard/jobs",
        Some(&token),
        Some(drill_job()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    example("job", job);
    let (_, jobs) = call(&s, Method::GET, "/api/v1/jobs", Some(&token), None).await;
    example("jobs", jobs);
    let (status, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    example("fleet", fleet);
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/machines/bungard/spindle",
        Some(&token),
        Some(json!({"spindle": "vfd", "vfd": {"max_xy_mm_min": 900}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, spindle) = call(
        &s,
        Method::GET,
        "/api/v1/machines/bungard/spindle",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    example("spindle", spindle);
}

#[tokio::test]
async fn the_answer_about_the_updates_matches_the_example_the_studio_reads() {
    let s = server("updates", true);
    let token = admin(&s).await;
    let (status, view) = call(&s, Method::GET, "/api/v1/updates", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    example("updates", view);
}
