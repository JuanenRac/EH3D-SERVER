//! The scopes of the requests about the machines and their head: a token that has scopes needs the scope of each request even if its role would
//! allow it, and a token without scopes keeps whatever its role allows.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, scoped_token, server, Server};
use serde_json::{json, Value};

fn machine(id: &str) -> Value {
    json!({"id": id, "name": "Bungard", "model": "ccd-mtc", "connection": {"kind": "demo"}})
}

/// Every request about the machines that needs a scope besides the role, with a body that passes the checks of the shape.
fn table() -> Vec<(Method, &'static str, Option<Value>, &'static str)> {
    vec![
        (
            Method::POST,
            "/api/v1/machines",
            Some(machine("m2")),
            "machines:manage",
        ),
        (
            Method::PUT,
            "/api/v1/machines/m1",
            Some(machine("m1")),
            "machines:manage",
        ),
        (
            Method::DELETE,
            "/api/v1/machines/nope",
            None,
            "machines:manage",
        ),
        (
            Method::PUT,
            "/api/v1/machines/m1/spindle",
            Some(json!({})),
            "machines:manage",
        ),
        (
            Method::GET,
            "/api/v1/machines/m1/spindle",
            None,
            "machines:view",
        ),
        (Method::DELETE, "/api/v1/jobs/nojob", None, "jobs:cancel"),
        (
            Method::GET,
            "/api/v1/machines/m1/head/status",
            None,
            "machines:view",
        ),
        (
            Method::GET,
            "/api/v1/machines/m1/head/stream",
            None,
            "machines:view",
        ),
        (
            Method::GET,
            "/api/v1/machines/m1/head/light",
            None,
            "machines:view",
        ),
        (
            Method::GET,
            "/api/v1/machines/m1/head/snapshot",
            None,
            "machines:view",
        ),
        (
            Method::GET,
            "/api/v1/machines/m1/head/inspect",
            None,
            "machines:view",
        ),
        (
            Method::GET,
            "/api/v1/machines/m1/head/inspect/profile",
            None,
            "machines:view",
        ),
        (
            Method::POST,
            "/api/v1/machines/m1/head/measure?samples=1",
            None,
            "jobs:prepare",
        ),
        (
            Method::POST,
            "/api/v1/machines/m1/head/light?on=1",
            None,
            "jobs:prepare",
        ),
        (
            Method::POST,
            "/api/v1/machines/m1/head/inspect",
            None,
            "jobs:prepare",
        ),
        (
            Method::PUT,
            "/api/v1/machines/m1/head/inspect/profile",
            Some(json!({})),
            "jobs:prepare",
        ),
        (
            Method::POST,
            "/api/v1/machines/m1/head/inspect/calibrate",
            None,
            "jobs:prepare",
        ),
    ]
}

async fn with_machine(name: &str) -> (Server, String) {
    let s = server(name);
    let boss = admin(&s).await;
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines",
        Some(&boss),
        Some(machine("m1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (s, boss)
}

fn refused_for_scope(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::FORBIDDEN && body["error"] == "forbidden_scope"
}

#[tokio::test]
async fn a_token_with_scopes_needs_the_scope_of_each_request_about_the_machines() {
    let (s, boss) = with_machine("machine-scopes-table").await;
    // the token of a design program that was an administrator's
    let program = scoped_token(&s, &boss, "admin", &["catalog:write", "projects:write"]).await;
    let legacy = {
        let (status, body) = call(
            &s,
            Method::POST,
            "/api/v1/tokens",
            Some(&boss),
            Some(json!({"name": "legacy", "role": "admin"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"].as_str().unwrap().to_string()
    };
    for (method, path, body, scope) in table() {
        let (status, answer) = call(&s, method.clone(), path, Some(&program), body.clone()).await;
        assert!(
            refused_for_scope(status, &answer),
            "{method} {path}: {status} {answer}"
        );
        // a scope of another request of the machines is not the one of this
        let other = [
            "machines:view",
            "jobs:prepare",
            "jobs:cancel",
            "machines:manage",
        ]
        .into_iter()
        .find(|o| *o != scope)
        .unwrap();
        let wrong = scoped_token(&s, &boss, "admin", &[other]).await;
        let (status, answer) = call(&s, method.clone(), path, Some(&wrong), body.clone()).await;
        assert!(
            refused_for_scope(status, &answer),
            "{method} {path} with {other}: {status} {answer}"
        );
        // with its scope the request goes through to its own answer (whatever it is, it is not a refusal)
        let right = scoped_token(&s, &boss, "admin", &[scope]).await;
        let (status, answer) = call(&s, method.clone(), path, Some(&right), body.clone()).await;
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "{method} {path} with {scope}: {status} {answer}"
        );
        // a token without scopes keeps whatever its role allows
        let (status, answer) = call(&s, method.clone(), path, Some(&legacy), body.clone()).await;
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "{method} {path} without scopes: {status} {answer}"
        );
    }
}

#[tokio::test]
async fn the_scope_of_the_machines_does_not_lift_the_role() {
    let (s, boss) = with_machine("machine-scopes-role").await;
    let viewer = scoped_token(&s, &boss, "viewer", &["machines:manage", "jobs:prepare"]).await;
    for (method, path, body) in [
        (Method::PUT, "/api/v1/machines/m1/spindle", Some(json!({}))),
        (Method::POST, "/api/v1/machines/m1/head/inspect", None),
    ] {
        let (status, answer) = call(&s, method, path, Some(&viewer), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} {answer}");
        assert_eq!(answer["error"], "forbidden", "{path}");
    }
}
