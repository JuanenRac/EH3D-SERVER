//! What the tests of the common services share: a server on a temporary folder, requests through the real router, the first administrator and users with a role.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

pub const PASSWORD: &str = "clave-larga-123";

pub struct Server {
    pub app: Router,
    pub state: Arc<AppState>,
    pub dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.state.stop_all();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn server_with(name: &str, change: impl FnOnce(&mut ServerConfig)) -> Server {
    let dir = std::env::temp_dir().join(format!("eh3d-server-svc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = ServerConfig {
        data_dir: dir.clone(),
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

pub fn server(name: &str) -> Server {
    server_with(name, |_| {})
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    pub fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    pub fn error(&self) -> String {
        self.json()["error"].as_str().unwrap_or("").to_string()
    }
}

/// A request with any body and any headers.
pub async fn send(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
    content_type: Option<&str>,
) -> Reply {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        request = request.header("authorization", format!("Bearer {t}"));
    }
    if let Some(c) = content_type {
        request = request.header("content-type", c);
    }
    for (k, v) in headers {
        request = request.header(*k, *v);
    }
    let request = request
        .body(body.map(Body::from).unwrap_or_else(Body::empty))
        .unwrap();
    let response = s.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        bytes,
    }
}

/// A request with a JSON body (or none); the answer as JSON.
pub async fn call(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let has_body = body.is_some();
    let reply = send(
        s,
        method,
        path,
        token,
        &[],
        body.map(|b| b.to_string().into_bytes()),
        if has_body {
            Some("application/json")
        } else {
            None
        },
    )
    .await;
    (reply.status, reply.json())
}

/// Makes the first administrator and returns the token of their login.
pub async fn admin(s: &Server) -> String {
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

/// Makes a user with a role (the administrator's token is `boss`) and returns the token of their login.
pub async fn user(s: &Server, boss: &str, name: &str, role: &str) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/users",
        Some(boss),
        Some(json!({"name": name, "role": role, "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": name, "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().unwrap().to_string()
}

/// Makes a user with a role without logging them in (the administrator's token is `boss`).
pub async fn add_user(s: &Server, boss: &str, name: &str, role: &str) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/users",
        Some(boss),
        Some(json!({"name": name, "role": role, "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A token of a program with scopes (the administrator's token is `boss`).
pub async fn scoped_token(s: &Server, boss: &str, role: &str, scopes: &[&str]) -> String {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/tokens",
        Some(boss),
        Some(json!({"name": "program", "role": role, "scopes": scopes})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().unwrap().to_string()
}

/// A text for the query of an address: everything but letters, digits and `-_.` as `%XX`.
pub fn enc(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
