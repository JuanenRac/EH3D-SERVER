//! The artifacts, the blobs and the uploads end to end through the real router: two identities (the bytes and the record), who sees what, the check of the hash, the limits,
//! and the tasks that work on an artifact and make new ones.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        std::env::temp_dir().join(format!("eh3d-server-art-api-{name}-{}", std::process::id()));
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
    body: Option<Vec<u8>>,
    json_body: bool,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        request = request.header("authorization", format!("Bearer {t}"));
    }
    if json_body {
        request = request.header("content-type", "application/json");
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
    (status, headers, bytes)
}

async fn call(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = raw(
        s,
        method,
        path,
        token,
        body.map(|b| b.to_string().into_bytes()),
        true,
    )
    .await;
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

async fn token(s: &Server, admin: &str, name: &str, role: &str) -> String {
    let (_, body) = call(
        s,
        Method::POST,
        "/api/v1/tokens",
        Some(admin),
        Some(json!({"name": name, "role": role})),
    )
    .await;
    body["token"].as_str().unwrap().to_string()
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn put(s: &Server, t: &str, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/artifacts",
        Some(t),
        Some(json!({"name": name, "format": "text/plain", "data_base64": b64(bytes)})),
    )
    .await
}

async fn wait_task(s: &Server, t: &str, id: &str) -> Value {
    let limit = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, b) = call(
            s,
            Method::GET,
            &format!("/api/v1/tasks/{id}"),
            Some(t),
            None,
        )
        .await;
        if matches!(b["state"].as_str(), Some("done" | "failed" | "cancelled")) {
            return b;
        }
        assert!(Instant::now() < limit, "{b}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn owner_id(s: &Server, name: &str) -> String {
    let auth = s.state.auth.lock().unwrap();
    auth.tokens()
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
        .clone()
}

#[tokio::test]
async fn an_artifact_has_its_own_id_and_the_hash_of_its_bytes_and_comes_back_whole() {
    let s = server("basic");
    let a = admin(&s).await;
    let bytes = b"the bytes of a design \x00\x01\xff";
    let (st, made) = put(&s, &a, "board.phoenixproj", bytes).await;
    assert_eq!(st, StatusCode::CREATED, "{made}");
    assert!(eh3d_server::ids::is_valid(
        eh3d_server::ids::Kind::Artifact,
        made["artifact_id"].as_str().unwrap()
    ));
    assert_eq!(made["blob_sha256"], sha(bytes));
    assert_eq!(made["size"], bytes.len());
    let id = made["artifact_id"].as_str().unwrap();
    let (st, headers, got) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        Some(&a),
        None,
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(got, bytes);
    assert_eq!(headers["x-eh3d-blob-sha256"], sha(bytes).as_str());
    let (_, meta) = call(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}/meta"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(meta["owner"], "juan");
    assert_eq!(meta["retention"], "keep_while_referenced");
    // the states of validation are kept apart: only "intact" is known
    assert_eq!(
        meta["validation"],
        json!({"intact": true, "readable": null, "coherent": null, "erc_drc_run": null, "oracle_checked": null, "physically_validated": null})
    );
    assert!(meta["audit"]["created_at"].is_u64());
}

#[tokio::test]
async fn the_same_bytes_made_twice_are_two_artifacts_with_one_blob() {
    let s = server("two");
    let a = admin(&s).await;
    let (_, one) = put(&s, &a, "a.txt", b"same bytes").await;
    let (_, two) = call(
        &s,
        Method::POST,
        "/api/v1/artifacts",
        Some(&a),
        Some(json!({"name": "b.txt", "format": "text/plain", "params": {"ruleset": "other"}, "producer": {"tool": "t", "version": "1"}, "data_base64": b64(b"same bytes")})),
    )
    .await;
    assert_ne!(one["artifact_id"], two["artifact_id"]);
    assert_eq!(one["blob_sha256"], two["blob_sha256"]);
    // one copy on the disk
    let blobs = s.dir.join("blobs");
    let count = std::fs::read_dir(&blobs)
        .unwrap()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path()).unwrap().flatten())
        .count();
    assert_eq!(count, 1);
    // the provenance is the record's, not the blob's
    let (_, meta) = call(
        &s,
        Method::GET,
        &format!(
            "/api/v1/artifacts/{}/meta",
            two["artifact_id"].as_str().unwrap()
        ),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(meta["producer"]["tool"], "t");
    assert_eq!(meta["params"], json!({"ruleset": "other"}));
}

