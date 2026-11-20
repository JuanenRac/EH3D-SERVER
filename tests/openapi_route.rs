//! The server serves its own contract at `GET /api/v1/openapi.json`: the same document as `docs/openapi.json`, for any login (a viewer), with a tag that lets a client ask
//! whether it changed and a time it may be kept. A client uses it to notice the routes the server it talks to no longer has.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, send, server, user};

const PATH: &str = "/api/v1/openapi.json";

#[tokio::test]
async fn it_needs_a_login_and_a_viewer_is_enough() {
    let s = server("openapi-route-auth");
    let boss = admin(&s).await;
    let anonymous = send(&s, Method::GET, PATH, None, &[], None, None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let reply = send(&s, Method::GET, PATH, Some(&viewer), &[], None, None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), "application/json");
}

#[tokio::test]
async fn it_is_the_document_of_the_repository_and_names_its_own_route() {
    let s = server("openapi-route-body");
    let boss = admin(&s).await;
    let reply = send(&s, Method::GET, PATH, Some(&boss), &[], None, None).await;
    assert_eq!(reply.status, StatusCode::OK);
    let file = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/openapi.json"),
    )
    .unwrap();
    assert_eq!(String::from_utf8(reply.bytes.clone()).unwrap(), file);
    let doc = reply.json();
    assert_eq!(doc["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(doc["paths"]["/openapi.json"]["get"].is_object());
}

#[tokio::test]
async fn it_can_be_kept_and_asked_again_with_its_tag() {
    let s = server("openapi-route-etag");
    let boss = admin(&s).await;
    let first = send(&s, Method::GET, PATH, Some(&boss), &[], None, None).await;
    let tag = first.header("etag");
    assert!(
        tag.starts_with('"') && tag.ends_with('"') && tag.len() > 10,
        "{tag}"
    );
    assert_eq!(first.header("cache-control"), "private, max-age=300");
    for header in [
        tag.clone(),
        format!("W/{tag}"),
        "*".to_string(),
        format!("\"other\", {tag}"),
    ] {
        let again = send(
            &s,
            Method::GET,
            PATH,
            Some(&boss),
            &[("if-none-match", header.as_str())],
            None,
            None,
        )
        .await;
        assert_eq!(again.status, StatusCode::NOT_MODIFIED, "{header}");
        assert!(again.bytes.is_empty());
        assert_eq!(again.header("etag"), tag);
    }
    let changed = send(
        &s,
        Method::GET,
        PATH,
        Some(&boss),
        &[("if-none-match", "\"another\"")],
        None,
        None,
    )
    .await;
    assert_eq!(changed.status, StatusCode::OK);
    assert_eq!(changed.bytes, first.bytes);
}
