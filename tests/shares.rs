//! Who owns what, who it is shared with, and what a token may do: projects with their revisions, the sharing of projects and of artifacts, the scopes and the templates of the
//! tokens of programs. Most of these tests are about what must be refused.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use base64::Engine as _;
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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

fn server_with(name: &str, change: impl FnOnce(&mut ServerConfig)) -> Server {
    let dir =
        std::env::temp_dir().join(format!("eh3d-server-shares-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = ServerConfig {
        data_dir: dir.clone(),
        tasks_selftest: true,
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

fn server(name: &str) -> Server {
    server_with(name, |_| {})
}

async fn raw(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
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
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

async fn call(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, bytes) = raw(s, method, path, token, body).await;
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
    call(
        s,
        Method::POST,
        "/api/v1/setup",
        None,
        Some(json!({"code": code, "name": "juan", "password": "una-clave-larga"})),
    )
    .await;
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

/// A token with the role (and, when given, a template and scopes): gives the id and the secret.
async fn token_with(
    s: &Server,
    admin: &str,
    name: &str,
    role: &str,
    extra: Value,
) -> (String, String) {
    let mut body = json!({"name": name, "role": role});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        body[k] = v;
    }
    let (st, b) = call(s, Method::POST, "/api/v1/tokens", Some(admin), Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    (
        b["id"].as_str().unwrap().into(),
        b["token"].as_str().unwrap().into(),
    )
}

async fn token(s: &Server, admin: &str, name: &str, role: &str) -> (String, String) {
    token_with(s, admin, name, role, json!({})).await
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn put(s: &Server, t: &str, name: &str, bytes: &[u8]) -> Value {
    let (st, b) = call(
        s,
        Method::POST,
        "/api/v1/artifacts",
        Some(t),
        Some(json!({"name": name, "format": "text/plain", "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes)})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b
}

fn code(b: &Value) -> Option<&str> {
    b["error"].as_str()
}

#[tokio::test]
async fn a_project_has_an_owner_and_its_revisions_follow_each_other() {
    let s = server("project");
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (st, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "board one"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{p}");
    let pid = p["id"].as_str().unwrap().to_string();
    assert!(eh3d_server::ids::is_valid(
        eh3d_server::ids::Kind::Project,
        &pid
    ));
    assert_eq!(
        (p["head"].clone(), p["shares"].clone()),
        (json!(0), json!([]))
    );
    let one = put(&s, &ana, "v1", b"first").await;
    let two = put(&s, &ana, "v2", b"second").await;
    let path = format!("/api/v1/projects/{pid}/revisions");
    let rev = |artifact: &Value, parent: u64| json!({"artifact_id": artifact["artifact_id"], "parent": parent});
    let (st, r) = call(&s, Method::POST, &path, Some(&ana), Some(rev(&one, 0))).await;
    assert_eq!((st, r["number"].clone()), (StatusCode::CREATED, json!(1)));
    let (st, r) = call(&s, Method::POST, &path, Some(&ana), Some(rev(&two, 1))).await;
    assert_eq!((st, r["number"].clone()), (StatusCode::CREATED, json!(2)));
    // two saves from the same parent: the second is told
    let (st, b) = call(&s, Method::POST, &path, Some(&ana), Some(rev(&one, 1))).await;
    assert_eq!((st, code(&b)), (StatusCode::CONFLICT, Some("stale_parent")));
    let (_, got) = call(
        &s,
        Method::GET,
        &format!("/api/v1/projects/{pid}"),
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(got["head"], 2);
    assert_eq!(got["revisions"].as_array().unwrap().len(), 2);
    // an artifact that does not exist, or a name that is empty
    let (st, b) = call(
        &s,
        Method::POST,
        &path,
        Some(&ana),
        Some(json!({"artifact_id": "a-00000000-0000-7000-8000-000000000000", "parent": 2})),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_artifact"))
    );
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "  "})),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_name"))
    );
    // an artifact in a revision cannot be deleted
    let (st, b) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{}", one["artifact_id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::CONFLICT, Some("artifact_in_use"))
    );
}

#[tokio::test]
async fn nothing_is_shared_by_default_and_a_project_of_another_does_not_exist() {
    let s = server("hidden");
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (_, bob) = token(&s, &a, "bob", "operator").await;
    let (cy_id, _) = token(&s, &a, "cy", "operator").await;
    let (_, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "private"})),
    )
    .await;
    let pid = p["id"].as_str().unwrap();
    let art = put(&s, &ana, "v1", b"private bytes").await;
    call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{pid}/revisions"),
        Some(&ana),
        Some(json!({"artifact_id": art["artifact_id"], "parent": 0})),
    )
    .await;
    for (method, path, body) in [
        (Method::GET, format!("/api/v1/projects/{pid}"), None),
        (Method::DELETE, format!("/api/v1/projects/{pid}"), None),
        (
            Method::POST,
            format!("/api/v1/projects/{pid}/revisions"),
            Some(json!({"artifact_id": art["artifact_id"], "parent": 1})),
        ),
        (
            Method::POST,
            format!("/api/v1/projects/{pid}/shares"),
            Some(json!({"token": cy_id, "access": "read"})),
        ),
        (
            Method::DELETE,
            format!("/api/v1/projects/{pid}/shares/bob"),
            None,
        ),
    ] {
        let (st, b) = call(&s, method.clone(), &path, Some(&bob), body).await;
        assert!(
            (st, code(&b)) == (StatusCode::NOT_FOUND, Some("unknown_project"))
                || (st, code(&b)) == (StatusCode::NOT_FOUND, Some("unknown_artifact")),
            "{method} {path}: {st} {b}"
        );
    }
    let (_, list) = call(&s, Method::GET, "/api/v1/projects", Some(&bob), None).await;
    assert_eq!(list, json!([]));
    // and the same answer as for an id that was never made
    let (st, b) = call(
        &s,
        Method::GET,
        "/api/v1/projects/p-00000000-0000-7000-8000-000000000000",
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_project"))
    );
    // the bytes of its revision are not his either, and the question about the blob answers no
    let (st, _) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{}", art["artifact_id"].as_str().unwrap()),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = raw(
        &s,
        Method::HEAD,
        &format!("/api/v1/blobs/{}", sha(b"private bytes")),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // the administrator sees everything
    let (_, list) = call(&s, Method::GET, "/api/v1/projects", Some(&a), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn sharing_gives_exactly_the_access_asked_for_and_can_be_taken_back() {
    let s = server("share");
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (bob_id, bob) = token(&s, &a, "bob", "operator").await;
    let (cy_id, cy) = token(&s, &a, "cy", "operator").await;
    let (_, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "shared"})),
    )
    .await;
    let pid = p["id"].as_str().unwrap();
    let art = put(&s, &ana, "v1", b"shared bytes").await;
    let rev = format!("/api/v1/projects/{pid}/revisions");
    call(
        &s,
        Method::POST,
        &rev,
        Some(&ana),
        Some(json!({"artifact_id": art["artifact_id"], "parent": 0})),
    )
    .await;
    // read access: sees the project and the bytes of its revisions, writes nothing, shares nothing
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{pid}/shares"),
        Some(&ana),
        Some(json!({"token": bob_id, "access": "read"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, seen) = call(
        &s,
        Method::GET,
        &format!("/api/v1/projects/{pid}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        seen.get("shares").is_none(),
        "who else sees it is for the owner to know"
    );
    let (_, list) = call(&s, Method::GET, "/api/v1/projects", Some(&bob), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (st, bytes) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{}", art["artifact_id"].as_str().unwrap()),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(
        (st, bytes.as_slice()),
        (StatusCode::OK, &b"shared bytes"[..])
    );
    let (st, _) = raw(
        &s,
        Method::HEAD,
        &format!("/api/v1/blobs/{}", sha(b"shared bytes")),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mine = put(&s, &bob, "his", b"his bytes").await;
    let (st, b) = call(
        &s,
        Method::POST,
        &rev,
        Some(&bob),
        Some(json!({"artifact_id": mine["artifact_id"], "parent": 1})),
    )
    .await;
    assert_eq!((st, code(&b)), (StatusCode::FORBIDDEN, Some("forbidden")));
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{pid}/shares"),
        Some(&bob),
        Some(json!({"token": cy_id, "access": "read"})),
    )
    .await;
    assert_eq!((st, code(&b)), (StatusCode::FORBIDDEN, Some("forbidden")));
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/projects/{pid}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // cy still sees nothing
    let (st, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/projects/{pid}"),
        Some(&cy),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // write access: bob adds a revision
    call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{pid}/shares"),
        Some(&ana),
        Some(json!({"token": bob_id, "access": "write"})),
    )
    .await;
    let (st, r) = call(
        &s,
        Method::POST,
        &rev,
        Some(&bob),
        Some(json!({"artifact_id": mine["artifact_id"], "parent": 1})),
    )
    .await;
    assert_eq!((st, r["number"].clone()), (StatusCode::CREATED, json!(2)));
    // and the owner sees who it is shared with
    let (_, owner_view) = call(
        &s,
        Method::GET,
        &format!("/api/v1/projects/{pid}"),
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(
        owner_view["shares"],
        json!([{"who": format!("token:{bob_id}"), "access": "write"}])
    );
    // an artifact bob did not make is still not his if it is in no project he sees
    let other = put(&s, &ana, "unrelated", b"unrelated").await;
    let (st, _) = raw(
        &s,
        Method::GET,
        &format!(
            "/api/v1/artifacts/{}",
            other["artifact_id"].as_str().unwrap()
        ),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // taking it back: the project, its bytes and the question about the blob go with it
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/projects/{pid}/shares/token:{bob_id}"),
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/projects/{pid}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{}", art["artifact_id"].as_str().unwrap()),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, b) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/projects/{pid}/shares/token:{bob_id}"),
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_project")),
        "nothing to take back"
    );
    let _ = cy_id;
}

#[tokio::test]
async fn a_share_is_checked_and_can_be_switched_off() {
    let s = server("share-bad");
    let a = admin(&s).await;
    let (ana_id, ana) = token(&s, &a, "ana", "operator").await;
    let (_, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "x"})),
    )
    .await;
    let path = format!("/api/v1/projects/{}/shares", p["id"].as_str().unwrap());
    for (body, status, error) in [
        (
            json!({"token": "nobody", "access": "read"}),
            StatusCode::NOT_FOUND,
            "unknown_token",
        ),
        (
            json!({"user": "nobody", "access": "read"}),
            StatusCode::NOT_FOUND,
            "unknown_user",
        ),
        (
            json!({"access": "read"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_share",
        ),
        (
            json!({"user": "juan", "token": ana_id, "access": "read"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_share",
        ),
        (
            json!({"token": ana_id, "access": "read"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_share",
        ),
        (
            json!({"user": "juan", "access": "own"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_request",
        ),
    ] {
        let (st, b) = call(&s, Method::POST, &path, Some(&ana), Some(body.clone())).await;
        assert_eq!((st, code(&b)), (status, Some(error)), "{body}");
    }
    // a user by name works
    let (st, _) = call(
        &s,
        Method::POST,
        &path,
        Some(&ana),
        Some(json!({"user": "juan", "access": "read"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, mine) = call(
        &s,
        Method::GET,
        &format!("/api/v1/projects/{}", p["id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(mine["shares"][0]["who"], "juan");
    drop(s);
    // with the switch off nothing can be shared
    let s = server_with("share-off", |c| c.shares_enabled = false);
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (_, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "x"})),
    )
    .await;
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{}/shares", p["id"].as_str().unwrap()),
        Some(&ana),
        Some(json!({"user": "juan", "access": "read"})),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_IMPLEMENTED, Some("shares_off"))
    );
    let art = put(&s, &ana, "v", b"v").await;
    let (st, b) = call(
        &s,
        Method::POST,
        &format!(
            "/api/v1/artifacts/{}/shares",
            art["artifact_id"].as_str().unwrap()
        ),
        Some(&ana),
        Some(json!({"user": "juan", "access": "read"})),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_IMPLEMENTED, Some("shares_off"))
    );
}

#[tokio::test]
async fn an_artifact_is_shared_for_reading_by_its_owner_only() {
    let s = server("share-art");
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (bob_id, bob) = token(&s, &a, "bob", "operator").await;
    let (cy_id, _cy) = token(&s, &a, "cy", "operator").await;
    let art = put(&s, &ana, "doc", b"one file").await;
    let id = art["artifact_id"].as_str().unwrap();
    let path = format!("/api/v1/artifacts/{id}/shares");
    let (st, _) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // only for reading
    let (st, b) = call(
        &s,
        Method::POST,
        &path,
        Some(&ana),
        Some(json!({"token": bob_id, "access": "write"})),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_share"))
    );
    let (st, _) = call(
        &s,
        Method::POST,
        &path,
        Some(&ana),
        Some(json!({"token": bob_id, "access": "read"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, bytes) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!((st, bytes.as_slice()), (StatusCode::OK, &b"one file"[..]));
    // the one it was shared with cannot pass it on, look at who else has it, or delete it
    let (st, b) = call(
        &s,
        Method::POST,
        &path,
        Some(&bob),
        Some(json!({"token": cy_id, "access": "read"})),
    )
    .await;
    assert_eq!((st, code(&b)), (StatusCode::FORBIDDEN, Some("forbidden")));
    let (st, _) = call(&s, Method::GET, &path, Some(&bob), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, listed) = call(&s, Method::GET, &path, Some(&ana), None).await;
    assert_eq!(listed["shares"].as_array().unwrap().len(), 1);
    // an input that bob can see is accepted for his own artifact
    let (st, _) = call(&s, Method::POST, "/api/v1/artifacts", Some(&bob), Some(json!({"name": "derived", "format": "text/plain", "inputs": [{"artifact_id": id, "blob_sha256": art["blob_sha256"]}], "data_base64": ""}))).await;
    assert_eq!(st, StatusCode::CREATED);
    // taking it back
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("{path}/token:{bob_id}"),
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_deleted_project_is_hidden_and_only_an_administrator_purges_it() {
    let s = server("delete-project");
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (_, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "x"})),
    )
    .await;
    let path = format!("/api/v1/projects/{}", p["id"].as_str().unwrap());
    let (st, b) = call(&s, Method::POST, &format!("{path}/purge"), Some(&a), None).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_project")),
        "only a deleted project is purged"
    );
    let (st, _) = call(&s, Method::DELETE, &path, Some(&ana), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&s, Method::GET, &path, Some(&ana), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, kept) = call(&s, Method::GET, &path, Some(&a), None).await;
    assert_eq!(
        kept["deleted"], true,
        "the administrator still sees what was deleted"
    );
    let (st, _) = call(&s, Method::POST, &format!("{path}/purge"), Some(&ana), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&s, Method::POST, &format!("{path}/purge"), Some(&a), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&s, Method::GET, &path, Some(&a), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_token_of_a_design_program_can_never_hold_a_scope_of_the_machines() {
    let s = server("templates");
    let a = admin(&s).await;
    for template in ["phoenix", "cam"] {
        for scope in [
            "jobs:authorize",
            "jobs:start",
            "jobs:confirm",
            "jobs:cancel",
        ] {
            let (st, b) = call(&s, Method::POST, "/api/v1/tokens", Some(&a), Some(json!({"name": "p", "role": "operator", "template": template, "scopes": ["tasks:run", scope]}))).await;
            assert_eq!(
                (st, code(&b)),
                (StatusCode::FORBIDDEN, Some("forbidden_scope")),
                "{template} {scope}"
            );
        }
    }
    // the same scopes are fine for a person's client or a custom token
    for template in ["client", "custom"] {
        let (st, _) = call(&s, Method::POST, "/api/v1/tokens", Some(&a), Some(json!({"name": "p", "role": "operator", "template": template, "scopes": ["jobs:start", "jobs:confirm"]}))).await;
        assert_eq!(st, StatusCode::OK, "{template}");
    }
    let (st, b) = call(&s, Method::POST, "/api/v1/tokens", Some(&a), Some(json!({"name": "p", "role": "operator", "template": "phoenix", "scopes": ["tasks:run", "nonsense"]}))).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_scope"))
    );
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tokens",
        Some(&a),
        Some(json!({"name": "p", "role": "operator", "template": "robot"})),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_template"))
    );
    // nothing was made by the refusals: the list holds only the two good ones
    let (_, list) = call(&s, Method::GET, "/api/v1/tokens", Some(&a), None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    // a phoenix token without scopes gets what the design programs may hold, and a machine job is not among it
    let (_, ph) = token_with(&s, &a, "ph", "operator", json!({"template": "phoenix"})).await;
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&ph), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ph),
        Some(json!({"name": "from phoenix"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    for (method, path) in [
        (Method::POST, "/api/v1/jobs/j0000/start"),
        (Method::POST, "/api/v1/jobs/j0000/confirm"),
        (Method::POST, "/api/v1/jobs/j0000/cancel"),
        (Method::POST, "/api/v1/machines/m1/stop"),
    ] {
        let (st, b) = call(&s, method, path, Some(&ph), None).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("forbidden_scope")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn scopes_narrow_a_role_and_never_widen_it() {
    let s = server("scopes");
    let a = admin(&s).await;
    // a token with only the reading of tasks
    let (_, reader) = token_with(
        &s,
        &a,
        "reader",
        "operator",
        json!({"template": "custom", "scopes": ["tasks:read"]}),
    )
    .await;
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&reader), None).await;
    assert_eq!(st, StatusCode::OK);
    for (method, path, body) in [
        (
            Method::POST,
            "/api/v1/tasks",
            Some(json!({"kind": "server.selftest"})),
        ),
        (Method::GET, "/api/v1/machines", None),
        (Method::GET, "/api/v1/projects", None),
        (Method::GET, "/api/v1/artifacts", None),
        (
            Method::POST,
            "/api/v1/artifacts",
            Some(json!({"name": "x", "format": "a/b", "data_base64": ""})),
        ),
        (
            Method::POST,
            "/api/v1/uploads",
            Some(json!({"size": 1, "sha256": "0".repeat(64)})),
        ),
    ] {
        let (st, b) = call(&s, method, path, Some(&reader), body).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("forbidden_scope")),
            "{path}"
        );
    }
    let (st, _) = raw(
        &s,
        Method::HEAD,
        &format!("/api/v1/blobs/{}", "0".repeat(64)),
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // a viewer with the scope to run tasks still cannot: the role says no
    let (_, viewer) = token_with(
        &s,
        &a,
        "viewer",
        "viewer",
        json!({"scopes": ["tasks:run", "tasks:read"]}),
    )
    .await;
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&viewer),
        Some(json!({"kind": "server.selftest"})),
    )
    .await;
    assert_eq!((st, code(&b)), (StatusCode::FORBIDDEN, Some("forbidden")));
    // an empty list of scopes is a token that may do nothing that has a scope
    let (_, nothing) = token_with(&s, &a, "nothing", "admin", json!({"scopes": []})).await;
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&nothing), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // a token made without scopes keeps what its role gave it (nothing changes for what exists)
    let (_, plain) = token(&s, &a, "plain", "operator").await;
    for path in [
        "/api/v1/machines",
        "/api/v1/jobs",
        "/api/v1/tasks",
        "/api/v1/projects",
        "/api/v1/artifacts",
    ] {
        let (st, _) = call(&s, Method::GET, path, Some(&plain), None).await;
        assert_eq!(st, StatusCode::OK, "{path}");
    }
    // the list of tokens says what each carries
    let (_, list) = call(&s, Method::GET, "/api/v1/tokens", Some(&a), None).await;
    let r = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "reader")
        .unwrap();
    assert_eq!(
        (r["template"].clone(), r["scopes"].clone()),
        (json!("custom"), json!(["tasks:read"]))
    );
}

#[tokio::test]
async fn a_token_that_is_revoked_rotated_or_expired_stops_working() {
    let s = server("tokens");
    let a = admin(&s).await;
    let (id, old) = token_with(
        &s,
        &a,
        "prog",
        "operator",
        json!({"scopes": ["tasks:read"]}),
    )
    .await;
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&old), None).await;
    assert_eq!(st, StatusCode::OK);
    // rotation: the old secret stops at once, the new one has the same limits
    let (st, rotated) = call(
        &s,
        Method::POST,
        &format!("/api/v1/tokens/{id}/rotate"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rotated}");
    let new = rotated["token"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&old), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&new), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = call(&s, Method::GET, "/api/v1/machines", Some(&new), None).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("forbidden_scope"))
    );
    // only an administrator rotates, and only a token that exists
    let (st, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/tokens/{id}/rotate"),
        Some(&new),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tokens/nope/rotate",
        Some(&a),
        None,
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_token"))
    );
    // revocation
    call(
        &s,
        Method::DELETE,
        &format!("/api/v1/tokens/{id}"),
        Some(&a),
        None,
    )
    .await;
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&new), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // expiry: a token that lasts a day does not work two days on
    let secret = {
        let mut auth = s.state.auth.lock().unwrap();
        let (_, secret) = auth
            .create_token("short", eh3d_server::auth::Role::Operator, 1_000, Some(1))
            .unwrap();
        assert!(auth.identify(&secret, 1_000 + 3_600).is_some());
        assert!(auth.identify(&secret, 1_000 + 2 * 86_400).is_none());
        secret
    };
    let (st, _) = call(&s, Method::GET, "/api/v1/tasks", Some(&secret), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "it is long past its day");
}

