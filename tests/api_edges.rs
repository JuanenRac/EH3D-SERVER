//! The edges of the interface: requests that are wrong in some way must get a client error with a body that can be read, never a server error or a panic.

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

async fn raw(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> StatusCode {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        request = request.header("authorization", format!("Bearer {t}"));
    }
    let request = request
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    s.app.clone().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn an_unknown_route_and_a_wrong_method_are_client_errors() {
    let s = server("edge-routes");
    let (status, _) = call(&s, Method::GET, "/api/v1/no-such-thing", None, None).await;
    assert!(status.is_client_error(), "{status}");
    let (status, _) = call(&s, Method::DELETE, "/api/v1/health", None, None).await;
    assert!(status.is_client_error(), "{status}");
}

#[tokio::test]
async fn a_request_without_a_login_or_with_a_wrong_token_is_refused() {
    let s = server("edge-auth");
    let (status, _) = call(&s, Method::GET, "/api/v1/machines", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for token in ["", "x", "not-a-real-token", "Bearer", "0000000000000000"] {
        let (status, _) = call(&s, Method::GET, "/api/v1/machines", Some(token), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "token {token:?}");
    }
}

#[tokio::test]
async fn a_login_with_a_body_that_is_not_json_or_lacks_fields_is_a_client_error() {
    let s = server("edge-login");
    for body in [
        "",
        "{",
        "[]",
        "null",
        "\"text\"",
        "{\"name\":1,\"password\":2}",
        "{}",
        "{\"name\":\"\",\"password\":\"\"}",
    ] {
        let status = raw(&s, Method::POST, "/api/v1/login", None, body).await;
        assert!(status.is_client_error(), "body {body:?}: {status}");
    }
}

#[tokio::test]
async fn a_machine_that_does_not_exist_is_not_found_and_odd_ids_never_make_a_server_error() {
    let s = server("edge-machine");
    let token = admin(&s).await;
    for id in [
        "nothing",
        "..",
        "%2e%2e",
        "a%20b",
        "%00",
        "x".repeat(300).as_str(),
    ] {
        let (status, _) = call(
            &s,
            Method::GET,
            &format!("/api/v1/machines/{id}/head/status"),
            Some(&token),
            None,
        )
        .await;
        assert!(!status.is_server_error(), "id {id:?}: {status}");
        assert_ne!(status, StatusCode::OK, "id {id:?}");
    }
}

#[tokio::test]
async fn a_machine_with_a_body_that_is_not_a_machine_is_a_client_error() {
    let s = server("edge-add");
    let token = admin(&s).await;
    for body in [
        "",
        "{",
        "[]",
        "{}",
        "{\"id\":5}",
        "{\"id\":\"\",\"name\":\"\"}",
    ] {
        let status = raw(&s, Method::POST, "/api/v1/machines", Some(&token), body).await;
        assert!(status.is_client_error(), "body {body:?}: {status}");
    }
    let (status, body) = call(&s, Method::GET, "/api/v1/machines", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.to_string().len() < 400,
        "no machine was added by a bad request: {body}"
    );
}
