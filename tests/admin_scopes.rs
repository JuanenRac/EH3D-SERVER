//! The scopes of the requests of administration: a token that has scopes needs the scope of the request even if it is an administrator's, a token
//! without scopes does whatever its role allows, and a limited token cannot make a looser one.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, scoped_token, server, PASSWORD};
use serde_json::{json, Value};

/// Every request of administration with a body that passes the checks of the shape, and the scope it needs.
fn table() -> Vec<(Method, &'static str, Option<Value>, &'static str)> {
    let user = json!({"name": "zed", "role": "viewer", "password": PASSWORD});
    vec![
        (Method::GET, "/api/v1/users", None, "admin:users"),
        (Method::POST, "/api/v1/users", Some(user), "admin:users"),
        (
            Method::PUT,
            "/api/v1/users/nobody",
            Some(json!({"role": "viewer"})),
            "admin:users",
        ),
        (Method::DELETE, "/api/v1/users/nobody", None, "admin:users"),
        (
            Method::GET,
            "/api/v1/auth/sessions?all=true",
            None,
            "admin:users",
        ),
        (Method::GET, "/api/v1/tokens", None, "admin:tokens"),
        (
            Method::POST,
            "/api/v1/tokens",
            Some(json!({"name": "other", "role": "viewer", "scopes": ["admin:tokens"]})),
            "admin:tokens",
        ),
        (
            Method::POST,
            "/api/v1/tokens/nobody/rotate",
            None,
            "admin:tokens",
        ),
        (
            Method::DELETE,
            "/api/v1/tokens/nobody",
            None,
            "admin:tokens",
        ),
        (Method::GET, "/api/v1/auth/keys", None, "admin:tokens"),
        (Method::GET, "/api/v1/auth/policy", None, "admin:policy"),
        (
            Method::PUT,
            "/api/v1/auth/policy",
            Some(json!({"key": "no_such_switch", "value": true})),
            "admin:policy",
        ),
        (Method::GET, "/api/v1/audit", None, "admin:policy"),
        (Method::GET, "/api/v1/updates", None, "admin:updates"),
        (Method::POST, "/api/v1/updates/check", None, "admin:updates"),
        (
            Method::POST,
            "/api/v1/updates/apply",
            Some(json!({"components": ["server"]})),
            "admin:updates",
        ),
        (
            Method::POST,
            "/api/v1/updates/undo",
            Some(json!({"components": ["server"]})),
            "admin:updates",
        ),
        (
            Method::POST,
            "/api/v1/updates/clear-history",
            None,
            "admin:updates",
        ),
    ]
}

fn refused_for_scope(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::FORBIDDEN && body["error"] == "forbidden_scope"
}

#[tokio::test]
async fn a_token_with_scopes_needs_the_scope_of_each_request_of_administration() {
    let s = server("admin-scopes-table");
    let boss = admin(&s).await;
    // the token of a design program that was an administrator's: it can write catalogs, and nothing of the administration
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
        // without the scope
        let (status, answer) = call(&s, method.clone(), path, Some(&program), body.clone()).await;
        assert!(
            refused_for_scope(status, &answer),
            "{method} {path}: {status} {answer}"
        );
        // with only a scope of another family of the administration
        let other = [
            "admin:users",
            "admin:tokens",
            "admin:policy",
            "admin:updates",
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
        // with its scope the request goes through to its own answer (whatever it is, it is not a refusal of the scope or of the role)
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
async fn the_scope_does_not_lift_the_role() {
    let s = server("admin-scopes-role");
    let boss = admin(&s).await;
    let operator = scoped_token(&s, &boss, "operator", &["admin:users", "admin:tokens"]).await;
    for path in ["/api/v1/users", "/api/v1/tokens"] {
        let (status, answer) = call(&s, Method::GET, path, Some(&operator), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} {answer}");
        assert_eq!(answer["error"], "forbidden", "{path}");
    }
}

#[tokio::test]
async fn a_program_token_made_without_scopes_cannot_administer_and_cannot_be_given_the_scopes() {
    let s = server("admin-scopes-programs");
    let boss = admin(&s).await;
    for template in ["phoenix", "cam"] {
        let (status, body) = call(
            &s,
            Method::POST,
            "/api/v1/tokens",
            Some(&boss),
            Some(json!({"name": "design", "role": "admin", "template": template})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = body["token"].as_str().unwrap().to_string();
        let (status, answer) = call(&s, Method::GET, "/api/v1/users", Some(&token), None).await;
        assert!(refused_for_scope(status, &answer), "{template}: {answer}");
        for scope in [
            "admin:users",
            "admin:tokens",
            "admin:policy",
            "admin:updates",
        ] {
            let (status, answer) = call(
                &s,
                Method::POST,
                "/api/v1/tokens",
                Some(&boss),
                Some(
                    json!({"name": "d2", "role": "admin", "template": template, "scopes": [scope]}),
                ),
            )
            .await;
            assert!(
                refused_for_scope(status, &answer),
                "{template} {scope}: {status} {answer}"
            );
        }
    }
}

#[tokio::test]
async fn a_limited_token_cannot_make_a_token_that_gets_out_of_its_limits() {
    let s = server("admin-scopes-jail");
    let boss = admin(&s).await;
    let limited = scoped_token(&s, &boss, "admin", &["admin:tokens", "catalog:read"]).await;
    let make = |scopes: Option<Value>| {
        let mut body = json!({"name": "child", "role": "admin"});
        if let Some(list) = scopes {
            body["scopes"] = list;
        }
        body
    };
    // none given means the whole of the role: that is out of the limits
    for scopes in [
        None,
        Some(json!(["catalog:read", "jobs:start"])),
        Some(json!(["admin:users"])),
        Some(json!([])),
    ] {
        let wanted = scopes.clone();
        let (status, answer) = call(
            &s,
            Method::POST,
            "/api/v1/tokens",
            Some(&limited),
            Some(make(scopes)),
        )
        .await;
        // an empty list is inside the limits (it can do nothing), the others are not
        if wanted == Some(json!([])) {
            assert_eq!(status, StatusCode::OK, "{answer}");
        } else {
            assert!(refused_for_scope(status, &answer), "{wanted:?}: {answer}");
        }
    }
    // inside the limits it is made
    let (status, answer) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&limited),
        Some(make(Some(json!(["catalog:read"])))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    // a person (and a token without scopes) makes any
    let (status, answer) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&boss),
        Some(make(None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
}