#[tokio::test]
async fn the_tasks_of_a_shared_project_keep_their_owner() {
    let s = server("task-shared");
    let a = admin(&s).await;
    let (_, ana) = token(&s, &a, "ana", "operator").await;
    let (bob_id, bob) = token(&s, &a, "bob", "operator").await;
    let art = put(&s, &ana, "v1", b"project").await;
    let (_, p) = call(
        &s,
        Method::POST,
        "/api/v1/projects",
        Some(&ana),
        Some(json!({"name": "x"})),
    )
    .await;
    let pid = p["id"].as_str().unwrap();
    call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{pid}/revisions"),
        Some(&ana),
        Some(json!({"artifact_id": art["artifact_id"], "parent": 0})),
    )
    .await;
    let task = json!({"kind": "server.selftest", "project": art["artifact_id"], "params": {"steps": 1, "step_ms": 1}});
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&bob),
        Some(task.clone()),
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("no_project"))
    );
    call(
        &s,
        Method::POST,
        &format!("/api/v1/projects/{pid}/shares"),
        Some(&ana),
        Some(json!({"token": bob_id, "access": "read"})),
    )
    .await;
    let (st, mine) = call(&s, Method::POST, "/api/v1/tasks", Some(&bob), Some(task)).await;
    assert_eq!(st, StatusCode::CREATED, "{mine}");
    // the task is bob's: ana, who owns the project, does not see it; the administrator does
    let path = format!("/api/v1/tasks/{}", mine["id"].as_str().unwrap());
    let (st, _) = call(&s, Method::GET, &path, Some(&ana), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&s, Method::GET, &path, Some(&a), None).await;
    assert_eq!(st, StatusCode::OK);
}