#[tokio::test]
async fn the_artifacts_of_another_owner_do_not_exist_and_the_blob_question_is_not_an_oracle() {
    let s = server("privacy");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let bob = token(&s, &a, "bob", "operator").await;
    let secret = b"only ana has this file";
    let (_, made) = put(&s, &ana, "mine.txt", secret).await;
    let id = made["artifact_id"].as_str().unwrap().to_string();
    for (method, path) in [
        (Method::GET, format!("/api/v1/artifacts/{id}")),
        (Method::GET, format!("/api/v1/artifacts/{id}/meta")),
    ] {
        let (st, b) = call(&s, method, &path, Some(&bob), None).await;
        assert_eq!(
            (st, b["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_artifact")),
            "{path}"
        );
    }
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "only an administrator deletes");
    let (_, list) = call(&s, Method::GET, "/api/v1/artifacts", Some(&bob), None).await;
    assert_eq!(list, json!([]));
    // HEAD /blobs: yes for the owner, no for the other, and the other's no is the same as for a hash nobody holds
    let head = |t: &str, hash: String| {
        let t = t.to_string();
        let s = &s;
        async move {
            raw(
                s,
                Method::HEAD,
                &format!("/api/v1/blobs/{hash}"),
                Some(&t),
                None,
                false,
            )
            .await
            .0
        }
    };
    assert_eq!(head(&ana, sha(secret)).await, StatusCode::OK);
    assert_eq!(head(&bob, sha(secret)).await, StatusCode::NOT_FOUND);
    assert_eq!(
        head(&bob, sha(b"nobody has this")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(head(&bob, "not a hash".into()).await, StatusCode::NOT_FOUND);
    assert_eq!(
        head(&a, sha(secret)).await,
        StatusCode::OK,
        "the administrator sees all"
    );
    // the deduplication is invisible: bob sends the same bytes, gets an artifact of his own, and now he holds the blob too
    let (st, mine) = put(&s, &bob, "his.txt", secret).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_ne!(mine["artifact_id"], made["artifact_id"]);
    assert_eq!(mine["blob_sha256"], made["blob_sha256"]);
    assert_eq!(head(&bob, sha(secret)).await, StatusCode::OK);
    // a viewer reads and cannot make
    let viewer = token(&s, &a, "view", "viewer").await;
    let (st, b) = put(&s, &viewer, "x.txt", b"x").await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (st, _, _) = raw(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        None,
        None,
        false,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bad_descriptions_and_inputs_are_refused() {
    let s = server("bad");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let bob = token(&s, &a, "bob", "operator").await;
    for body in [
        json!({"name": "", "format": "text/plain", "data_base64": ""}),
        json!({"name": "a/b", "format": "text/plain", "data_base64": ""}),
        json!({"name": "x", "format": "Text/Plain", "data_base64": ""}),
        json!({"name": "x", "format": "text/plain", "result_type": "T9", "data_base64": ""}),
        json!({"name": "x", "format": "text/plain", "data_base64": "***not base64***"}),
        json!({"name": "x", "format": "text/plain", "inputs": [{"artifact_id": "a-00000000-0000-7000-8000-000000000000", "blob_sha256": "0".repeat(64)}], "data_base64": ""}),
    ] {
        let (st, b) = call(
            &s,
            Method::POST,
            "/api/v1/artifacts",
            Some(&ana),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            (st, b["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_artifact")),
            "{body}"
        );
    }
    // an input has to be an artifact the caller sees, with the hash it carries
    let (_, theirs) = put(&s, &bob, "bobs.txt", b"bob").await;
    let input = json!({"artifact_id": theirs["artifact_id"], "blob_sha256": theirs["blob_sha256"]});
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/artifacts",
        Some(&ana),
        Some(
            json!({"name": "x", "format": "text/plain", "inputs": [input.clone()], "data_base64": ""}),
        ),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_artifact"))
    );
    let (st, _) = call(
        &s,
        Method::POST,
        "/api/v1/artifacts",
        Some(&bob),
        Some(json!({"name": "x", "format": "text/plain", "inputs": [input], "data_base64": ""})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
}

#[tokio::test]
async fn an_upload_in_parts_makes_the_artifact_only_when_all_is_there_and_the_hash_is_right() {
    let s = server("upload");
    let a = admin(&s).await;
    let whole: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let (st, up) = call(
        &s,
        Method::POST,
        "/api/v1/uploads",
        Some(&a),
        Some(json!({"size": whole.len(), "sha256": sha(&whole)})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let id = up["id"].as_str().unwrap().to_string();
    assert!(eh3d_server::ids::is_valid(
        eh3d_server::ids::Kind::Upload,
        &id
    ));
    let send = |from: usize, to: usize| {
        let path = format!("/api/v1/uploads/{id}?offset={from}");
        let bytes = whole[from..to].to_vec();
        let (s, a) = (&s, a.clone());
        async move { raw(s, Method::PUT, &path, Some(&a), Some(bytes), false).await }
    };
    // the end first, then a gap: it cannot be completed
    let (st, _, body) = send(3000, 5000).await;
    assert_eq!(st, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["received"], 2000);
    let meta = json!({"name": "big.bin", "format": "application/octet-stream"});
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/uploads/{id}/complete"),
        Some(&a),
        Some(meta.clone()),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("upload_incomplete"))
    );
    // the same part again is fine, different bytes at the same place are not
    assert_eq!(send(3000, 5000).await.0, StatusCode::OK);
    let (st, _, body) = raw(
        &s,
        Method::PUT,
        &format!("/api/v1/uploads/{id}?offset=3000"),
        Some(&a),
        Some(vec![9u8; 2000]),
        false,
    )
    .await;
    let b: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("upload_conflict"))
    );
    // past the declared size
    let (st, _, _) = raw(
        &s,
        Method::PUT,
        &format!("/api/v1/uploads/{id}?offset=4900"),
        Some(&a),
        Some(vec![0u8; 200]),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(send(0, 3000).await.0, StatusCode::OK);
    let (st, made) = call(
        &s,
        Method::POST,
        &format!("/api/v1/uploads/{id}/complete"),
        Some(&a),
        Some(meta),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{made}");
    assert_eq!(made["blob_sha256"], sha(&whole));
    let (_, _, got) = raw(
        &s,
        Method::GET,
        &format!(
            "/api/v1/artifacts/{}",
            made["artifact_id"].as_str().unwrap()
        ),
        Some(&a),
        None,
        false,
    )
    .await;
    assert_eq!(got, whole);
    // the upload is gone
    let (st, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/uploads/{id}/complete"),
        Some(&a),
        Some(json!({"name": "x", "format": "a/b"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_wrong_hash_or_somebody_elses_upload_gives_nothing() {
    let s = server("upload-bad");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let bob = token(&s, &a, "bob", "operator").await;
    let bytes = b"real bytes".to_vec();
    let (_, up) = call(
        &s,
        Method::POST,
        "/api/v1/uploads",
        Some(&ana),
        Some(json!({"size": bytes.len(), "sha256": sha(b"other bytes")})),
    )
    .await;
    let id = up["id"].as_str().unwrap().to_string();
    // another user cannot put a part, finish or throw it away: for him it does not exist
    let (st, _, _) = raw(
        &s,
        Method::PUT,
        &format!("/api/v1/uploads/{id}?offset=0"),
        Some(&bob),
        Some(bytes.clone()),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let complete = json!({"name": "x", "format": "a/b"});
    let (st, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/uploads/{id}/complete"),
        Some(&bob),
        Some(complete.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/uploads/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = raw(
        &s,
        Method::PUT,
        &format!("/api/v1/uploads/{id}?offset=0"),
        Some(&ana),
        Some(bytes.clone()),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = call(
        &s,
        Method::POST,
        &format!("/api/v1/uploads/{id}/complete"),
        Some(&ana),
        Some(complete.clone()),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("hash_mismatch"))
    );
    // nothing was kept: no artifact, no blob, and the upload is gone
    let (_, list) = call(&s, Method::GET, "/api/v1/artifacts", Some(&a), None).await;
    assert_eq!(list, json!([]));
    assert_eq!(
        raw(
            &s,
            Method::HEAD,
            &format!("/api/v1/blobs/{}", sha(&bytes)),
            Some(&a),
            None,
            false
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (st, _) = call(
        &s,
        Method::POST,
        &format!("/api/v1/uploads/{id}/complete"),
        Some(&ana),
        Some(complete),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // a bad declaration
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/uploads",
        Some(&ana),
        Some(json!({"size": 4, "sha256": "ABC"})),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_upload"))
    );
}

#[tokio::test]
async fn the_limits_of_the_store_and_of_the_uploads_hold() {
    let s = server_with("limits", |c| {
        c.max_upload_mb = 1;
        c.uploads_per_user = 1;
        c.artifact_store_gb = 1;
    });
    let a = admin(&s).await;
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/uploads",
        Some(&a),
        Some(json!({"size": 2 * 1024 * 1024, "sha256": sha(b"x")})),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("too_big"))
    );
    let (st, _) = call(
        &s,
        Method::POST,
        "/api/v1/uploads",
        Some(&a),
        Some(json!({"size": 10, "sha256": sha(b"x")})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/uploads",
        Some(&a),
        Some(json!({"size": 10, "sha256": sha(b"y")})),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("quota"))
    );
    // an artifact over the largest one
    let big = vec![0u8; 1024 * 1024 + 1];
    let (st, b) = put(&s, &a, "big", &big).await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("too_big"))
    );
}

#[tokio::test]
async fn an_administrator_deletes_and_the_blob_goes_with_the_last_artifact() {
    let s = server("delete");
    let a = admin(&s).await;
    let (_, one) = put(&s, &a, "a", b"shared").await;
    let (_, two) = put(&s, &a, "b", b"shared").await;
    let present = || async {
        let path = format!("/api/v1/blobs/{}", sha(b"shared"));
        raw(&s, Method::HEAD, &path, Some(&a), None, false).await
    };
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{}", one["artifact_id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        present().await.0,
        StatusCode::OK,
        "the other still points to the bytes"
    );
    call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{}", two["artifact_id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(present().await.0, StatusCode::NOT_FOUND);
    let (st, b) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{}", two["artifact_id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_artifact"))
    );
}

#[tokio::test]
async fn retention_is_set_by_an_administrator_and_verify_finds_a_damaged_blob() {
    let s = server("retention");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let (_, made) = put(&s, &ana, "a", b"keep me").await;
    let id = made["artifact_id"].as_str().unwrap();
    let path = format!("/api/v1/artifacts/{id}/retention");
    let (st, _) = call(
        &s,
        Method::PUT,
        &path,
        Some(&ana),
        Some(json!({"retention": "pinned"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &s,
        Method::PUT,
        &path,
        Some(&a),
        Some(json!({"retention": "keep_until"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNPROCESSABLE_ENTITY,
        "keep_until needs a date"
    );
    let (st, b) = call(
        &s,
        Method::PUT,
        &path,
        Some(&a),
        Some(json!({"retention": "pinned"})),
    )
    .await;
    assert_eq!(
        (st, b["retention"].clone()),
        (StatusCode::OK, json!("pinned"))
    );
    let (st, b) = call(
        &s,
        Method::PUT,
        &path,
        Some(&a),
        Some(json!({"retention": "keep_until", "until": 4102444800u64})),
    )
    .await;
    assert_eq!(
        (st, b["keep_until"].clone()),
        (StatusCode::OK, json!(4102444800u64))
    );
    // the check of the store
    let (_, ok) = call(&s, Method::POST, "/api/v1/artifacts/verify", Some(&a), None).await;
    assert_eq!(ok["ok"], true);
    let blob = s
        .dir
        .join("blobs")
        .join(&sha(b"keep me")[..2])
        .join(sha(b"keep me"));
    std::fs::write(&blob, b"damaged").unwrap();
    let (_, bad) = call(&s, Method::POST, "/api/v1/artifacts/verify", Some(&a), None).await;
    assert_eq!(bad["ok"], false);
    assert_eq!(
        bad["problems"][0],
        json!({"blob_sha256": sha(b"keep me"), "problem": "damaged"})
    );
    let (st, _) = call(
        &s,
        Method::POST,
        "/api/v1/artifacts/verify",
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_task_works_on_an_artifact_and_its_files_become_artifacts_with_their_provenance() {
    let s = server("task");
    let a = admin(&s).await;
    let ana = token(&s, &a, "ana", "operator").await;
    let bob = token(&s, &a, "bob", "operator").await;
    let (_, project) = put(&s, &ana, "board.phoenixproj", b"PROJECT BYTES").await;
    let pid = project["artifact_id"].as_str().unwrap();
    let task = |p: &str, emit: &str| json!({"kind": "server.selftest", "project": p, "params": {"steps": 1, "step_ms": 1, "emit": {"name": "report.txt", "text": emit}}});
    // somebody else's artifact is not a project for you
    let (st, b) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&bob),
        Some(task(pid, "x")),
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("no_project"))
    );
    // two tasks on the same project with the same output bytes but other parameters
    let (_, t1) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&ana),
        Some(task(pid, "same output")),
    )
    .await;
    let mut other = task(pid, "same output");
    other["params"]["steps"] = json!(2);
    let (_, t2) = call(&s, Method::POST, "/api/v1/tasks", Some(&ana), Some(other)).await;
    let d1 = wait_task(&s, &ana, t1["id"].as_str().unwrap()).await;
    let d2 = wait_task(&s, &ana, t2["id"].as_str().unwrap()).await;
    assert_eq!(
        (d1["state"].clone(), d2["state"].clone()),
        (json!("done"), json!("done")),
        "{d1} {d2}"
    );
    assert_eq!(
        d1["result"]["data"]["project_sha256"],
        sha(b"PROJECT BYTES")
    );
    let (a1, a2) = (&d1["result"]["artifacts"][0], &d2["result"]["artifacts"][0]);
    assert_ne!(a1["artifact_id"], a2["artifact_id"], "two records");
    assert_eq!(a1["kind"], "report");
    assert_eq!(a1["result_type"], "T4");
    let meta = |id: &Value| {
        let path = format!("/api/v1/artifacts/{}/meta", id.as_str().unwrap());
        let (s, ana) = (&s, ana.clone());
        async move { call(s, Method::GET, &path, Some(&ana), None).await.1 }
    };
    let (m1, m2) = (
        meta(&a1["artifact_id"]).await,
        meta(&a2["artifact_id"]).await,
    );
    assert_eq!(m1["blob_sha256"], m2["blob_sha256"], "one blob");
    assert_eq!(m1["blob_sha256"], sha(b"same output"));
    assert_ne!(m1["params"], m2["params"]);
    assert_ne!(
        m1["producer"]["config_sha256"],
        m2["producer"]["config_sha256"]
    );
    assert_eq!(m1["owner"], format!("token:{}", owner_id(&s, "ana")));
    assert_eq!(m1["producer"]["tool"], "server.selftest");
    assert_eq!(
        m1["inputs"],
        json!([{"artifact_id": pid, "blob_sha256": project["blob_sha256"]}])
    );
    assert_eq!(m1["origin"]["project"], pid);
    // the artifacts a task works on or made cannot be deleted while the task is there
    let (st, b) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{pid}"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("artifact_in_use"))
    );
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{}", a1["artifact_id"].as_str().unwrap()),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // once the tasks are removed they can
    for t in [&t1, &t2] {
        call(
            &s,
            Method::DELETE,
            &format!("/api/v1/tasks/{}", t["id"].as_str().unwrap()),
            Some(&ana),
            None,
        )
        .await;
    }
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/artifacts/{pid}"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "nothing points to it any more");
}

#[tokio::test]
async fn a_cancelled_task_leaves_no_artifact() {
    let s = server("cancel-art");
    let a = admin(&s).await;
    let (_, t) = call(
        &s,
        Method::POST,
        "/api/v1/tasks",
        Some(&a),
        Some(json!({"kind": "server.selftest", "params": {"steps": 1000, "step_ms": 100, "emit": {"name": "r.txt", "text": "never"}}})),
    )
    .await;
    let id = t["id"].as_str().unwrap();
    let limit = Instant::now() + Duration::from_secs(10);
    while call(
        &s,
        Method::GET,
        &format!("/api/v1/tasks/{id}"),
        Some(&a),
        None,
    )
    .await
    .1["state"]
        != "running"
    {
        assert!(Instant::now() < limit);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    call(
        &s,
        Method::POST,
        &format!("/api/v1/tasks/{id}/cancel"),
        Some(&a),
        None,
    )
    .await;
    let done = wait_task(&s, &a, id).await;
    assert_eq!(done["state"], "cancelled");
    let (_, list) = call(&s, Method::GET, "/api/v1/artifacts", Some(&a), None).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn the_artifacts_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!(
        "eh3d-server-art-api-restart-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    let id = {
        let state = AppState::open(cfg.clone()).unwrap();
        let s = Server {
            app: router(state.clone()),
            state,
            dir: dir.clone(),
        };
        let a = admin(&s).await;
        let (_, made) = put(&s, &a, "keep.txt", b"kept").await;
        let id = made["artifact_id"].as_str().unwrap().to_string();
        std::mem::forget(s);
        id
    };
    let state = AppState::open(cfg).unwrap();
    let store = state.artifacts.store.lock().unwrap();
    let r = store.get(&id).unwrap();
    assert_eq!(r.blob_sha256, sha(b"kept"));
    assert_eq!(store.read_blob(&r.blob_sha256).unwrap(), b"kept");
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
