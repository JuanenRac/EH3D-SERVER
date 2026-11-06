//! The project store end to end through the real router: the projects and the files of an application, the versions and `If-Match`, the locks, the names that are refused,
//! the limits, the check of the bytes on the disk and the `.eh3dproj` container.

mod common;

use std::io::Cursor;

use axum::http::{Method, StatusCode};
use common::{admin, call, enc, scoped_token, send, server, server_with, user, Server};
use eh3d_container::{AtomicOptions, Container, ContainerSpec, ContainerWriter};
use serde_json::json;
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn make(s: &Server, token: &str, app: &str, name: &str) -> (String, String) {
    let r = send(
        s,
        Method::POST,
        &format!("/api/v1/apps/{app}/projects"),
        Some(token),
        &[],
        Some(json!({ "name": name }).to_string().into_bytes()),
        Some("application/json"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json());
    (
        r.json()["id"].as_str().unwrap().to_string(),
        r.header("etag"),
    )
}

fn file_url(app: &str, id: &str, name: &str) -> String {
    format!(
        "/api/v1/apps/{app}/projects/{id}/files/content?name={}",
        enc(name)
    )
}

async fn put(
    s: &Server,
    token: &str,
    url: &str,
    bytes: &[u8],
    if_match: Option<&str>,
) -> common::Reply {
    let mut headers = Vec::new();
    if let Some(tag) = if_match {
        headers.push(("if-match", tag));
    }
    send(
        s,
        Method::PUT,
        url,
        Some(token),
        &headers,
        Some(bytes.to_vec()),
        Some("application/octet-stream"),
    )
    .await
}

#[tokio::test]
async fn a_project_is_made_listed_read_and_renamed_with_its_etag() {
    let s = server("store-project");
    let boss = admin(&s).await;
    let (id, etag) = make(&s, &boss, "studio", "front panel").await;
    let (status, list) = call(
        &s,
        Method::GET,
        "/api/v1/apps/studio/projects",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["name"], "front panel");
    // the search keeps the names that have the text, in any letter case
    let (_, found) = call(
        &s,
        Method::GET,
        "/api/v1/apps/studio/projects?q=PANEL",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(found["total"], 1);
    let (_, none) = call(
        &s,
        Method::GET,
        "/api/v1/apps/studio/projects?q=zzz",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(none["total"], 0);
    let url = format!("/api/v1/apps/studio/projects/{id}");
    let got = send(&s, Method::GET, &url, Some(&boss), &[], None, None).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.header("etag"), etag);
    // a rename needs the tag, and the right one
    let body = Some(json!({"name": "front panel v2"}).to_string().into_bytes());
    let json_type = Some("application/json");
    let missing = send(
        &s,
        Method::PUT,
        &url,
        Some(&boss),
        &[],
        body.clone(),
        json_type,
    )
    .await;
    assert_eq!(missing.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(missing.error(), "precondition_required");
    let wrong = send(
        &s,
        Method::PUT,
        &url,
        Some(&boss),
        &[("if-match", "\"99\"")],
        body.clone(),
        json_type,
    )
    .await;
    assert_eq!(wrong.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(wrong.error(), "precondition_failed");
    let renamed = send(
        &s,
        Method::PUT,
        &url,
        Some(&boss),
        &[("if-match", etag.as_str())],
        body,
        json_type,
    )
    .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.json());
    assert_eq!(renamed.json()["name"], "front panel v2");
    assert_ne!(renamed.header("etag"), etag);
}

#[tokio::test]
async fn a_file_is_never_overwritten_every_write_is_a_version() {
    let s = server("store-versions");
    let boss = admin(&s).await;
    let (id, _) = make(&s, &boss, "studio", "board").await;
    let url = file_url("studio", &id, "layers/top.gbr");
    let first = put(&s, &boss, &url, b"one", None).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json());
    assert_eq!(first.json()["version"], 1);
    assert_eq!(first.json()["sha256"], sha(b"one"));
    assert_eq!(first.header("etag"), format!("\"{}\"", sha(b"one")));
    // over a file that exists the tag is needed
    let blind = put(&s, &boss, &url, b"two", None).await;
    assert_eq!(blind.status, StatusCode::PRECONDITION_REQUIRED);
    let stale = put(&s, &boss, &url, b"two", Some("\"deadbeef\"")).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let tag = first.header("etag");
    let second = put(&s, &boss, &url, b"two", Some(tag.as_str())).await;
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(second.json()["version"], 2);
    // the same bytes again make no version
    let tag = second.header("etag");
    let same = put(&s, &boss, &url, b"two", Some(tag.as_str())).await;
    assert_eq!(same.status, StatusCode::OK);
    assert_eq!(same.json()["unchanged"], true);
    // the current bytes, an old version and the list of versions
    let now = send(&s, Method::GET, &url, Some(&boss), &[], None, None).await;
    assert_eq!(now.bytes, b"two");
    assert_eq!(now.header("x-eh3d-version"), "2");
    let old = send(
        &s,
        Method::GET,
        &format!("{url}&version=1"),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(old.bytes, b"one");
    let missing = send(
        &s,
        Method::GET,
        &format!("{url}&version=9"),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(missing.error(), "unknown_version");
    let versions = send(
        &s,
        Method::GET,
        &format!(
            "/api/v1/apps/studio/projects/{id}/files/versions?name={}",
            enc("layers/top.gbr")
        ),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await
    .json();
    assert_eq!(versions["versions"][0]["version"], 2);
    assert_eq!(versions["versions"][1]["version"], 1);
    // removing a file takes it off the list and keeps its versions; writing it again brings it back
    let removed = send(
        &s,
        Method::DELETE,
        &url,
        Some(&boss),
        &[("if-match", tag.as_str())],
        None,
        None,
    )
    .await;
    assert_eq!(removed.status, StatusCode::OK);
    let (_, files) = call(
        &s,
        Method::GET,
        &format!("/api/v1/apps/studio/projects/{id}/files"),
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(files["files"].as_array().unwrap().len(), 0);
    let back = put(&s, &boss, &url, b"three", None).await;
    assert_eq!(back.status, StatusCode::CREATED);
    assert_eq!(back.json()["version"], 3);
    let unknown = send(
        &s,
        Method::GET,
        &file_url("studio", &id, "nope.txt"),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(unknown.error(), "unknown_file");
}

#[tokio::test]
async fn names_that_leave_the_folder_or_belong_to_windows_are_refused() {
    let s = server("store-names");
    let boss = admin(&s).await;
    let (id, _) = make(&s, &boss, "studio", "board").await;
    for bad in [
        "../secret",
        "a/../../b",
        "/etc/passwd",
        "C:\\x.txt",
        "layers\\top.gbr",
        "CON",
        "layers/nul.txt",
        "name.",
        "integrity.json",
        "a//b",
    ] {
        let r = put(&s, &boss, &file_url("studio", &id, bad), b"x", None).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_eq!(r.error(), "bad_filename", "{bad}");
    }
    // a name that differs only in letter case is the same file for Windows
    assert_eq!(
        put(
            &s,
            &boss,
            &file_url("studio", &id, "Layers/Top.gbr"),
            b"x",
            None
        )
        .await
        .status,
        StatusCode::CREATED
    );
    let clash = put(
        &s,
        &boss,
        &file_url("studio", &id, "layers/top.gbr"),
        b"x",
        None,
    )
    .await;
    assert_eq!(clash.error(), "bad_filename");
    // no name at all
    let none = put(
        &s,
        &boss,
        &format!("/api/v1/apps/studio/projects/{id}/files/content"),
        b"x",
        None,
    )
    .await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);
    assert_eq!(none.error(), "bad_request");
}

#[tokio::test]
async fn the_limits_come_from_the_settings() {
    let s = server_with("store-limits", |c| {
        c.project_max_file_mb = 1;
        c.project_max_files = 2;
    });
    let boss = admin(&s).await;
    let (id, _) = make(&s, &boss, "studio", "board").await;
    let big = vec![7u8; 1024 * 1024 + 1];
    let r = put(&s, &boss, &file_url("studio", &id, "big.bin"), &big, None).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.json());
    assert_eq!(r.error(), "file_too_big");
    for name in ["a.txt", "b.txt"] {
        assert_eq!(
            put(&s, &boss, &file_url("studio", &id, name), b"x", None)
                .await
                .status,
            StatusCode::CREATED
        );
    }
    let third = put(&s, &boss, &file_url("studio", &id, "c.txt"), b"y", None).await;
    assert_eq!(third.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(third.error(), "project_quota");
}

#[tokio::test]
async fn bytes_that_changed_on_the_disk_are_not_given_back() {
    let s = server("store-damage");
    let boss = admin(&s).await;
    let (id, _) = make(&s, &boss, "studio", "board").await;
    let url = file_url("studio", &id, "a.txt");
    assert_eq!(
        put(&s, &boss, &url, b"hello", None).await.status,
        StatusCode::CREATED
    );
    let blob = s
        .dir
        .join("pstore")
        .join("apps")
        .join("studio")
        .join(&id)
        .join("blobs")
        .join(sha(b"hello"));
    assert!(blob.is_file());
    std::fs::write(&blob, b"hullo").unwrap();
    let r = send(&s, Method::GET, &url, Some(&boss), &[], None, None).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(r.error(), "blob_damaged");
}

#[tokio::test]
async fn a_lock_keeps_the_others_out_and_the_roles_and_the_owner_decide_who_sees() {
    let s = server_with("store-lock", |c| c.project_shared = true);
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana1", "operator").await;
    let bob = user(&s, &boss, "bob1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let (id, _) = make(&s, &ana, "studio", "shared board").await;
    let lock_url = format!("/api/v1/apps/studio/projects/{id}/lock");
    let (status, held) = call(&s, Method::PUT, &lock_url, Some(&ana), None).await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(held["lock"]["holder"], "ana1");
    let (status, refused) = call(&s, Method::PUT, &lock_url, Some(&bob), None).await;
    assert_eq!(status.as_u16(), 423);
    assert_eq!(refused["error"], "locked");
    let blocked = put(&s, &bob, &file_url("studio", &id, "a.txt"), b"x", None).await;
    assert_eq!(blocked.status.as_u16(), 423);
    // the lock is visible to everybody who sees the project
    let (_, seen) = call(&s, Method::GET, &lock_url, Some(&viewer), None).await;
    assert_eq!(seen["lock"]["holder"], "ana1");
    // a viewer reads and nothing else
    let denied = put(&s, &viewer, &file_url("studio", &id, "a.txt"), b"x", None).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    // bob cannot let it go, the administrator can with force, and then bob writes
    let (status, _) = call(&s, Method::DELETE, &lock_url, Some(&bob), None).await;
    assert_eq!(status.as_u16(), 423);
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("{lock_url}?force=true"),
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, again) = call(&s, Method::DELETE, &lock_url, Some(&ana), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["error"], "not_locked");
    let written = put(&s, &bob, &file_url("studio", &id, "a.txt"), b"x", None).await;
    assert_eq!(written.status, StatusCode::CREATED);
    // only the owner and the administrators hide a project
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/apps/studio/projects/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn without_the_team_mode_a_project_is_its_owners_and_the_administrators() {
    let s = server("store-owner");
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana1", "operator").await;
    let bob = user(&s, &boss, "bob1", "operator").await;
    let (id, _) = make(&s, &ana, "studio", "mine").await;
    let url = format!("/api/v1/apps/studio/projects/{id}");
    let (status, body) = call(&s, Method::GET, &url, Some(&bob), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "unknown_project");
    let (status, _) = call(&s, Method::GET, &url, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    // the same id under another application does not exist
    let (status, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/apps/android/projects/{id}"),
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = call(
        &s,
        Method::GET,
        "/api/v1/apps/studio/projects",
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(list["total"], 0);
    // hidden: the owner no longer sees it, the administrator purges it
    let (status, _) = call(&s, Method::DELETE, &url, Some(&ana), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&s, Method::GET, &url, Some(&ana), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&s, Method::POST, &format!("{url}/purge"), Some(&ana), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&s, Method::POST, &format!("{url}/purge"), Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&s, Method::GET, &url, Some(&boss), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_application_has_to_be_one_the_server_knows_and_the_scopes_narrow_a_token() {
    let s = server("store-apps");
    let boss = admin(&s).await;
    for app in ["My-Tool", "my-tool", "..", "a%20b"] {
        let (status, body) = call(
            &s,
            Method::GET,
            &format!("/api/v1/apps/{app}/projects"),
            Some(&boss),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{app}");
        assert_eq!(body["error"], "bad_app", "{app}");
    }
    let reader = scoped_token(&s, &boss, "operator", &["projects:read"]).await;
    let (status, _) = call(
        &s,
        Method::GET,
        "/api/v1/apps/studio/projects",
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/apps/studio/projects",
        Some(&reader),
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden_scope");
    let (status, _) = call(&s, Method::GET, "/api/v1/apps/studio/projects", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

fn package(entries: &[(&str, &[u8])], dir: &std::path::Path) -> Vec<u8> {
    std::fs::create_dir_all(dir).unwrap();
    let mut writer = ContainerWriter::new(&ContainerSpec::eh3dproj_v1(), dir);
    for (name, bytes) in entries {
        writer.add(name, bytes).unwrap();
    }
    let path = dir.join("test.eh3dproj");
    writer
        .write_to_path(&path, &AtomicOptions::default())
        .unwrap();
    std::fs::read(&path).unwrap()
}

#[tokio::test]
async fn a_project_goes_in_and_out_as_an_eh3dproj_container() {
    let s = server("store-container");
    let boss = admin(&s).await;
    let (id, _) = make(&s, &boss, "studio", "board").await;
    let work = s.dir.join("container-work");
    let bytes = package(
        &[
            ("project.json", &br#"{"name":"board"}"#[..]),
            ("layers/top.gbr", &b"G04 top*"[..]),
            ("vision/a.json", &b"{}"[..]),
        ],
        &work,
    );
    let url = format!("/api/v1/apps/studio/projects/{id}");
    let imported = send(
        &s,
        Method::POST,
        &format!("{url}/import"),
        Some(&boss),
        &[],
        Some(bytes.clone()),
        Some("application/octet-stream"),
    )
    .await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.json());
    assert_eq!(imported.json()["imported"], 3);
    let (_, files) = call(&s, Method::GET, &format!("{url}/files"), Some(&boss), None).await;
    let names: Vec<&str> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["layers/top.gbr", "project.json", "vision/a.json"]
    );
    // the same container again changes nothing
    let again = send(
        &s,
        Method::POST,
        &format!("{url}/import"),
        Some(&boss),
        &[],
        Some(bytes),
        Some("application/octet-stream"),
    )
    .await;
    assert_eq!(again.json()["imported"], 0);
    assert_eq!(again.json()["unchanged"], 3);
    // out: a container that the library of the format reads and checks
    let out = send(
        &s,
        Method::GET,
        &format!("{url}/export"),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.json());
    assert_eq!(out.header("content-type"), "application/zip");
    let mut container = Container::open(
        Cursor::new(out.bytes.clone()),
        &ContainerSpec::eh3dproj_v1(),
    )
    .unwrap();
    container.check_layout().unwrap();
    container.verify_integrity().unwrap();
    assert_eq!(
        container.read_limited("layers/top.gbr", 1024).unwrap(),
        b"G04 top*"
    );
    // a container that is not one, or one with a damaged entry, is refused as a whole
    let junk = send(
        &s,
        Method::POST,
        &format!("{url}/import"),
        Some(&boss),
        &[],
        Some(b"this is not a zip".to_vec()),
        Some("application/octet-stream"),
    )
    .await;
    assert_eq!(junk.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(junk.error(), "bad_container");
    // a project without project.json cannot be exported
    let (empty, _) = make(&s, &boss, "studio", "empty").await;
    let none = send(
        &s,
        Method::GET,
        &format!("/api/v1/apps/studio/projects/{empty}/export"),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(none.error(), "bad_container");
}

#[tokio::test]
async fn the_projects_are_read_again_when_the_server_starts() {
    let s = server("store-restart");
    let boss = admin(&s).await;
    let (id, _) = make(&s, &boss, "pcb", "kept").await;
    let url = file_url("pcb", &id, "a.txt");
    assert_eq!(
        put(&s, &boss, &url, b"still here", None).await.status,
        StatusCode::CREATED
    );
    let again = eh3d_server::state::AppState::open(eh3d_server::config::ServerConfig {
        data_dir: s.dir.clone(),
        ..eh3d_server::config::ServerConfig::default()
    })
    .unwrap();
    let book = again.pstore.book.lock().unwrap();
    assert_eq!(book.list("pcb", &identity_admin(), "", 0, 10).0, 1);
}

fn identity_admin() -> eh3d_server::auth::Identity {
    eh3d_server::auth::Identity {
        name: "juan".into(),
        role: eh3d_server::auth::Role::Admin,
        is_token: false,
        token_id: None,
        scopes: None,
        template: None,
    }
}
