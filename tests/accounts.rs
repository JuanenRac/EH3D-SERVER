//! What a token can hand to others, and who owns what when a user is deleted and another gets the name: a token limited to scopes never gives more than it has, what a
//! token that expires makes does not outlive it, and a new user inherits nothing. The data of a server from before users had an identifier stays with its people.

mod common;

use std::path::{Path, PathBuf};

use axum::http::{Method, StatusCode};
use common::{add_user, admin, call, scoped_token, server, server_with, user, Server, PASSWORD};
use eh3d_server::auth::Auth;
use serde_json::{json, Value};

const OTHER_PASSWORD: &str = "otra-clave-larga-456";

const VIEWER_AND_USERS: [&str; 6] = [
    "admin:users",
    "projects:read",
    "catalog:read",
    "artifacts:read",
    "tasks:read",
    "machines:view",
];

fn refused(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::FORBIDDEN && body["error"] == "forbidden_scope"
}

async fn new_user(s: &Server, token: &str, name: &str, role: &str) -> (StatusCode, Value) {
    let body = json!({"name": name, "role": role, "password": PASSWORD});
    call(s, Method::POST, "/api/v1/users", Some(token), Some(body)).await
}

async fn change(s: &Server, token: &str, name: &str, body: Value) -> (StatusCode, Value) {
    let path = format!("/api/v1/users/{name}");
    call(s, Method::PUT, &path, Some(token), Some(body)).await
}

async fn logs_in(s: &Server, name: &str, password: &str) -> bool {
    let body = json!({"name": name, "password": password});
    let (status, _) = call(s, Method::POST, "/api/v1/login", None, Some(body)).await;
    status == StatusCode::OK
}

