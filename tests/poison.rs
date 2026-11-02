//! A thread that panics while it holds a lock of the state must not leave the server dead. It is a test file of its own because the count of the locks that were taken
//! over is of the whole process: in the same process as the other tests it would make their health checks `degraded`.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use eh3d_server::sync::Locked;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
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
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_thread_that_panicked_with_a_lock_does_not_leave_the_server_dead() {
    let dir = std::env::temp_dir().join(format!("eh3d-server-poison-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    })
    .unwrap();
    let app = router(state.clone());

    let code = state.auth.locked().setup_code().unwrap().to_string();
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        None,
        Some(json!({"code": code, "name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, login) = call(
        &app,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    let token = login["token"].as_str().unwrap().to_string();
    let (_, before) = call(&app, Method::GET, "/api/v1/health", Some(&token), None).await;
    assert_eq!(before["status"], "ok");

    // a thread panics while it holds the lock of the users
    let held = state.clone();
    let _ = std::thread::spawn(move || {
        let _held = held.auth.lock().unwrap();
        panic!("a bug in some handler");
    })
    .join();
    assert!(state.auth.lock().is_err(), "the lock is poisoned");

    // the server still answers: the token of before identifies, a login works
    let (status, me) = call(&app, Method::GET, "/api/v1/session", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // and the health check tells that something happened
    let (_, health) = call(&app, Method::GET, "/api/v1/health", Some(&token), None).await;
    assert_eq!(health["status"], "degraded");
    assert!(health["recovered_locks"].as_u64().unwrap() >= 1);
    // the lock is clean again
    assert!(state.auth.lock().is_ok());
    state.stop_all();
    let _ = std::fs::remove_dir_all(&dir);
}