#[tokio::test]
async fn a_limited_token_never_touches_an_administrator_nor_makes_one() {
    let s = server("accounts-admins");
    let boss = admin(&s).await;
    add_user(&s, &boss, "second", "admin").await;
    let limited = scoped_token(&s, &boss, "admin", &VIEWER_AND_USERS).await;
    let (status, body) = new_user(&s, &limited, "third", "admin").await;
    assert!(refused(status, &body), "{body}");
    for name in ["juan", "second"] {
        for wanted in [
            json!({"password": OTHER_PASSWORD}),
            json!({"role": "viewer"}),
            json!({"disabled": true}),
            json!({}),
        ] {
            let (status, body) = change(&s, &limited, name, wanted.clone()).await;
            assert!(refused(status, &body), "{name} {wanted}: {body}");
        }
        let path = format!("/api/v1/users/{name}");
        let (status, body) = call(&s, Method::DELETE, &path, Some(&limited), None).await;
        assert!(refused(status, &body), "{name}: {body}");
    }
    assert!(logs_in(&s, "second", PASSWORD).await);
    assert!(!logs_in(&s, "third", PASSWORD).await);
    // a person still does all of it
    let (status, body) = change(&s, &boss, "second", json!({"password": OTHER_PASSWORD})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_limited_token_gives_only_roles_it_covers() {
    let s = server("accounts-roles");
    let boss = admin(&s).await;
    add_user(&s, &boss, "oper1", "operator").await;
    add_user(&s, &boss, "view1", "viewer").await;
    // the scope of the users alone covers no role at all
    let bare = scoped_token(&s, &boss, "admin", &["admin:users"]).await;
    let (status, body) = new_user(&s, &bare, "nina", "viewer").await;
    assert!(refused(status, &body), "{body}");
    let limited = scoped_token(&s, &boss, "admin", &VIEWER_AND_USERS).await;
    let (status, body) = new_user(&s, &limited, "nina", "operator").await;
    assert!(refused(status, &body), "{body}");
    let (status, body) = new_user(&s, &limited, "nina", "viewer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (name, wanted) in [
        ("view1", json!({"role": "operator"})),
        ("view1", json!({"role": "admin"})),
        ("oper1", json!({"password": OTHER_PASSWORD})),
        ("oper1", json!({"disabled": false})),
    ] {
        let (status, body) = change(&s, &limited, name, wanted.clone()).await;
        assert!(refused(status, &body), "{name} {wanted}: {body}");
    }
    assert!(logs_in(&s, "oper1", PASSWORD).await);
    // what only takes away, and what stays inside the token, is done
    for (name, wanted) in [
        ("view1", json!({"password": OTHER_PASSWORD})),
        ("oper1", json!({"disabled": true})),
        ("oper1", json!({"role": "viewer", "disabled": false})),
    ] {
        let (status, body) = change(&s, &limited, name, wanted.clone()).await;
        assert_eq!(status, StatusCode::OK, "{name} {wanted}: {body}");
    }
    let (status, body) = call(
        &s,
        Method::DELETE,
        "/api/v1/users/oper1",
        Some(&limited),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn make_token(s: &Server, token: &str, body: Value) -> (String, String) {
    let (status, answer) = call(s, Method::POST, "/api/v1/tokens", Some(token), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    (
        answer["id"].as_str().unwrap().to_string(),
        answer["token"].as_str().unwrap().to_string(),
    )
}

async fn expiry(s: &Server, boss: &str, id: &str) -> Option<u64> {
    let (status, list) = call(s, Method::GET, "/api/v1/tokens", Some(boss), None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list.as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap()["expires"]
        .as_u64()
}

#[tokio::test]
async fn what_a_token_that_expires_makes_does_not_outlive_it() {
    let s = server("accounts-expiry");
    let boss = admin(&s).await;
    let (forever_id, _) = make_token(&s, &boss, json!({"name": "forever", "role": "admin"})).await;
    let (parent_id, parent) = make_token(
        &s,
        &boss,
        json!({"name": "parent", "role": "admin", "days": 5}),
    )
    .await;
    let end = expiry(&s, &boss, &parent_id).await.unwrap();
    let (none, _) = make_token(&s, &parent, json!({"name": "none", "role": "admin"})).await;
    let (long, _) = make_token(
        &s,
        &parent,
        json!({"name": "long", "role": "admin", "days": 3650}),
    )
    .await;
    let (short, _) = make_token(
        &s,
        &parent,
        json!({"name": "short", "role": "admin", "days": 1}),
    )
    .await;
    assert_eq!(expiry(&s, &boss, &none).await, Some(end));
    assert_eq!(expiry(&s, &boss, &long).await, Some(end));
    assert!(expiry(&s, &boss, &short).await.unwrap() < end);
    // nor is the secret of a longer token taken by rotating it
    let rotate = |id: &str| format!("/api/v1/tokens/{id}/rotate");
    let (status, body) = call(&s, Method::POST, &rotate(&forever_id), Some(&parent), None).await;
    assert!(refused(status, &body), "{body}");
    assert!(body.get("token").is_none());
    let (status, body) = call(&s, Method::POST, &rotate(&short), Some(&parent), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // a person, whose login also ends, makes tokens that last as asked
    assert_eq!(expiry(&s, &boss, &forever_id).await, None);
}

#[tokio::test]
async fn the_key_of_a_machine_is_not_made_with_more_than_the_caller_has() {
    let s = server("accounts-compat");
    let boss = admin(&s).await;
    let machine =
        json!({"id": "p1", "name": "p1", "plugin": "printer3d", "settings": {"compat": "both"}});
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(machine),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = "/api/v1/work/machines/p1/compat/key";
    let manage = scoped_token(&s, &boss, "admin", &["machines:manage"]).await;
    let (status, body) = call(&s, Method::POST, path, Some(&manage), None).await;
    assert!(refused(status, &body), "{body}");
    let view = scoped_token(&s, &boss, "admin", &["machines:manage", "machines:view"]).await;
    let write = json!({"write": true});
    let (status, body) = call(&s, Method::POST, path, Some(&view), Some(write)).await;
    assert!(refused(status, &body), "{body}");
    let (status, body) = call(&s, Method::POST, path, Some(&view), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, short) = make_token(
        &s,
        &boss,
        json!({"name": "short", "role": "admin", "days": 1}),
    )
    .await;
    let (status, body) = call(&s, Method::POST, path, Some(&short), None).await;
    assert!(refused(status, &body), "{body}");
}

/// A project of each store made by `token`: its id in the projects with revisions and its id in the store of the applications.
async fn two_projects(s: &Server, token: &str, name: &str) -> (String, String) {
    let mut ids = Vec::new();
    for path in ["/api/v1/projects", "/api/v1/apps/studio/projects"] {
        let body = json!({"name": name});
        let (status, answer) = call(s, Method::POST, path, Some(token), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{path}: {answer}");
        assert!(!answer["owner"].as_str().unwrap().contains('#'), "{answer}");
        ids.push(answer["id"].as_str().unwrap().to_string());
    }
    (ids[0].clone(), ids[1].clone())
}

async fn status_of(s: &Server, token: &str, path: &str) -> StatusCode {
    call(s, Method::GET, path, Some(token), None).await.0
}

#[tokio::test]
async fn a_new_user_with_the_name_of_a_deleted_one_inherits_nothing() {
    let s = server("accounts-inherit");
    let boss = admin(&s).await;
    let old = user(&s, &boss, "ana", "operator").await;
    let (project, stored) = two_projects(&s, &old, "mine").await;
    let (shared, _) = two_projects(&s, &boss, "theirs").await;
    let shares = format!("/api/v1/projects/{shared}/shares");
    let share = json!({"user": "ana", "access": "read"});
    let (status, body) = call(&s, Method::POST, &shares, Some(&boss), Some(share.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["shares"], json!([{"who": "ana", "access": "read"}]));
    let paths = [
        format!("/api/v1/projects/{project}"),
        format!("/api/v1/apps/studio/projects/{stored}"),
        format!("/api/v1/projects/{shared}"),
    ];
    for path in &paths {
        assert_eq!(status_of(&s, &old, path).await, StatusCode::OK, "{path}");
    }

    let (status, body) = call(&s, Method::DELETE, "/api/v1/users/ana", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new = user(&s, &boss, "ana", "operator").await;
    for path in &paths {
        assert_eq!(
            status_of(&s, &new, path).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
        // what the deleted one made stays for the administrator, under the name it had
        assert_eq!(status_of(&s, &boss, path).await, StatusCode::OK, "{path}");
    }
    let (_, list) = call(&s, Method::GET, "/api/v1/projects", Some(&new), None).await;
    assert_eq!(list, json!([]));
    let (_, kept) = call(&s, Method::GET, &paths[0], Some(&boss), None).await;
    assert_eq!(kept["owner"], "ana");

    // shared again, the new one sees it and the share of the old one is gone
    let (status, body) = call(&s, Method::POST, &shares, Some(&boss), Some(share)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["shares"], json!([{"who": "ana", "access": "read"}]));
    assert_eq!(status_of(&s, &new, &paths[2]).await, StatusCode::OK);
    let one = format!("{shares}/ana");
    let (status, body) = call(&s, Method::DELETE, &one, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(status_of(&s, &new, &paths[2]).await, StatusCode::NOT_FOUND);
}

fn json_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            json_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// Rewrites every JSON file of a data folder as a server from before wrote it: no identifier of a user anywhere, the name where the owner is.
fn as_before(dir: &Path) {
    let users: Value =
        serde_json::from_slice(&std::fs::read(dir.join("users.json")).unwrap()).unwrap();
    let known: Vec<(String, String)> = users
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["uid"].as_str().unwrap().to_string(),
                u["name"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let mut files = Vec::new();
    json_files(dir, &mut files);
    for path in files {
        let mut text = std::fs::read_to_string(&path).unwrap();
        for (uid, name) in &known {
            assert_ne!(uid, name);
            text = text.replace(uid, name);
        }
        if let Ok(mut value) = serde_json::from_str::<Value>(&text) {
            drop_uid(&mut value);
            text = serde_json::to_string_pretty(&value).unwrap();
        }
        std::fs::write(&path, text).unwrap();
    }
}

fn drop_uid(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("uid");
            map.values_mut().for_each(drop_uid);
        }
        Value::Array(list) => list.iter_mut().for_each(drop_uid),
        _ => {}
    }
}

#[tokio::test]
async fn the_data_of_a_server_from_before_stays_with_its_people_and_nothing_is_rewritten() {
    let first = server("accounts-before");
    let boss = admin(&first).await;
    let ana = user(&first, &boss, "ana", "operator").await;
    let (project, stored) = two_projects(&first, &ana, "mine").await;
    let old_data = first.dir.with_extension("old");
    let _ = std::fs::remove_dir_all(&old_data);
    copy_tree(&first.dir, &old_data);
    as_before(&old_data);
    let users_before = std::fs::read(old_data.join("users.json")).unwrap();
    assert!(!String::from_utf8_lossy(&users_before).contains('#'));

    let data = old_data.clone();
    let s = server_with("accounts-after", move |c| c.data_dir = data);
    // opening wrote nothing: a cut at any moment of it leaves the old data
    assert_eq!(
        std::fs::read(old_data.join("users.json")).unwrap(),
        users_before
    );
    let body = json!({"name": "ana", "password": PASSWORD});
    let (status, answer) = call(&s, Method::POST, "/api/v1/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let ana = answer["token"].as_str().unwrap().to_string();
    let paths = [
        format!("/api/v1/projects/{project}"),
        format!("/api/v1/apps/studio/projects/{stored}"),
    ];
    for path in &paths {
        let (status, answer) = call(&s, Method::GET, path, Some(&ana), None).await;
        assert_eq!(status, StatusCode::OK, "{path}: {answer}");
        assert_eq!(answer["owner"], "ana");
    }
    // deleted and made again after the update, the name brings nothing with it, also after the files are read again
    let body = json!({"name": "juan", "password": "una-clave-larga"});
    let (_, answer) = call(&s, Method::POST, "/api/v1/login", None, Some(body)).await;
    let boss = answer["token"].as_str().unwrap().to_string();
    let (status, answer) = call(&s, Method::DELETE, "/api/v1/users/ana", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let new = user(&s, &boss, "ana", "operator").await;
    for path in &paths {
        assert_eq!(
            status_of(&s, &new, path).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let mut again = Auth::open(&old_data, 12).unwrap();
    let (token, _, _) = again
        .login("ana", PASSWORD, eh3d_server::jobs::now())
        .unwrap();
    let who = again.identify(&token, eh3d_server::jobs::now()).unwrap();
    assert_ne!(who.owner(), "ana");
    let juan = again.owner_key_of("juan").unwrap();
    assert_eq!(juan, "juan");
    let _ = std::fs::remove_dir_all(&old_data);
}

#[test]
fn a_saved_login_is_taken_up_by_the_same_user_and_not_by_a_new_one_of_the_name() {
    let dir = std::env::temp_dir().join(format!("eh3d-accounts-saved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let now = eh3d_server::jobs::now();
    let mut auth = Auth::open(&dir, 12).unwrap();
    let code = auth.setup_code().unwrap().to_string();
    auth.setup(&code, "juan", "una-clave-larga").unwrap();
    auth.add_user("ana", eh3d_server::auth::Role::Operator, PASSWORD)
        .unwrap();
    auth.set_persist_sessions(true);
    let (token, _, _) = auth.login("ana", PASSWORD, now).unwrap();
    let saved = std::fs::read(dir.join("sessions.json")).unwrap();
    // the same user takes it up
    let mut same = Auth::open(&dir, 12).unwrap();
    same.set_persist_sessions(true);
    assert!(same.identify(&token, now).is_some());
    // another user with the name does not, even if the file of the old logins comes back
    same.delete_user("ana").unwrap();
    same.add_user("ana", eh3d_server::auth::Role::Operator, PASSWORD)
        .unwrap();
    std::fs::write(dir.join("sessions.json"), &saved).unwrap();
    let mut other = Auth::open(&dir, 12).unwrap();
    other.set_persist_sessions(true);
    assert!(other.identify(&token, now).is_none());
    // a login saved by a server from before, with no identifier anywhere, is taken up after the update
    std::fs::write(dir.join("sessions.json"), &saved).unwrap();
    for name in ["users.json", "sessions.json"] {
        let path = dir.join(name);
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        drop_uid(&mut value);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let mut updated = Auth::open(&dir, 12).unwrap();
    updated.set_persist_sessions(true);
    assert_eq!(updated.identify(&token, now).unwrap().owner(), "ana");
    let _ = std::fs::remove_dir_all(&dir);
}
